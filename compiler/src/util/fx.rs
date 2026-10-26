//! Fast non-cryptographic hashing (FxHash, as used by rustc) and a stable 128-bit content
//! hash for cache keys.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

#[derive(Default, Clone, Copy)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.add(u64::from_le_bytes(c.try_into().unwrap()));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(buf));
        }
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(i as u64);
    }
    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add(i as u64);
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(i as u64);
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

pub type FxBuildHasher = BuildHasherDefault<FxHasher>;
pub type FxHashMap<K, V> = HashMap<K, V, FxBuildHasher>;
pub type FxHashSet<K> = HashSet<K, FxBuildHasher>;

pub fn fx_map<K, V>() -> FxHashMap<K, V> {
    FxHashMap::default()
}

pub fn fx_set<K>() -> FxHashSet<K> {
    FxHashSet::default()
}

/// A stable, platform-independent 128-bit hash for content-addressed caches. Two independent
/// 64-bit lanes with strong final mixing; not cryptographic, but collision-safe for
/// non-adversarial inputs.
#[derive(Clone)]
pub struct StableHasher {
    a: u64,
    b: u64,
    len: u64,
}

impl Default for StableHasher {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn mix(x: u64) -> u64 {
    // splitmix64 finalizer
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl StableHasher {
    pub fn new() -> Self {
        StableHasher { a: 0x243f_6a88_85a3_08d3, b: 0x1319_8a2e_0370_7344, len: 0 }
    }

    #[inline]
    fn word(&mut self, w: u64) {
        self.a = mix(self.a ^ w).wrapping_add(0x9e37_79b9_7f4a_7c15);
        self.b = mix(self.b.rotate_left(29) ^ w.wrapping_mul(0xff51_afd7_ed55_8ccd));
    }

    pub fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.len = self.len.wrapping_add(bytes.len() as u64);
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.word(u64::from_le_bytes(c.try_into().unwrap()));
        }
        let rest = chunks.remainder();
        let mut buf = [0u8; 8];
        buf[..rest.len()].copy_from_slice(rest);
        buf[7] ^= rest.len() as u8;
        self.word(u64::from_le_bytes(buf));
        self
    }

    pub fn str(&mut self, s: &str) -> &mut Self {
        self.bytes(s.as_bytes())
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.word(v);
        self
    }

    pub fn finish(&self) -> u128 {
        let a = mix(self.a ^ self.len);
        let b = mix(self.b ^ a.rotate_left(17));
        ((a as u128) << 64) | b as u128
    }
}

pub fn hash_hex(h: u128) -> String {
    format!("{h:032x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_hash_distinguishes_inputs() {
        let h1 = StableHasher::new().str("abc").finish();
        let h2 = StableHasher::new().str("abd").finish();
        let h3 = StableHasher::new().str("abc").finish();
        assert_ne!(h1, h2);
        assert_eq!(h1, h3);
        // length is part of the hash
        let h4 = StableHasher::new().bytes(&[0]).finish();
        let h5 = StableHasher::new().bytes(&[0, 0]).finish();
        assert_ne!(h4, h5);
    }
}
