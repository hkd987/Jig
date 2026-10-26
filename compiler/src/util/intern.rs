//! Global string interner. Symbols are 4-byte handles; their strings live for the process.

use super::fx::FxHashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(pub u32);

struct Interner {
    map: FxHashMap<&'static str, Symbol>,
    count: u32,
    chunk: String,
}

/// Lock-free read side: symbol `n` lives in segment `floor(log2(n + 1))`, which holds
/// `2^seg` entries. Segments are allocated once and never move.
static SEGMENTS: [OnceLock<Box<[OnceLock<&'static str>]>>; 32] = [const { OnceLock::new() }; 32];

#[inline]
fn slot(n: u32) -> (usize, usize) {
    let k = n as u64 + 1;
    let seg = 63 - k.leading_zeros() as usize;
    (seg, (k - (1u64 << seg)) as usize)
}

impl Interner {
    fn alloc(&mut self, s: &str) -> &'static str {
        if self.chunk.capacity() - self.chunk.len() < s.len() {
            let cap = (64 * 1024).max(s.len());
            // The old chunk is leaked on purpose: its strings are referenced for the process.
            let old = std::mem::replace(&mut self.chunk, String::with_capacity(cap));
            std::mem::forget(old);
        }
        let start = self.chunk.len();
        self.chunk.push_str(s);
        // SAFETY: the chunk never reallocates (we checked capacity) and is never freed.
        unsafe {
            let ptr = self.chunk.as_ptr().add(start);
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(ptr, s.len()))
        }
    }

    fn intern(&mut self, s: &str) -> Symbol {
        if let Some(&sym) = self.map.get(s) {
            return sym;
        }
        let stored = self.alloc(s);
        let sym = Symbol(self.count);
        self.count += 1;
        let (seg, off) = slot(sym.0);
        let segment = SEGMENTS[seg].get_or_init(|| (0..1usize << seg).map(|_| OnceLock::new()).collect());
        let _ = segment[off].set(stored);
        self.map.insert(stored, sym);
        sym
    }
}

fn interner() -> &'static Mutex<Interner> {
    static I: OnceLock<Mutex<Interner>> = OnceLock::new();
    I.get_or_init(|| {
        let mut i = Interner {
            map: FxHashMap::default(),
            count: 0,
            chunk: String::with_capacity(64 * 1024),
        };
        for s in sym::PREDEFINED {
            i.intern(s);
        }
        Mutex::new(i)
    })
}

impl Symbol {
    pub fn intern(s: &str) -> Symbol {
        interner().lock().unwrap().intern(s)
    }
    pub fn as_str(self) -> &'static str {
        let (seg, off) = slot(self.0);
        if let Some(segment) = SEGMENTS[seg].get() {
            if let Some(s) = segment[off].get() {
                return s;
            }
        }
        // Not yet visible (or the interner is uninitialized): initialize and retry.
        let _ = interner();
        SEGMENTS[seg].get().and_then(|sg| sg[off].get()).copied().unwrap_or("<invalid symbol>")
    }
}

impl std::fmt::Debug for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

impl std::fmt::Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A per-thread cache in front of the global interner, so hot paths (the lexer and parser)
/// take the lock only for strings they have not seen yet.
#[derive(Default)]
pub struct LocalInterner {
    cache: FxHashMap<Box<str>, Symbol>,
}

impl LocalInterner {
    pub fn intern(&mut self, s: &str) -> Symbol {
        if let Some(&sym) = self.cache.get(s) {
            return sym;
        }
        let sym = Symbol::intern(s);
        self.cache.insert(s.into(), sym);
        sym
    }
}

macro_rules! predefined {
    ($($name:ident = $s:expr),* $(,)?) => {
        pub mod sym {
            use super::Symbol;
            pub const PREDEFINED: &[&str] = &[$($s),*];
            predefined!(@consts 0u32, $($name)*);
        }
    };
    (@consts $n:expr, $name:ident $($rest:ident)*) => {
        pub const $name: Symbol = Symbol($n);
        predefined!(@consts $n + 1u32, $($rest)*);
    };
    (@consts $n:expr,) => {};
}

predefined! {
    EMPTY = "",
    UNDERSCORE = "_",
    INT = "Int",
    I32 = "I32",
    U8 = "U8",
    U32 = "U32",
    U64 = "U64",
    FLOAT = "Float",
    BOOL = "Bool",
    CHAR = "Char",
    STR = "Str",
    LIST = "List",
    MAP = "Map",
    SET = "Set",
    OPTION = "Option",
    RESULT_TY = "Result",
    NEVER = "Never",
    SOME = "Some",
    NONE = "None",
    OK = "Ok",
    ERR = "Err",
    EQ = "Eq",
    ORD = "Ord",
    HASH = "Hash",
    SHOW = "Show",
    MAIN = "main",
    SELF_LOWER = "self",
    RESULT = "result",
    OLD = "old",
    FORALL = "forall",
    EXISTS = "exists",
    STD = "std",
    IO = "io",
    FS = "fs",
    NET = "net",
    CLOCK = "clock",
    RAND = "rand",
    ENV = "env",
    SPAWN = "spawn",
    FFI = "ffi",
    NEW = "new",
    STR_METHOD = "str",
    LEN = "len",
    ASSERT = "assert",
    PANIC = "panic",
    MIN = "min",
    MAX = "max",
    WORLD = "World",
    VALUE = "value",
    ERROR = "error",
    TODO = "todo",
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_is_stable() {
        let a = Symbol::intern("hello");
        let b = Symbol::intern("hello");
        assert_eq!(a, b);
        assert_eq!(a.as_str(), "hello");
        assert_eq!(sym::INT.as_str(), "Int");
        assert_eq!(Symbol::intern("Int"), sym::INT);
        assert_eq!(sym::TODO.as_str(), "todo");
        let mut l = LocalInterner::default();
        assert_eq!(l.intern("hello"), a);
    }
}
