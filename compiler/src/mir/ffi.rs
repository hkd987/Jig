//! C functions called from the interpreter. A symbol is found with `dlsym` among the
//! process's libraries and those `jig.mod` links (opened with `dlopen`), and called through
//! one signature that fills every argument register: integer-class arguments go in order to
//! the integer registers and `Float` arguments to the floating-point ones, which is how the
//! x86-64 System V and AArch64 conventions pass them to a non-variadic C function, whatever
//! its exact parameter list. The checker limits C functions to what fits in registers.

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::{Mutex, OnceLock};

extern "C" {
    fn dlopen(file: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

const RTLD_NOW: i32 = 2;
const RTLD_GLOBAL: i32 = 0x100;

/// An argument as the registers see it.
pub enum CArg {
    Int(i64),
    Float(f64),
    Str(CString),
}

/// What the result register holds.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CRet {
    Int,
    Float,
    Str,
    Void,
}

/// The result: the integer register, or the floating-point one, or a copied string.
pub enum CValue {
    Int(i64),
    Float(f64),
    Str(String),
    Void,
}

/// Open each library `jig.mod` links, once per process.
fn open_links(links: &[String]) {
    static OPENED: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    let opened = OPENED.get_or_init(|| Mutex::new(Vec::new()));
    let Ok(mut opened) = opened.lock() else { return };
    for l in links {
        if opened.contains(l) {
            continue;
        }
        opened.push(l.clone());
        for name in [format!("lib{l}.so"), format!("lib{l}.so.6"), format!("lib{l}.dylib")] {
            let Ok(c) = CString::new(name) else { continue };
            // SAFETY: dlopen with a valid NUL-terminated name; the handle is kept open for the
            // rest of the process (symbols are looked up globally).
            if !unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_GLOBAL) }.is_null() {
                break;
            }
        }
    }
}

/// The address of the C function `name`, if a loaded library defines it.
pub fn symbol(name: &str, links: &[String]) -> Option<usize> {
    open_links(links);
    let c = CString::new(name).ok()?;
    // SAFETY: RTLD_DEFAULT (a null handle) searches the process's global symbols.
    let p = unsafe { dlsym(std::ptr::null_mut(), c.as_ptr()) };
    (!p.is_null()).then_some(p as usize)
}

type IntFn = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, f64, f64, f64, f64, f64, f64, f64, f64) -> i64;
type FloatFn = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, f64, f64, f64, f64, f64, f64, f64, f64) -> f64;

/// Call the C function at `addr`.
///
/// # Safety
/// `addr` must be a non-variadic C function whose parameters, in order, are the integer-class
/// and `double` values of `args` (at most 6 and 8), returning what `ret` says.
pub unsafe fn call(addr: usize, args: &[CArg], ret: CRet) -> CValue {
    let mut ints = [0i64; 6];
    let mut floats = [0f64; 8];
    let (mut ni, mut nf) = (0, 0);
    for a in args {
        match a {
            CArg::Int(v) if ni < 6 => {
                ints[ni] = *v;
                ni += 1;
            }
            CArg::Str(s) if ni < 6 => {
                ints[ni] = s.as_ptr() as i64;
                ni += 1;
            }
            CArg::Float(v) if nf < 8 => {
                floats[nf] = *v;
                nf += 1;
            }
            _ => {}
        }
    }
    let [a, b, c, d, e, f] = ints;
    let [x0, x1, x2, x3, x4, x5, x6, x7] = floats;
    match ret {
        CRet::Float => {
            // SAFETY: see the function's contract; extra registers are ignored by the callee.
            let g: FloatFn = unsafe { std::mem::transmute::<usize, FloatFn>(addr) };
            CValue::Float(unsafe { g(a, b, c, d, e, f, x0, x1, x2, x3, x4, x5, x6, x7) })
        }
        _ => {
            // SAFETY: as above.
            let g: IntFn = unsafe { std::mem::transmute::<usize, IntFn>(addr) };
            let r = unsafe { g(a, b, c, d, e, f, x0, x1, x2, x3, x4, x5, x6, x7) };
            match ret {
                CRet::Void => CValue::Void,
                CRet::Str if r == 0 => CValue::Str(String::new()),
                // SAFETY: a C function returning `const char *` returns a NUL-terminated string.
                CRet::Str => CValue::Str(unsafe { CStr::from_ptr(r as *const c_char) }.to_string_lossy().into_owned()),
                _ => CValue::Int(r),
            }
        }
    }
}

/// A Jig string as C sees it: up to its first NUL byte.
pub fn c_string(s: &str) -> CString {
    let end = s.find('\0').unwrap_or(s.len());
    CString::new(&s[..end]).unwrap_or_default()
}
