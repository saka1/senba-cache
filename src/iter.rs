//! Iterator types for [`Cache`]: [`Iter`], [`IterMut`], [`Keys`], [`Values`],
//! and the draining iterator [`Drain`].

use std::hash::BuildHasher;
use std::marker::PhantomData;

use super::{Cache, EMPTY, Entry, Shard, SlotSize};

/// Iterator over a [`Cache`] yielding `(&K, &V)` pairs. Created by [`Cache::iter`].
pub struct Iter<'a, K, V, S: SlotSize> {
    pub(super) shards: &'a [Shard<K, V, S>],
    pub(super) shard_idx: usize,
    pub(super) slot_idx: usize,
}

impl<'a, K, V, S: SlotSize> Iterator for Iter<'a, K, V, S> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let sh = self.shards.get(self.shard_idx)?;
            if self.slot_idx >= sh.len {
                self.shard_idx += 1;
                self.slot_idx = 0;
                continue;
            }
            let i = self.slot_idx;
            self.slot_idx += 1;
            let id = Shard::<K, V, S>::id_of(sh.tags[i]);
            // SAFETY: tags[0..len] are live (I4'), so entries[id] is initialized (I6).
            // The lifetime 'a comes from `shards: &'a [Shard<...>]`, so the returned
            // references stay valid for the iterator's lifetime.
            let e = unsafe { &*sh.entry_ptr(id) };
            return Some((&e.key, &e.value));
        }
    }
}

/// Mutable iterator over a [`Cache`] yielding `(&K, &mut V)` pairs. Created by [`Cache::iter_mut`].
///
/// Holds a raw pointer to the cache's shard array plus a `PhantomData<&'a mut [Shard]>`
/// to encode the exclusive borrow at the type level. The implementation walks
/// shards via raw pointer arithmetic and reads `len` / `tags[i]` through
/// `addr_of!` projections so that no intermediate `&mut Shard` ever exists
/// while a previously-yielded `&'a mut V` is alive — `&mut Shard` would
/// otherwise claim unique access to bytes the caller still holds a borrow into.
pub struct IterMut<'a, K, V, S: SlotSize> {
    pub(super) shards: *mut Shard<K, V, S>,
    pub(super) n_shards: usize,
    pub(super) shard_idx: usize,
    pub(super) slot_idx: usize,
    pub(super) _marker: PhantomData<&'a mut [Shard<K, V, S>]>,
}

// SAFETY: IterMut is morally `&'a mut [Shard<K, V, S>]` — same Send/Sync
// bounds as the underlying mutable slice reference.
unsafe impl<K: Send, V: Send, S: SlotSize> Send for IterMut<'_, K, V, S> {}
unsafe impl<K: Sync, V: Sync, S: SlotSize> Sync for IterMut<'_, K, V, S> {}

impl<'a, K, V, S: SlotSize> Iterator for IterMut<'a, K, V, S> {
    type Item = (&'a K, &'a mut V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.shard_idx >= self.n_shards {
                return None;
            }
            // SAFETY: shards points into the cache's Box<[Shard]> for which we
            // hold an exclusive borrow (encoded in `_marker`). shard_idx is
            // bounded by n_shards.
            let sh: *mut Shard<K, V, S> = unsafe { self.shards.add(self.shard_idx) };
            // SAFETY: addr_of! produces a raw pointer to the field without
            // forming any intermediate reference, so the read does not alias
            // previously yielded `&mut V` borrows into entries[id].
            let len = unsafe { std::ptr::addr_of!((*sh).len).read() };
            if self.slot_idx >= len {
                self.shard_idx += 1;
                self.slot_idx = 0;
                continue;
            }
            let i = self.slot_idx;
            self.slot_idx += 1;
            // SAFETY: tags[0..len] are LIVE (I4'). We read the u16 through the
            // tags storage's data pointer; the brief shared reborrow of
            // `AlignedTags` (deref-coerced to `&[u16]` for `as_ptr`) covers
            // only the tags-storage metadata bytes inside Shard, which are
            // disjoint from the entries arena's heap allocation where any
            // outstanding `&mut V` lives.
            let tag = unsafe {
                let tags_field = std::ptr::addr_of!((*sh).tags);
                let data = (*tags_field).as_ptr();
                *data.add(i)
            };
            let id = Shard::<K, V, S>::id_of(tag);
            // SAFETY: same disjoint-fields argument for the entries Vec
            // metadata. `entries[id]` is initialized (I6) because the tag is
            // LIVE. Distinct (shard_idx, id) pairs across iterations means
            // yielded `&mut V`s do not alias each other.
            let entry_ptr: *mut Entry<K, V> = unsafe {
                let entries_field = std::ptr::addr_of_mut!((*sh).entries);
                (*entries_field).as_mut_ptr().add(id) as *mut Entry<K, V>
            };
            // SAFETY: entry_ptr is a unique, valid pointer to an initialized
            // Entry; reborrowing as &'a (key) and &'a mut (value) is sound for
            // the rest of the iterator's lifetime.
            let key: &'a K = unsafe { &*std::ptr::addr_of!((*entry_ptr).key) };
            let value: &'a mut V = unsafe { &mut *std::ptr::addr_of_mut!((*entry_ptr).value) };
            return Some((key, value));
        }
    }
}

/// Iterator over a [`Cache`]'s keys. Created by [`Cache::keys`].
pub struct Keys<'a, K, V, S: SlotSize> {
    pub(super) iter: Iter<'a, K, V, S>,
}

impl<'a, K, V, S: SlotSize> Iterator for Keys<'a, K, V, S> {
    type Item = &'a K;
    fn next(&mut self) -> Option<&'a K> {
        self.iter.next().map(|(k, _)| k)
    }
}

/// Iterator over a [`Cache`]'s values. Created by [`Cache::values`].
pub struct Values<'a, K, V, S: SlotSize> {
    pub(super) iter: Iter<'a, K, V, S>,
}

impl<'a, K, V, S: SlotSize> Iterator for Values<'a, K, V, S> {
    type Item = &'a V;
    fn next(&mut self) -> Option<&'a V> {
        self.iter.next().map(|(_, v)| v)
    }
}

/// Draining iterator over a [`Cache`], yielding owned `(K, V)` pairs.
/// Created by [`Cache::drain`].
///
/// At construction the cache is reset to logically empty (`len = 0`,
/// `hand = 0`, all tags zeroed); the [`Drain`] retains exclusive access via
/// `&mut Cache` and walks the original arena slots through saved per-shard
/// lengths. Dropping the [`Drain`] drops every still-pending entry; leaking
/// it via [`std::mem::forget`] leaks every still-pending entry but leaves
/// the cache in a consistent, reusable state (see [`Cache::drain`]).
pub struct Drain<'a, K, V, S: SlotSize, H: BuildHasher> {
    cache: &'a mut Cache<K, V, S, H>,
    /// Per-shard length captured at `Drain::new`. Together with invariant I8
    /// (`live ids = 0..len` per shard) this fully describes the set of still
    /// initialized arena slots — no per-slot tag inspection is needed.
    old_lens: Box<[usize]>,
    shard_idx: usize,
    /// Next entry id to drain in `shards[shard_idx]`. Monotonically advances
    /// to `old_lens[shard_idx]`, then resets when `shard_idx` is bumped. The
    /// monotonic advance is what makes `next()` and `Drop`'s cleanup pass
    /// disjoint (no double-drop and no double-`ptr::read`).
    next_id: usize,
}

impl<'a, K, V, S, H> Drain<'a, K, V, S, H>
where
    S: SlotSize,
    H: BuildHasher,
{
    pub(super) fn new(cache: &'a mut Cache<K, V, S, H>) -> Self {
        // Snapshot per-shard `len` and reset every shard to the empty state.
        // Tags `[0..old_len]` must be zeroed here (rather than incrementally
        // as entries are yielded): a subsequent `insert` walks a SIMD scan
        // window of `round_up(new_len, LANE)` tags, which can extend past
        // the slots `insert` itself has just written. Stale LIVE bits in
        // that window can spuriously match the new hash, leading SIMD `find`
        // to dereference an arena slot whose entry was already yielded by
        // (and hence moved out of) this Drain — that read would be UB.
        // Pre-zeroing is the cheapest way to make `mem::forget(drain)`
        // followed by arbitrary cache use sound: the cache sees a fully
        // consistent empty state irrespective of how many entries the
        // user actually consumed before forgetting.
        let old_lens: Box<[usize]> = cache
            .shards
            .iter_mut()
            .map(|sh| {
                let l = sh.len;
                for t in &mut sh.tags[..l] {
                    *t = EMPTY;
                }
                sh.len = 0;
                sh.hand = 0;
                l
            })
            .collect();
        Drain {
            cache,
            old_lens,
            shard_idx: 0,
            next_id: 0,
        }
    }
}

impl<K, V, S, H> Iterator for Drain<'_, K, V, S, H>
where
    S: SlotSize,
    H: BuildHasher,
{
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        loop {
            if self.shard_idx >= self.old_lens.len() {
                return None;
            }
            let old_len = self.old_lens[self.shard_idx];
            if self.next_id >= old_len {
                self.shard_idx += 1;
                self.next_id = 0;
                continue;
            }
            let id = self.next_id;
            self.next_id += 1;
            let sh = &self.cache.shards[self.shard_idx];
            // SAFETY: by I8, the shard's live ids at the time of `Drain::new`
            // were exactly `0..old_lens[shard_idx]`, so `entries[id]` was
            // initialized then. No prior `next()` call has moved `id` out
            // (the monotonic `next_id` counter ensures each id is visited
            // at most once), and `Drain::new` did not touch the entries
            // arena. Therefore `entry_ptr(id)` points at an initialized
            // `Entry<K, V>`. The `next_id` advance happens *before* the
            // read, so an unwind during `Drop` cleanup will not revisit
            // this id even if the read itself somehow panicked (it cannot
            // — `ptr::read` is a memcpy).
            let entry = unsafe { std::ptr::read(sh.entry_ptr(id)) };
            return Some((entry.key, entry.value));
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let mut remaining = 0usize;
        if self.shard_idx < self.old_lens.len() {
            remaining += self.old_lens[self.shard_idx].saturating_sub(self.next_id);
            for &l in &self.old_lens[self.shard_idx + 1..] {
                remaining += l;
            }
        }
        (remaining, Some(remaining))
    }
}

impl<K, V, S, H> ExactSizeIterator for Drain<'_, K, V, S, H>
where
    S: SlotSize,
    H: BuildHasher,
{
}

impl<K, V, S, H> std::iter::FusedIterator for Drain<'_, K, V, S, H>
where
    S: SlotSize,
    H: BuildHasher,
{
}

impl<K, V, S, H> Drop for Drain<'_, K, V, S, H>
where
    S: SlotSize,
    H: BuildHasher,
{
    /// Drops every still-pending entry. Mirrors [`std::vec::Drain`]'s
    /// "consume the rest" semantics so that an early-dropped iterator
    /// always leaves the cache empty (rather than half-drained). A user
    /// `Drop` that panics during this cleanup pass is treated like any
    /// other double-panic — the second panic aborts.
    fn drop(&mut self) {
        while self.next().is_some() {}
    }
}
