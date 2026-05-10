//! `Shard` user-facing read wrappers (Layer C).
//!
//! All methods here are thin projections over `find` (from `scan.rs`):
//!
//! ```text
//! find → maybe (hits/misses bump + visited |= 1<<pos) → entry_ptr_from_tag → project
//! ```
//!
//! The promoting half (`get` / `get_mut` / `get_key_value` and the hit path of
//! `get_or_insert_with`) shares a `find_and_touch` helper that folds the
//! counter-and-visited bookkeeping; the non-promoting half (`peek*` / `contains`)
//! calls `find` directly because counter / visited handling is a no-op for them.
//! `find_and_touch` is `#[inline]`, so the `promote` branch is constant-folded
//! at every call site.
//!
//! Editing this file in isolation is safe whenever the shard's `find` /
//! addressing primitives are unchanged. When the SIEVE state machine
//! (`state.rs`) moves, this layer typically does not need to follow.

use std::borrow::Borrow;
use std::hash::Hash;
use std::num::NonZeroU16;

use super::Shard;
use crate::SlotSize;

impl<K, V, S: SlotSize> Shard<K, V, S>
where
    K: Hash + Eq,
{
    pub(crate) fn contains<Q>(&self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> bool
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        self.find(key, Self::needle_from_hash(hash), has_avx2_bmi1)
            .is_some()
    }

    /// `find` + optional promotion (hits/misses bump + visited bit set).
    /// `promote=true` is the `get` family; `peek` family calls `find` directly
    /// since they neither bump counters nor set VISITED. The `#[inline]` hint
    /// lets LLVM constant-fold the `promote` branch away per call site.
    #[inline]
    fn find_and_touch<Q>(
        &mut self,
        key: &Q,
        hash: u64,
        has_avx2_bmi1: bool,
        promote: bool,
    ) -> Option<(usize, NonZeroU16)>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let needle = Self::needle_from_hash(hash);
        match self.find(key, needle, has_avx2_bmi1) {
            Some((pos, tag)) => {
                if promote {
                    self.hits += 1;
                    self.visited |= 1u64 << pos;
                }
                Some((pos, tag))
            }
            None => {
                if promote {
                    self.misses += 1;
                }
                None
            }
        }
    }

    pub(crate) fn get<Q>(&mut self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let (_pos, tag) = self.find_and_touch(key, hash, has_avx2_bmi1, true)?;
        // SAFETY: tag came from a live slot in `find`.
        Some(unsafe { &(*self.entry_ptr_from_tag(tag)).value })
    }

    pub(crate) fn get_mut<Q>(&mut self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let (_pos, tag) = self.find_and_touch(key, hash, has_avx2_bmi1, true)?;
        // SAFETY: tag came from a live slot in `find`; `&mut self` makes the
        // returned `&mut V` the only outstanding borrow into the entry.
        Some(unsafe { &mut (*self.entry_ptr_mut_from_tag(tag)).value })
    }

    /// Non-promoting lookup. Same as `get` but does not set the VISITED bit,
    /// so peeked entries do not survive an extra SIEVE sweep.
    pub(crate) fn peek<Q>(&self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let needle = Self::needle_from_hash(hash);
        let (_pos, tag) = self.find(key, needle, has_avx2_bmi1)?;
        // SAFETY: tag came from a live slot in `find`.
        let e = unsafe { &*self.entry_ptr_from_tag(tag) };
        Some(&e.value)
    }

    /// Non-promoting `&mut V` lookup. Like `get_mut` but does not set VISITED.
    pub(crate) fn peek_mut<Q>(&mut self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let needle = Self::needle_from_hash(hash);
        let (_pos, tag) = self.find(key, needle, has_avx2_bmi1)?;
        // SAFETY: tag came from a live slot in `find`; `&mut self` makes the
        // returned `&mut V` the only outstanding borrow into the entry.
        let e = unsafe { &mut *self.entry_ptr_mut_from_tag(tag) };
        Some(&mut e.value)
    }

    /// Promoting lookup that returns `(&K, &V)`. Sets VISITED on hit (same
    /// as `get`).
    pub(crate) fn get_key_value<Q>(
        &mut self,
        key: &Q,
        hash: u64,
        has_avx2_bmi1: bool,
    ) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let (_pos, tag) = self.find_and_touch(key, hash, has_avx2_bmi1, true)?;
        // SAFETY: tag came from a live slot in `find`.
        let e = unsafe { &*self.entry_ptr_from_tag(tag) };
        Some((&e.key, &e.value))
    }

    /// Non-promoting variant of `get_key_value`.
    pub(crate) fn peek_key_value<Q>(
        &self,
        key: &Q,
        hash: u64,
        has_avx2_bmi1: bool,
    ) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let needle = Self::needle_from_hash(hash);
        let (_pos, tag) = self.find(key, needle, has_avx2_bmi1)?;
        // SAFETY: tag came from a live slot in `find`.
        let e = unsafe { &*self.entry_ptr_from_tag(tag) };
        Some((&e.key, &e.value))
    }

    /// On hit: set VISITED and return `&value`. On miss: evaluate `f`, insert,
    /// and return `&value` of the freshly inserted entry. The new entry always
    /// lives at `tags[self.len - 1]` (steady-state evict path writes there;
    /// warm-up path post-increments `self.len`), which avoids a second `find`.
    pub(crate) fn get_or_insert_with<F>(
        &mut self,
        key: K,
        hash: u64,
        has_avx2_bmi1: bool,
        f: F,
    ) -> &V
    where
        F: FnOnce() -> V,
    {
        if let Some((_pos, tag)) = self.find_and_touch(&key, hash, has_avx2_bmi1, true) {
            // SAFETY: tag came from a live slot in `find`.
            let e = unsafe { &*self.entry_ptr_from_tag(tag) };
            return &e.value;
        }
        // `find_and_touch` already bumped misses on the None path. `insert`
        // increments `insertions` (and `evictions` if it overflows capacity)
        // and re-runs `find` internally to detect duplicate keys; the miss
        // path here never hits the replace branch, so this does not
        // double-count any counter.
        let value = f();
        let _evicted = self.insert(key, value, hash, has_avx2_bmi1);
        let pos = self.len - 1;
        let id = Self::id_of(self.tags[pos]);
        // SAFETY: insert just wrote a live tag at write_pos = self.len - 1
        // (warm-up: pos = old self.len then len += 1; evict: write_pos = last = len - 1).
        let e = unsafe { &*self.entry_ptr(id) };
        &e.value
    }
}
