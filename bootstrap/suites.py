#!/usr/bin/env python3
"""Runs the test suites against jigc alone (D6): jigc is built from bootstrap/jigc.c with a C
compiler, then each suite whose expectations live in the repository is checked through
jigc's command line, as compiler/tests/ checks them through the bootstrap compiler:

  ui        compiler/tests/ui: the errors match the `//~ E0xxx` annotations
  run       compiler/tests/run: each program's output on the interpreter is its .out file
  native    the same programs built (debug and release) and run natively
  prove     compiler/tests/prove: each verbose report is its .out file (needs z3)
  props     compiler/tests/props: generated inputs, filtering, shrinking, determinism
  trace     the bank example's traceability questions
  docs      the cookbook's snippets check
  projects  every example and jigc itself check and pass their tests; the repository is
            formatted
  modules   jig get, jig.sum and replace against local git repositories (needs git)
  net       the greet and http examples over real sockets, natively and on the interpreter

Suites about the bootstrap compiler itself (checking on several threads, the differential
tests against jigc, its unit tests) have no counterpart. Needs no Rust.

usage: suites.py [--jigc PATH] [SUITE...]
"""
import http.client
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TESTS = os.path.join(ROOT, "compiler", "tests")


def build_jigc(out):
    cc = os.environ.get("CC", "cc")
    src = os.path.join(ROOT, "bootstrap", "jigc.c")
    subprocess.run([cc, "-O2", "-fno-math-errno", "-std=gnu11", "-w", "-pthread", src, "-lm", "-o", out], check=True)
    return out


class Runner:
    def __init__(self, jigc, work):
        self.jigc = jigc
        self.work = work

    def run(self, args, cwd=ROOT, stdin="", env=None, timeout=600):
        e = dict(os.environ)
        e["PWD"] = cwd
        e.update(env or {})
        p = subprocess.run(
            [self.jigc] + args, cwd=cwd, input=stdin.encode(), capture_output=True, env=e, timeout=timeout
        )
        return p.returncode, p.stdout.decode("utf-8", "replace"), p.stderr.decode("utf-8", "replace")

    def scratch(self, *parts):
        d = os.path.join(self.work, *parts)
        shutil.rmtree(d, ignore_errors=True)
        os.makedirs(d)
        return d


def lines(text):
    """Rust's `str::lines`."""
    out = text.split("\n")
    if out and out[-1] == "":
        out.pop()
    return [l[:-1] if l.endswith("\r") else l for l in out]


def sorted_entries(d):
    return sorted(os.listdir(d))


# ---------- ui ----------


def expected_in(src):
    out = []
    for i, line in enumerate(lines(src)):
        if line.lstrip().startswith("#~^"):
            pos = line.find("#~^")
            out += [(i, c) for c in line[pos + 3 :].split() if c.startswith("E")]
            continue
        pos = line.find("//~")
        if pos >= 0:
            out += [(i + 1, c) for c in line[pos + 3 :].split() if c.startswith("E")]
    return out


def diag_keys(stdout):
    """(file, line, code) of each diagnostic `jigc test --json` printed instead of testing."""
    keys = {}
    for l in lines(stdout):
        if not l.startswith("{"):
            continue
        d = json.loads(l)
        if "code" not in d:
            continue
        k = (d.get("file", "<nowhere>"), d["span"]["line"] if "span" in d else 0, d["code"])
        keys[k] = keys.get(k, 0) + 1
    return keys


def suite_ui(r):
    ui = os.path.join(TESTS, "ui")
    failures = []
    for name in sorted_entries(ui):
        path = os.path.join(ui, name)
        stem = name[: -len(".jig")] if name.endswith(".jig") else name
        case = r.scratch("ui", stem)
        expected = {}
        if os.path.isdir(path):
            shutil.copytree(path, case, dirs_exist_ok=True)
            for dirpath, dirs, files in os.walk(case):
                dirs.sort()
                for f in sorted(files):
                    if f.endswith((".jig", ".feature")):
                        p = os.path.join(dirpath, f)
                        rel = os.path.relpath(p, case)
                        for line, code in expected_in(open(p, encoding="utf-8").read()):
                            expected[(rel, line, code)] = expected.get((rel, line, code), 0) + 1
            start = "app" if os.path.isdir(os.path.join(case, "app")) else "."
        elif name.endswith(".jig"):
            src = open(path, encoding="utf-8").read()
            with open(os.path.join(case, "main.jig"), "w", encoding="utf-8") as f:
                f.write(src)
            for line, code in expected_in(src):
                expected[("main.jig", line, code)] = expected.get(("main.jig", line, code), 0) + 1
            start = "."
        else:
            continue
        _, out, _ = r.run(["test", "--json", start], cwd=case)
        actual = diag_keys(out)
        if actual != expected:
            missing = [k for k in expected if actual.get(k) != expected[k]]
            extra = [k for k in actual if expected.get(k) != actual[k]]
            failures.append("ui %s: missing %s, unexpected %s" % (stem, missing, extra))
    return failures


# ---------- run and native ----------


def run_cases():
    d = os.path.join(TESTS, "run")
    return [os.path.join(d, n) for n in sorted_entries(d) if n.endswith(".jig")]


def case_dir(r, kind, src):
    name = os.path.basename(src)[: -len(".jig")]
    d = r.scratch(kind, name)
    shutil.copy(src, os.path.join(d, "main.jig"))
    return name, d


def suite_run(r):
    failures = []
    for src in run_cases():
        name, d = case_dir(r, "run", src)
        _, out, err = r.run(["debug", "sim", d])
        want = open(src[: -len(".jig")] + ".out", encoding="utf-8").read()
        if out != want:
            failures.append("run %s: %r, expected %r %s" % (name, out, want, err))
    return failures


def markers(stderr, code):
    """A native run's panic or exit as the interpreter's `<panic: ...>` / `<exit N>`."""
    i = stderr.find("panic at ")
    if i >= 0:
        after = stderr[i + len("panic at ") :]
        msg = after.split(": ", 1)[1] if ": " in after else after
        return "<panic: %s>\n" % msg.rstrip()
    return "" if code == 0 else "<exit %d>\n" % code


def suite_native(r):
    failures = []
    for release in [False, True]:
        for src in run_cases():
            name, d = case_dir(r, "native-release" if release else "native-debug", src)
            exe = os.path.join(d, "prog")
            code, _, err = r.run(["build", d, "-o", exe] + (["--release"] if release else []))
            if code != 0:
                failures.append("native %s: build failed: %s" % (name, err))
                continue
            p = subprocess.run([exe], cwd=d, capture_output=True, timeout=120)
            actual = p.stdout.decode("utf-8", "replace") + markers(p.stderr.decode("utf-8", "replace"), p.returncode)
            base = src[: -len(".jig")]
            want_path = base + ".release.out" if release and os.path.exists(base + ".release.out") else base + ".out"
            want = open(want_path, encoding="utf-8").read() if os.path.exists(want_path) else ""
            if actual != want:
                failures.append(
                    "native %s (%s): %r, expected %r" % (name, "release" if release else "debug", actual, want)
                )
    return failures


# ---------- prove ----------


def suite_prove(r):
    if shutil.which(os.environ.get("JIG_Z3", "z3")) is None:
        print("prove: skipped (no z3)")
        return []
    failures = []
    compiler = os.path.join(ROOT, "compiler")
    prove = os.path.join(TESTS, "prove")
    for name in sorted_entries(prove):
        if not os.path.isdir(os.path.join(prove, name)):
            continue
        _, out, err = r.run(["prove", "tests/prove/" + name, "--verbose", "--no-cache"], cwd=compiler)
        got = []
        for l in lines(out):
            if l.startswith("proved ") and l.endswith(" s") and ", " in l:
                l = l[: l.rfind(", ")]
            got.append(l)
        actual = "\n".join(got) + "\n"
        want = open(os.path.join(prove, name + ".out"), encoding="utf-8").read()
        if actual != want:
            failures.append("prove %s:\n--- expected ---\n%s--- actual ---\n%s%s" % (name, want, actual, err))
    return failures


# ---------- props, trace, docs ----------


def test_results(out):
    return [json.loads(l) for l in lines(out) if l.startswith("{") and '"kind":"test"' in l]


def suite_props(r):
    failures = []
    _, out, _ = r.run(["test", "--json", "compiler/tests/props"])
    results = {t["name"]: t for t in test_results(out)}
    for ok in ["add commutes", "no requires", "lists", "reverse twice"]:
        if not results.get(ok, {}).get("passed"):
            failures.append("props: %s did not pass: %s" % (ok, results.get(ok)))
    clamp = results.get("clamp is at most 50", {})
    if clamp.get("passed") is not False or not clamp.get("message", "").endswith("with inputs: x = 51"):
        failures.append("props: clamp is at most 50: %s" % clamp)
    _, again, _ = r.run(["test", "--json", "compiler/tests/props"])
    if [t.get("message") for t in test_results(again)] != [t.get("message") for t in test_results(out)]:
        failures.append("props: two runs differ")
    return failures


def suite_trace(r):
    failures = []
    bank = "examples/bank"
    _, text, _ = r.run(["trace", bank, "fn", "transfer"])
    if "Payments: A payment moves money" not in text:
        failures.append("trace fn transfer: " + text)
    _, text, _ = r.run(["trace", bank, "uncovered"])
    if "TxError.str" not in text or "transfer" in text:
        failures.append("trace uncovered: " + text)
    _, text, _ = r.run(["trace", bank, "scenario", "overdraft"])
    _, j, _ = r.run(["trace", bank, "scenario", "overdraft", "--json"])
    if "then_refused" not in text or len(json.loads(j).get("dies_without_it", [])) != 1:
        failures.append("trace scenario overdraft: %s %s" % (text, j))
    return failures


def code_blocks(md):
    out, cur = [], None
    for line in lines(md):
        if line.lstrip().startswith("```"):
            if cur is None:
                cur = ""
            else:
                out.append(cur)
                cur = None
            continue
        if cur is not None:
            cur += line + "\n"
    return out


def suite_docs(r):
    md = open(os.path.join(ROOT, "plugin", "skills", "jig", "cookbook.md"), encoding="utf-8").read()
    blocks = code_blocks(md)
    imports, body = set(), ""
    for b in blocks:
        for line in lines(b):
            if line.startswith("import "):
                imports.add(line)
            else:
                body += line + "\n"
        body += "\n"
    d = r.scratch("cookbook")
    with open(os.path.join(d, "main.jig"), "w", encoding="utf-8") as f:
        f.write("\n".join(sorted(imports)) + "\n\n" + body)
    _, out, _ = r.run(["test", "--json", "."], cwd=d)
    # Snippets are fragments, so dead code is expected; nothing else is.
    errs = [k for k in diag_keys(out) if not k[2].startswith("E06")]
    failures = ["docs: cookbook snippets have errors: %s" % errs] if errs else []
    if len(blocks) <= 8:
        failures.append("docs: only %d cookbook blocks" % len(blocks))
    return failures


# ---------- projects ----------


def suite_projects(r):
    failures = []
    ex = os.path.join(ROOT, "examples")
    for d in [os.path.join(ex, n) for n in sorted_entries(ex)] + [os.path.join(ROOT, "selfhost")]:
        if not os.path.isdir(d):
            continue
        for cmd in ["check", "test"]:
            code, out, err = r.run([cmd, d])
            if code != 0:
                failures.append("%s %s: exit %d\n%s%s" % (cmd, os.path.relpath(d, ROOT), code, out, err))
    dirs = ["compiler/std", "compiler/tests/run", "compiler/tests/fixtures", "compiler/tests/prove"]
    dirs += ["compiler/tests/props", "examples", "selfhost"]
    code, out, err = r.run(["fmt", "--check"] + dirs)
    if code != 0:
        failures.append("fmt --check: %s%s" % (out, err))
    return failures


# ---------- modules ----------


def git(d, *args):
    subprocess.run(["git", "-C", d, "-c", "user.email=t@t", "-c", "user.name=t"] + list(args), check=True,
                   capture_output=True)


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)


def suite_modules(r):
    if shutil.which("git") is None:
        print("modules: skipped (no git)")
        return []
    w = r.scratch("modules")
    env = {"JIG_HOME": os.path.join(w, "home"), "JIG_GIT_BASE": "file://%s/remotes/" % w}
    lib = os.path.join(w, "remotes/example.com/greetlib")
    write(os.path.join(lib, "jig.mod"), "module example.com/greetlib\n")
    write(os.path.join(lib, "greet/greet.jig"),
          'import "util"\n\npub fn hello(name: Str) -> Str {\n  return "hello, " + util.shout(name)\n}\n\n'
          "pub fn unused_api() -> Int {\n  let x = 1\n  return 2\n}\n")
    write(os.path.join(lib, "util/util.jig"), 'pub fn shout(s: Str) -> Str {\n  return s.to_upper() + "!"\n}\n')
    git(lib, "init", "-q")
    git(lib, "add", "-A")
    git(lib, "commit", "-qm", "v1")
    git(lib, "tag", "v1.0.0")
    g = os.path.join(lib, "greet/greet.jig")
    write(g, open(g).read().replace("hello, ", "hi, "))
    git(lib, "commit", "-qam", "v1.1")
    git(lib, "tag", "v1.1.0")
    mid = os.path.join(w, "remotes/example.com/middle")
    write(os.path.join(mid, "jig.mod"), "module example.com/middle\nrequire example.com/greetlib v1.1.0\n")
    write(os.path.join(mid, "m/m.jig"),
          'import "example.com/greetlib/greet"\n\npub fn twice(s: Str) -> Str {\n'
          '  return greet.hello(s) + " " + greet.hello(s)\n}\n')
    git(mid, "init", "-q")
    git(mid, "add", "-A")
    git(mid, "commit", "-qm", "m")
    git(mid, "tag", "v0.1.0")
    app = os.path.join(w, "app")
    write(os.path.join(app, "jig.mod"), "module app\nrequire example.com/greetlib v1.0.0\n")
    write(os.path.join(app, "main.jig"),
          'import "example.com/greetlib/greet"\nimport "std/io"\n\nfn main() effects {io} {\n'
          '  io.println(greet.hello("jig"))\n}\n')
    failures = []

    def step(what, args, ok, check):
        code, out, err = r.run(args, env=env)
        text = out + err
        if (code == 0) != ok or not check(text):
            failures.append("modules %s: exit %d: %s" % (what, code, text))
        return text

    step("check before get", ["check", app], False, lambda t: "E0211" in t and "jig get" in t)
    step("get", ["get", app], True, lambda t: "downloaded example.com/greetlib v1.0.0" in t)
    if not open(os.path.join(app, "jig.sum")).read().startswith("example.com/greetlib v1.0.0 "):
        failures.append("modules: jig.sum")
    step("check after get", ["check", app], True, lambda t: True)
    step("run", ["run", app], True, lambda t: t == "hello, JIG!\n")
    write(os.path.join(app, "jig.mod"),
          "module app\nrequire example.com/greetlib v1.0.0\nrequire example.com/middle v0.1.0\n")
    write(os.path.join(app, "main.jig"),
          'import "example.com/greetlib/greet"\nimport "example.com/middle/m"\nimport "std/io"\n\n'
          'fn main() effects {io} {\n  io.println(greet.hello("a") + " / " + m.twice("b"))\n}\n')
    step("get raised", ["get", app], True,
         lambda t: "example.com/middle v0.1.0" in t and "example.com/greetlib v1.1.0" in t)
    step("run raised", ["run", app], True, lambda t: t == "hi, A! / hi, B! hi, B!\n")
    sum_path = os.path.join(app, "jig.sum")
    good = open(sum_path).read()
    write(sum_path, "\n".join(
        "example.com/greetlib v1.1.0 " + "0" * 40 if l.startswith("example.com/greetlib") else l
        for l in lines(good)))
    step("tampered", ["get", app], False, lambda t: "jig.sum recorded" in t)
    write(sum_path, good)
    local = os.path.join(w, "local-greetlib")
    write(os.path.join(local, "jig.mod"), "module example.com/greetlib\n")
    write(os.path.join(local, "greet/greet.jig"), 'pub fn hello(name: Str) -> Str {\n  return "local " + name\n}\n')
    write(os.path.join(app, "jig.mod"),
          "module app\nrequire example.com/greetlib v9.9.9\nreplace example.com/greetlib => ../local-greetlib\n")
    write(os.path.join(app, "main.jig"),
          'import "example.com/greetlib/greet"\nimport "std/io"\n\nfn main() effects {io} {\n'
          '  io.println(greet.hello("x"))\n}\n')
    step("replace", ["run", app], True, lambda t: t == "local x\n")
    return failures


# ---------- net ----------


def free_addr():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return "127.0.0.1:%d" % port


def wait_listening(addr):
    host, port = addr.split(":")
    start = time.time()
    while time.time() - start < 10:
        try:
            socket.create_connection((host, int(port)), timeout=1).close()
            return True
        except OSError:
            time.sleep(0.02)
    return False


def suite_net(r):
    failures = []
    d = r.scratch("net")
    shutil.copy(os.path.join(ROOT, "examples/greet/main.jig"), os.path.join(d, "main.jig"))
    exe = os.path.join(d, "greet")
    code, _, err = r.run(["build", d, "-o", exe])
    if code != 0:
        return ["net: build failed: " + err]
    addr = free_addr()
    server = subprocess.Popen([exe, "serve", addr, "2"], stdout=subprocess.PIPE)
    if not wait_listening(addr):
        server.kill()
        return ["net: the server never listened"]
    # That probe was an (empty) request too.
    out = subprocess.run([exe, "ask", addr, "ann"], capture_output=True).stdout.decode()
    if out != "hello ann #2\n":
        failures.append("net: native client got %r" % out)
    if server.communicate(timeout=30)[0].decode() != "greeted 2\n":
        failures.append("net: the native server did not finish")
    # The interpreter speaks the same protocol, as client and as server.
    addr = free_addr()
    server = subprocess.Popen([exe, "serve", addr, "1"], stdout=subprocess.PIPE)
    start, reply = time.time(), ""
    while not reply and time.time() - start < 10:
        reply = r.run(["run", d, "--", "ask", addr, "bob"])[1]
        if not reply:
            time.sleep(0.02)
    if reply != "hello bob #1\n":
        failures.append("net: interpreted client got %r" % reply)
    server.communicate(timeout=30)
    addr = free_addr()
    server = subprocess.Popen([r.jigc, "run", d, "--", "serve", addr, "1"], stdout=subprocess.PIPE, cwd=ROOT)
    start, reply = time.time(), ""
    while not reply and time.time() - start < 10:
        reply = subprocess.run([exe, "ask", addr, "cy"], capture_output=True).stdout.decode()
        if not reply:
            time.sleep(0.02)
    if reply != "hello cy #1\n":
        failures.append("net: interpreted server answered %r" % reply)
    if server.communicate(timeout=30)[0].decode() != "greeted 1\n":
        failures.append("net: the interpreted server did not finish")
    failures += net_http(r)
    return failures


def net_http(r):
    """The http example answers an HTTP client, which keeps its side open, natively and on
    the interpreter."""
    failures = []
    d = r.scratch("net-http")
    shutil.copy(os.path.join(ROOT, "examples/http/main.jig"), os.path.join(d, "main.jig"))
    exe = os.path.join(d, "http")
    code, _, err = r.run(["build", d, "-o", exe])
    if code != 0:
        return ["net: http build failed: " + err]
    for how, cmd in (("native", [exe]), ("interpreted", [r.jigc, "run", d, "--"])):
        addr = free_addr()
        # The probe that waits for it to listen is a request too, then two over HTTP.
        server = subprocess.Popen(cmd + [addr, "3"], stdout=subprocess.PIPE, cwd=ROOT)
        if not wait_listening(addr):
            server.kill()
            failures.append("net: the %s http server never listened" % how)
            continue
        host, port = addr.split(":")
        got = []
        for method, path, body in (("GET", "/", None), ("POST", "/missing", "some body")):
            c = http.client.HTTPConnection(host, int(port), timeout=10)
            try:
                c.request(method, path, body=body)
                resp = c.getresponse()
                got.append((resp.status, resp.read().decode()))
            except OSError as e:
                got.append(("error", str(e)))
            finally:
                c.close()
        if got != [(200, "Hello, world!\n"), (404, "not found\n")]:
            failures.append("net: the %s http server answered %r" % (how, got))
        out = server.communicate(timeout=30)[0].decode()
        if out != "listening on %s\nserved 3\n" % addr:
            failures.append("net: the %s http server printed %r" % (how, out))
    return failures


SUITES = {
    "ui": suite_ui,
    "run": suite_run,
    "native": suite_native,
    "prove": suite_prove,
    "props": suite_props,
    "trace": suite_trace,
    "docs": suite_docs,
    "projects": suite_projects,
    "modules": suite_modules,
    "net": suite_net,
}


def main():
    args = sys.argv[1:]
    jigc = None
    if args[:1] == ["--jigc"]:
        jigc, args = os.path.abspath(args[1]), args[2:]
    names = args or list(SUITES)
    with tempfile.TemporaryDirectory() as work:
        if jigc is None:
            jigc = build_jigc(os.path.join(work, "jigc"))
        r = Runner(jigc, work)
        failed = 0
        for n in names:
            start = time.time()
            fs = SUITES[n](r)
            print("%-9s %s (%.1f s)" % (n, "ok" if not fs else "%d failed" % len(fs), time.time() - start))
            for f in fs:
                print("  " + f.replace("\n", "\n  "))
            failed += len(fs)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
