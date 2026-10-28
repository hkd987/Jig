//! `std/net` over real sockets: the greet example serves natively and answers a native
//! client and an interpreted one.

use jig::codegen::build::{build, BuildOpts};
use jig::sema::load::Sources;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn have_cc() -> bool {
    let cc = jig::codegen::build::c_compiler(false);
    Command::new(cc).arg("--version").output().is_ok_and(|o| o.status.success())
}

#[test]
fn greet_over_tcp() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/greet");
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("net-greet");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(src.join("main.jig"), work.join("main.jig")).unwrap();
    let mut prog = jig::sema::analyze(&Sources::Disk, &work);
    assert!(!prog.diags.has_errors());
    let exe = work.join("greet");
    let opts =
        BuildOpts { release: false, build_dir: work.join("build"), out: exe.clone(), proven: Default::default() };
    build(&mut prog, &opts).expect("build");

    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let server = Command::new(&exe).args(["serve", &addr, "2"]).stdout(Stdio::piped()).spawn().unwrap();
    let start = Instant::now();
    while std::net::TcpStream::connect(&addr).is_err() {
        assert!(start.elapsed() < Duration::from_secs(10), "the server never listened");
        std::thread::sleep(Duration::from_millis(20));
    }
    // That probe was a (empty) request too.
    let native = Command::new(&exe).args(["ask", &addr, "ann"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&native.stdout), "hello ann #2\n");
    let out = server.wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "greeted 2\n");

    // The interpreter speaks the same protocol.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let server = Command::new(&exe).args(["serve", &addr, "1"]).stdout(Stdio::piped()).spawn().unwrap();
    let start = Instant::now();
    let jig = env!("CARGO_BIN_EXE_jig");
    let reply = loop {
        let o = Command::new(jig).arg("run").arg(&work).args(["--", "ask", &addr, "bob"]).output().unwrap();
        let s = String::from_utf8_lossy(&o.stdout).to_string();
        if !s.is_empty() {
            break s;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "no reply: {}", String::from_utf8_lossy(&o.stderr));
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(reply, "hello bob #1\n");
    assert_eq!(String::from_utf8_lossy(&server.wait_with_output().unwrap().stdout), "greeted 1\n");
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn have(cmd: &str) -> bool {
    Command::new(cmd).arg("version").output().is_ok_and(|o| o.status.success())
}

/// The fetch example over HTTP and HTTPS against `https_server.py`, natively and on the
/// interpreter: certificates are checked (an unknown root and a name the certificate does not
/// have are refused), redirects followed and chunked replies joined.
#[test]
fn fetch_over_tls() {
    if !have_cc() || !have("openssl") || !Command::new("python3").arg("-V").output().is_ok_and(|o| o.status.success()) {
        eprintln!("skipping: needs a C compiler, openssl and python3");
        return;
    }
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("net-fetch");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(work.join("certs")).unwrap();
    std::fs::copy(tests.join("../../examples/fetch/main.jig"), work.join("main.jig")).unwrap();
    let mut prog = jig::sema::analyze(&Sources::Disk, &work);
    assert!(!prog.diags.has_errors());
    let exe = work.join("fetch");
    let opts =
        BuildOpts { release: false, build_dir: work.join("build"), out: exe.clone(), proven: Default::default() };
    build(&mut prog, &opts).expect("build");

    let (tls, plain) = (free_port(), free_port());
    let mut server = Command::new("python3")
        .arg(tests.join("https_server.py"))
        .arg(work.join("certs"))
        .args([tls.to_string(), plain.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(server.stdout.take().unwrap()), &mut ready).unwrap();
    assert_eq!(ready, "ready\n");
    let ca = work.join("certs/ca.pem");
    let jig = env!("CARGO_BIN_EXE_jig");
    let cases: Vec<(Vec<String>, bool, i32, String)> = vec![
        (vec![format!("https://localhost:{tls}/hello")], true, 0, format!("200\npath=/hello host=localhost:{tls}\n")),
        (vec![format!("https://localhost:{tls}/old")], true, 0, format!("200\npath=/hello?x=1 host=localhost:{tls}\n")),
        (
            vec![format!("https://localhost:{tls}/echo"), "text/plain".into(), "ping".into()],
            true,
            0,
            "201\ngot ping!\n".into(),
        ),
        (
            vec![format!("http://localhost:{plain}/hello")],
            false,
            0,
            format!("200\npath=/hello host=localhost:{plain}\n"),
        ),
        (
            vec![format!("https://localhost:{tls}/hello")],
            false,
            1,
            format!("fetch: localhost:{tls}: TLS handshake failed: unable to get local issuer certificate\n"),
        ),
        (
            vec![format!("https://127.0.0.1:{tls}/hello")],
            true,
            1,
            format!("fetch: 127.0.0.1:{tls}: TLS handshake failed: IP address mismatch\n"),
        ),
    ];
    for (args, trust, code, want) in &cases {
        for native in [true, false] {
            let mut cmd = if native {
                Command::new(&exe)
            } else {
                let mut c = Command::new(jig);
                c.arg("run").arg(&work).arg("--");
                c
            };
            cmd.args(args).env_remove("SSL_CERT_FILE").env_remove("SSL_CERT_DIR");
            if *trust {
                cmd.env("SSL_CERT_FILE", &ca);
            }
            let o = cmd.output().unwrap();
            let got = String::from_utf8_lossy(if *code == 0 { &o.stdout } else { &o.stderr }).to_string();
            let how = if native { "native" } else { "interpreted" };
            assert_eq!((o.status.code(), got.as_str()), (Some(*code), want.as_str()), "{how} fetch {args:?}");
        }
    }
    let _ = server.kill();
    let _ = server.wait();
}

/// One HTTP GET over a fresh connection, the whole reply.
fn http_get(addr: &str, path: &str) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

/// The api example natively with `net.serve_http_parallel`: eight requests that each wait
/// 400 ms are answered at once, well under the 3.2 s one at a time would take; the
/// interpreter answers the same requests, one at a time.
#[test]
fn parallel_serving() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/api");
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("net-api");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(src.join("main.jig"), work.join("main.jig")).unwrap();
    let mut prog = jig::sema::analyze(&Sources::Disk, &work);
    assert!(!prog.diags.has_errors());
    let exe = work.join("api");
    let opts =
        BuildOpts { release: false, build_dir: work.join("build"), out: exe.clone(), proven: Default::default() };
    build(&mut prog, &opts).expect("build");
    let jig = env!("CARGO_BIN_EXE_jig");
    for native in [true, false] {
        let addr = format!("127.0.0.1:{}", free_port());
        let n = if native { 8 } else { 2 };
        let mut cmd = if native {
            Command::new(&exe)
        } else {
            let mut c = Command::new(jig);
            c.arg("run").arg(&work).arg("--");
            c
        };
        // One more request: the probe that waits for it to listen.
        let server = cmd
            .args([addr.clone(), (n + 1).to_string()])
            .env("JIG_THREADS", "8")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while std::net::TcpStream::connect(&addr).is_err() {
            assert!(start.elapsed() < Duration::from_secs(20), "the server never listened");
            std::thread::sleep(Duration::from_millis(20));
        }
        let start = Instant::now();
        let replies: Vec<String> = (0..n)
            .map(|_| {
                let a = addr.clone();
                std::thread::spawn(move || http_get(&a, "/wait?ms=400"))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect();
        let took = start.elapsed();
        for r in &replies {
            assert!(r.starts_with("HTTP/1.1 200 OK\r\n") && r.ends_with("{\"waited\":400}\n"), "{r}");
        }
        if native {
            assert!(took < Duration::from_millis(2400), "eight waits took {took:?}: not answered at once");
        } else {
            assert!(took >= Duration::from_millis(800), "the interpreter answers one at a time");
        }
        let out = server.wait_with_output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), format!("listening on {addr}\nanswered {}\n", n + 1));
    }
}
