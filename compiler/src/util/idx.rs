//! Typed indices and index-addressed vectors, the backbone of the flat-arena data layout.

use std::marker::PhantomData;
use std::ops::{Index, IndexMut};

pub trait Idx: Copy + Eq + std::fmt::Debug {
    fn new(i: usize) -> Self;
    fn index(self) -> usize;
}

#[macro_export]
macro_rules! define_idx {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
        pub struct $name(pub u32);
        impl $crate::util::idx::Idx for $name {
            #[inline]
            fn new(i: usize) -> Self {
                debug_assert!(i <= u32::MAX as usize);
                $name(i as u32)
            }
            #[inline]
            fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

#[derive(Clone, Debug)]
pub struct IndexVec<I: Idx, T> {
    pub raw: Vec<T>,
    _m: PhantomData<fn(I)>,
}

impl<I: Idx, T> Default for IndexVec<I, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: Idx, T> IndexVec<I, T> {
    pub fn new() -> Self {
        IndexVec { raw: Vec::new(), _m: PhantomData }
    }
    pub fn with_capacity(n: usize) -> Self {
        IndexVec { raw: Vec::with_capacity(n), _m: PhantomData }
    }
    pub fn from_raw(raw: Vec<T>) -> Self {
        IndexVec { raw, _m: PhantomData }
    }
    #[inline]
    pub fn push(&mut self, v: T) -> I {
        let i = I::new(self.raw.len());
        self.raw.push(v);
        i
    }
    #[inline]
    pub fn len(&self) -> usize {
        self.raw.len()
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }
    pub fn next_idx(&self) -> I {
        I::new(self.raw.len())
    }
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.raw.iter()
    }
    pub fn iter_enumerated(&self) -> impl Iterator<Item = (I, &T)> {
        self.raw.iter().enumerate().map(|(i, v)| (I::new(i), v))
    }
    pub fn indices(&self) -> impl Iterator<Item = I> {
        (0..self.raw.len()).map(I::new)
    }
    pub fn get(&self, i: I) -> Option<&T> {
        self.raw.get(i.index())
    }
    pub fn resize(&mut self, n: usize, v: T)
    where
        T: Clone,
    {
        self.raw.resize(n, v)
    }
}

impl<I: Idx, T> Index<I> for IndexVec<I, T> {
    type Output = T;
    #[inline]
    fn index(&self, i: I) -> &T {
        &self.raw[i.index()]
    }
}

impl<I: Idx, T> IndexMut<I> for IndexVec<I, T> {
    #[inline]
    fn index_mut(&mut self, i: I) -> &mut T {
        &mut self.raw[i.index()]
    }
}

/// A contiguous run of elements in a side arena: `arena[start..start + len]`.
pub struct List<T> {
    pub start: u32,
    pub len: u32,
    _m: PhantomData<fn() -> T>,
}

impl<T> Clone for List<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for List<T> {}
impl<T> PartialEq for List<T> {
    fn eq(&self, o: &Self) -> bool {
        self.start == o.start && self.len == o.len
    }
}
impl<T> Eq for List<T> {}
impl<T> std::hash::Hash for List<T> {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.start.hash(h);
        self.len.hash(h);
    }
}
impl<T> std::fmt::Debug for List<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "List[{}..+{}]", self.start, self.len)
    }
}
impl<T> Default for List<T> {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl<T> List<T> {
    pub const EMPTY: List<T> = List { start: 0, len: 0, _m: PhantomData };
    pub fn new(start: usize, len: usize) -> Self {
        List { start: start as u32, len: len as u32, _m: PhantomData }
    }
    #[inline]
    pub fn is_empty(self) -> bool {
        self.len == 0
    }
    #[inline]
    pub fn len(self) -> usize {
        self.len as usize
    }
    #[inline]
    pub fn range(self) -> std::ops::Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }
}

/// An arena of list elements with a stack-discipline scratch buffer, so nested lists can be
/// built without per-node allocation.
pub struct ListArena<T> {
    pub items: Vec<T>,
    scratch: Vec<T>,
}

impl<T> Default for ListArena<T> {
    fn default() -> Self {
        ListArena { items: Vec::new(), scratch: Vec::new() }
    }
}

impl<T: Copy> ListArena<T> {
    #[inline]
    pub fn get(&self, l: List<T>) -> &[T] {
        &self.items[l.range()]
    }
    #[inline]
    pub fn get_mut(&mut self, l: List<T>) -> &mut [T] {
        &mut self.items[l.range()]
    }
    /// Mark the current scratch position; push items with `push_scratch`, then `finish(mark)`.
    #[inline]
    pub fn mark(&self) -> usize {
        self.scratch.len()
    }
    #[inline]
    pub fn push_scratch(&mut self, v: T) {
        self.scratch.push(v);
    }
    /// Drop scratch items pushed since `mark` (a parse that failed partway through a list).
    pub fn truncate_scratch(&mut self, mark: usize) {
        self.scratch.truncate(mark);
    }
    pub fn finish(&mut self, mark: usize) -> List<T> {
        let n = self.scratch.len() - mark;
        if n == 0 {
            return List::EMPTY;
        }
        let start = self.items.len();
        self.items.extend(self.scratch.drain(mark..));
        List::new(start, n)
    }
    pub fn alloc(&mut self, vals: &[T]) -> List<T> {
        if vals.is_empty() {
            return List::EMPTY;
        }
        let start = self.items.len();
        self.items.extend_from_slice(vals);
        List::new(start, vals.len())
    }
    pub fn alloc_iter(&mut self, vals: impl IntoIterator<Item = T>) -> List<T> {
        let start = self.items.len();
        self.items.extend(vals);
        let n = self.items.len() - start;
        if n == 0 {
            List::EMPTY
        } else {
            List::new(start, n)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_lists_build_with_stack_discipline() {
        let mut a: ListArena<u32> = ListArena::default();
        let outer = a.mark();
        a.push_scratch(1);
        let inner = a.mark();
        a.push_scratch(10);
        a.push_scratch(11);
        let l_inner = a.finish(inner);
        a.push_scratch(2);
        let l_outer = a.finish(outer);
        assert_eq!(a.get(l_inner), &[10, 11]);
        assert_eq!(a.get(l_outer), &[1, 2]);
    }
}
