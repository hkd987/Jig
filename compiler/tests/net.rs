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
