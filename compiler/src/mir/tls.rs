//! `net.fetch` on the interpreter: one HTTP exchange over TCP or TLS, as the C runtime does it
//! (`jig_net_fetch` in `jig_rt.c`), with the same results and messages. TLS goes through the
//! system's OpenSSL, opened when a program first fetches over it; the server's certificate
//! must chain to a trusted root (the system's, or `SSL_CERT_FILE`'s) and name the host.

use std::ffi::{c_char, c_int, c_long, c_ulong, c_void, CStr, CString};
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::sync::OnceLock;
use std::time::Duration;

extern "C" {
    fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;
/// How long a read or write may wait.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The OpenSSL functions a fetch calls, by their ABI (the same in OpenSSL 1.1 and 3).
struct Api {
    client_method: unsafe extern "C" fn() -> *const c_void,
    ctx_new: unsafe extern "C" fn(*const c_void) -> *mut c_void,
    ctx_default_paths: unsafe extern "C" fn(*mut c_void) -> c_int,
    ctx_set_verify: unsafe extern "C" fn(*mut c_void, c_int, *const c_void),
    ctx_free: unsafe extern "C" fn(*mut c_void),
    ssl_new: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    set_fd: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    ctrl: unsafe extern "C" fn(*mut c_void, c_int, c_long, *const c_void) -> c_long,
    set1_host: unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int,
    connect: unsafe extern "C" fn(*mut c_void) -> c_int,
    write: unsafe extern "C" fn(*mut c_void, *const c_void, c_int) -> c_int,
    read: unsafe extern "C" fn(*mut c_void, *mut c_void, c_int) -> c_int,
    verify_result: unsafe extern "C" fn(*const c_void) -> c_long,
    ssl_free: unsafe extern "C" fn(*mut c_void),
    err_clear: unsafe extern "C" fn(),
    err_get: unsafe extern "C" fn() -> c_ulong,
    err_string: unsafe extern "C" fn(c_ulong, *mut c_char, usize),
    verify_string: unsafe extern "C" fn(c_long) -> *const c_char,
}

const LIBS: [&str; 6] = [
    "libssl.so.3",
    "libssl.so.1.1",
    "libssl.so",
    "libssl.3.dylib",
    "/opt/homebrew/opt/openssl@3/lib/libssl.3.dylib",
    "/usr/local/opt/openssl@3/lib/libssl.3.dylib",
];

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(load).as_ref()
}

fn load() -> Option<Api> {
    let h = LIBS.iter().find_map(|name| {
        let c = CString::new(*name).ok()?;
        // SAFETY: dlopen with a NUL-terminated name; the library stays open for the process.
        let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
        (!h.is_null()).then_some(h)
    })?;
    let sym = |name: &str| -> Option<*mut c_void> {
        let c = CString::new(name).ok()?;
        // SAFETY: dlsym on a handle dlopen returned, with a NUL-terminated name.
        let p = unsafe { dlsym(h, c.as_ptr()) };
        (!p.is_null()).then_some(p)
    };
    macro_rules! f {
        ($name:literal) => {
            // SAFETY: the symbol is the OpenSSL function of that name, whose C signature is
            // the field's type.
            unsafe { cast(sym($name)?) }
        };
    }
    Some(Api {
        client_method: f!("TLS_client_method"),
        ctx_new: f!("SSL_CTX_new"),
        ctx_default_paths: f!("SSL_CTX_set_default_verify_paths"),
        ctx_set_verify: f!("SSL_CTX_set_verify"),
        ctx_free: f!("SSL_CTX_free"),
        ssl_new: f!("SSL_new"),
        set_fd: f!("SSL_set_fd"),
        ctrl: f!("SSL_ctrl"),
        set1_host: f!("SSL_set1_host"),
        connect: f!("SSL_connect"),
        write: f!("SSL_write"),
        read: f!("SSL_read"),
        verify_result: f!("SSL_get_verify_result"),
        ssl_free: f!("SSL_free"),
        err_clear: f!("ERR_clear_error"),
        err_get: f!("ERR_get_error"),
        err_string: f!("ERR_error_string_n"),
        verify_string: f!("X509_verify_cert_error_string"),
    })
}

/// The function at `p` as the function pointer type `F`.
///
/// # Safety
/// `p` must be a C function whose signature `F` describes.
unsafe fn cast<F: Copy>(p: *mut c_void) -> F {
    assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*mut c_void>());
    // SAFETY: `F` is a function pointer, the size of `p`; the caller vouches for its type.
    unsafe { std::mem::transmute_copy::<*mut c_void, F>(&p) }
}

/// Why the TLS connection failed: the certificate check's reason, else OpenSSL's first error.
fn tls_err(a: &Api, what: &str, ssl: *mut c_void) -> String {
    // SAFETY: `ssl` is null or a live SSL object; the error functions take no pointers but
    // the buffer, whose length is passed.
    let detail = unsafe {
        let v = if ssl.is_null() { 0 } else { (a.verify_result)(ssl) };
        let e = (a.err_get)();
        if v != 0 {
            CStr::from_ptr((a.verify_string)(v)).to_string_lossy().into_owned()
        } else if e != 0 {
            let mut buf = [0 as c_char; 256];
            (a.err_string)(e, buf.as_mut_ptr(), buf.len());
            CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
        } else {
            String::new()
        }
    };
    if detail.is_empty() {
        what.to_string()
    } else {
        format!("{what}: {detail}")
    }
}

fn timed_out(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

/// Send `req` to `addr` ("host:port") and read the reply until the server closes; over TLS
/// when `tls` is set. Errors are in the C runtime's words, without the address.
pub fn fetch(addr: &str, req: &str, tls: bool) -> Result<String, String> {
    let api = if tls {
        Some(api().ok_or_else(|| "TLS needs OpenSSL (libssl), which was not found".to_string())?)
    } else {
        None
    };
    let target = if addr.starts_with(':') { format!("127.0.0.1{addr}") } else { addr.to_string() };
    let mut s = TcpStream::connect(target).map_err(|e| super::interp::os_error(&e))?;
    let _ = s.set_read_timeout(Some(TIMEOUT));
    let _ = s.set_write_timeout(Some(TIMEOUT));
    let mut out = Vec::new();
    let Some(a) = api else {
        s.write_all(req.as_bytes()).map_err(|e| super::interp::os_error(&e))?;
        if let Err(e) = s.read_to_end(&mut out) {
            if timed_out(&e) {
                return Err("timed out".to_string());
            }
        }
        return Ok(String::from_utf8_lossy(&out).into_owned());
    };
    let host = CString::new(&addr[..addr.rfind(':').unwrap_or(0)]).map_err(|_| "expected host:port".to_string())?;
    // SAFETY: each call follows OpenSSL's API: objects are used only while live and freed
    // once; buffers are passed with their lengths; the socket outlives the SSL object.
    unsafe {
        (a.err_clear)();
        let ctx = (a.ctx_new)((a.client_method)());
        // Before SSL_new, which copies the verification settings.
        if !ctx.is_null() {
            (a.ctx_default_paths)(ctx);
            (a.ctx_set_verify)(ctx, 1, std::ptr::null());
        }
        let ssl = if ctx.is_null() { std::ptr::null_mut() } else { (a.ssl_new)(ctx) };
        let mut fail = None;
        if ssl.is_null() {
            fail = Some(tls_err(a, "TLS setup failed", std::ptr::null_mut()));
        } else {
            (a.set_fd)(ssl, s.as_raw_fd());
            (a.ctrl)(ssl, 55, 0, host.as_ptr().cast());
            if (a.set1_host)(ssl, host.as_ptr()) != 1 || (a.connect)(ssl) != 1 {
                fail = Some(tls_err(a, "TLS handshake failed", ssl));
            }
        }
        let mut rest = req.as_bytes();
        while fail.is_none() && !rest.is_empty() {
            let n = rest.len().min(1 << 30);
            let k = (a.write)(ssl, rest.as_ptr().cast(), n as c_int);
            if k <= 0 {
                fail = Some(tls_err(a, "TLS write failed", std::ptr::null_mut()));
            } else {
                rest = &rest[k as usize..];
            }
        }
        let mut buf = vec![0u8; 1 << 14];
        while fail.is_none() {
            set_errno(0);
            let k = (a.read)(ssl, buf.as_mut_ptr().cast(), buf.len() as c_int);
            if k <= 0 {
                if timed_out(&std::io::Error::last_os_error()) {
                    fail = Some("timed out".to_string());
                }
                break;
            }
            out.extend_from_slice(&buf[..k as usize]);
        }
        if !ssl.is_null() {
            (a.ssl_free)(ssl);
        }
        if !ctx.is_null() {
            (a.ctx_free)(ctx);
        }
        drop(s);
        match fail {
            Some(e) => Err(e),
            None => Ok(String::from_utf8_lossy(&out).into_owned()),
        }
    }
}

/// Clear `errno`, so that a read that ends without an error is not taken for a timeout.
fn set_errno(v: c_int) {
    extern "C" {
        #[cfg_attr(target_os = "linux", link_name = "__errno_location")]
        #[cfg_attr(target_os = "macos", link_name = "__error")]
        fn errno_location() -> *mut c_int;
    }
    // SAFETY: the C library's per-thread errno.
    unsafe { *errno_location() = v };
}
