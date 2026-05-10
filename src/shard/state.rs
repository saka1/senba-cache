//! `Shard` SIEVE state-machine mutators (Layer B).
//!
//! These are the operations that transition the shard between valid SIEVE
//! configurations under invariants I4'..I8 (documented in
//! `crate::shard` / `mod.rs`):
//!
//! - `insert`: warm-up vs evict path; shift-on-evict keeps `tags[0..len]`
//!   contiguously LIVE so the SIMD `find` window is always exactly `len` wide
//!   and the eviction sequence matches `sieve_orig` byte-for-byte.
//! - `remove`: tag-level shift (mirroring `sieve_orig`'s linked-list unlink)
//!   plus an id-level swap-to-fill-gap to restore I8.
//! - `clear`: drops every live entry and resets the shard.
//! - `retain(f)`: single-pass keep/drop compaction with a bitmap-based id
//!   remap; panic-safe via an internal RAII guard.
//!
//! All four call into `find` (`scan.rs`) for key lookup or into the
//! addressing primitives (`mod.rs`); none of them dereference `tags` /
//! `entries` directly without going through those layers.

use std::borrow::Borrow;
use std::hash::Hash;

use super::{EMPTY, Entry, LIVE, Shard, shift_visited_down_in_place};
use crate::SlotSize;

impl<K, V, S: SlotSize> Shard<K, V, S>
where
    K: Hash + Eq,
{
    pub(crate) fn insert(
        &mut self,
        key: K,
        value: V,
        hash: u64,
        has_avx2_bmi1: bool,
    ) -> Option<(K, V)> {
        self.insertions += 1;
        let needle = Self::needle_from_hash(hash);
        if let Some((pos, tag)) = self.find(&key, needle, has_avx2_bmi1) {
            // SAFETY: tag came from a live slot in `find`.
            let e = unsafe { &mut *self.entry_ptr_mut_from_tag(tag) };
            e.value = value;
            self.visited |= 1u64 << pos;
            return None;
        }

        // New entry. Warm-up extends the live region by one (`pos = self.len`);
        // steady state evicts at the SIEVE-chosen position, shifts the tail down,
        // and writes the new tag at `tags[len-1]` (the head end). Both paths end
        // with `tags[0..len]` contiguously LIVE, so no compaction is ever needed.
        let (evicted, write_pos, entry_id) = if self.len < self.capacity() {
            let pos = self.len;
            let id = self.len as u16;
            self.len += 1;
            (None, pos, id)
        } else {
            self.evictions += 1;
            let pos = self.find_evict_pos();
            let id = Self::id_of(self.tags[pos]) as u16;
            // SAFETY: live ⟹ entries[id] initialized (I6). After read, entries[id] is
            // uninit; we re-initialize it via ptr::write below before any other access.
            let entry = unsafe { std::ptr::read(self.entry_ptr(id as usize)) };

            // Shift tags[pos+1..len] down to tags[pos..len-1] so the live region
            // stays "tail (= 0) → head (= len-1)" ordered, mirroring sieve_orig's
            // linked-list unlink. The freed entry id is reused for the new tag.
            let last = self.len - 1;
            self.tags.copy_within(pos + 1..self.len, pos);
            // Mirror the same shift on the visited bitmap so bit `i` keeps
            // tracking the (post-shift) tag at `tags[i]`. Bit `last` becomes 0,
            // which is exactly what we want for the new entry written below.
            shift_visited_down_in_place(&mut self.visited, pos);

            // Hand mirrors sieve_orig's `hand = victim.prev` — the SIEVE successor,
            // which is now at `pos` after the shift (or wraps to 0 if victim was
            // the head, since head has no .prev in sieve_orig).
            self.hand = if pos < last { pos } else { 0 };

            (Some((entry.key, entry.value)), last, id)
        };

        self.tags[write_pos] = LIVE | (entry_id << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        // SAFETY: entry_id is either an unused warm-up slot or one just freed by evict.
        // Storage's entry field is at offset 0, so raw write reaches Entry directly.
        unsafe {
            std::ptr::write(self.entry_ptr_mut(entry_id as usize), Entry { key, value });
        }

        evicted
    }

    /// Removes `key` and returns its value. Slow path: O(per_shard) shift + linear
    /// scan for id swap.
    ///
    /// **Tag-level shift** (mirrors `sieve_orig`'s linked-list unlink): shifts
    /// `tags[pos+1..len]` down by one and marks the freed end EMPTY. This preserves
    /// the relative SIEVE order of all unaffected entries (= I4' is restored: no
    /// holes in the live region).
    ///
    /// **Id-level swap-to-fill-gap**: after the shift, also swaps `removed_id` with
    /// `self.len - 1` (the maximum live id) to restore I8 (live ids = `0..len`).
    /// This keeps the free-list-free structure intact so the warm-up branch in
    /// `insert` (`entry_id = self.len`) works correctly on the next insertion.
    pub(crate) fn remove<Q>(&mut self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let needle = Self::needle_from_hash(hash);
        let (pos, tag) = self.find(key, needle, has_avx2_bmi1)?;
        let removed_id = Self::id_of(tag.get());

        // (1) Read Entry out of entries[removed_id]. After this, entries[removed_id]
        // is logically uninitialized.
        // SAFETY: live ⟹ entries[removed_id] initialized (I6).
        let entry = unsafe { std::ptr::read(self.entry_ptr(removed_id)) };

        // (2) Shift tags down, marking the new tail as EMPTY (preserves I4').
        // After this, the live region is `tags[0..self.len - 1]`. Mirror the
        // same shift on the visited bitmap; the bit at the new (now-out-of-range)
        // position `last` is implicitly 0 because the source bit at `len` was 0.
        let last = self.len - 1;
        self.tags.copy_within(pos + 1..self.len, pos);
        self.tags[last] = EMPTY;
        shift_visited_down_in_place(&mut self.visited, pos);
        self.len = last;

        // (3) Adjust hand for the shift. Tags at positions > pos shifted one
        // slot down, so if hand was past pos it moves with its tag. When
        // hand == pos, the shift brought the successor (pos+1) down to pos,
        // so hand already points at the SIEVE successor — no adjustment
        // needed (mirrors sieve_orig's `hand = node.prev`). Wrap at len.
        if self.hand > pos {
            self.hand -= 1;
        }
        if self.hand >= self.len {
            self.hand = 0;
        }

        // (4) Restore I8: move entries[max_id] → entries[removed_id] via id swap.
        // The owning tag (somewhere in tags[0..len]) gets its id field rewritten
        // to point at the new slot.
        let max_id = self.len;
        if removed_id < max_id {
            let mut found = false;
            for i in 0..self.len {
                let t = self.tags[i];
                if t & LIVE != 0 && Self::id_of(t) == max_id {
                    // SAFETY: removed_id != max_id (guarded by the outer if), both initialized.
                    // After read, entries[max_id] becomes uninit; its tag is rewritten.
                    unsafe {
                        let v = std::ptr::read(self.entry_ptr(max_id));
                        std::ptr::write(self.entry_ptr_mut(removed_id), v);
                    }
                    let cleared = t & !Self::ID_MASK;
                    let new_id_field = (removed_id as u16) << Self::ID_SHIFT;
                    self.tags[i] = cleared | new_id_field;
                    found = true;
                    break;
                }
            }
            debug_assert!(
                found,
                "live id {max_id} should be referenced by some live tag"
            );
        }

        Some(entry.value)
    }

    /// Filters entries in place via `f(&K, &mut V) -> bool`. Single-pass
    /// compaction over `tags[0..len]` plus a bitmap-based id remap to restore I8
    /// (live ids = `0..len`). Avoids the per-deletion `find` + memmove +
    /// id-swap that a naive `iter`+`remove` loop would pay (`O(k·n)` → `O(n)`
    /// with the per-shard `n ≤ MAX_PER_SHARD` cap making the id remap a fixed
    /// `≤ 64×64` bitscan).
    pub(crate) fn retain<F>(&mut self, f: &mut F)
    where
        F: FnMut(&K, &mut V) -> bool,
    {
        let old_len = self.len;
        let old_hand = self.hand;

        // Panic guard: if `f` unwinds, drop every still-live entry and reset
        // the shard to empty so subsequent operations cannot UAF on a tag
        // pointing at a dropped slot. The keep/drop pass below maintains the
        // invariant that `tags[i] & LIVE != 0` iff `entries[id_of(tags[i])]`
        // is still initialized, so this loop is sound at any panic point.
        struct Guard<'a, K, V, S: SlotSize> {
            shard: &'a mut Shard<K, V, S>,
        }
        impl<K, V, S: SlotSize> Drop for Guard<'_, K, V, S> {
            fn drop(&mut self) {
                for i in 0..self.shard.tags.len() {
                    let t = self.shard.tags[i];
                    if t & LIVE != 0 {
                        let id = Shard::<K, V, S>::id_of(t);
                        // SAFETY: LIVE bit ⟹ entries[id] initialized (I6).
                        unsafe { std::ptr::drop_in_place(self.shard.entry_ptr_mut(id)) };
                        self.shard.tags[i] = EMPTY;
                    }
                }
                self.shard.len = 0;
                self.shard.hand = 0;
                self.shard.visited = 0;
            }
        }
        let guard = Guard { shard: self };
        let shard = &mut *guard.shard;

        // Snapshot the visited bitmap; we accumulate the post-compaction view
        // into `new_visited` and commit at the end. If `f` panics mid-loop we
        // simply discard the local — the guard's Drop zeroes `shard.visited`.
        let old_visited = shard.visited;

        // Pass 1: walk tags[0..old_len], decide keep/drop, compact survivors
        // into tags[0..write] in place. Each iteration is structured so that
        // if `f` panics, the only mutation already committed is to the slot
        // currently being read — and that slot is left in a state the guard's
        // Drop can clean up uniformly (LIVE tag ⟹ initialized entry).
        let mut write = 0usize;
        let mut new_visited: u64 = 0;
        let mut drops_before_hand = 0usize;
        for read in 0..old_len {
            let t = shard.tags[read];
            debug_assert!(t & LIVE != 0, "tags[0..len] must be LIVE under I4'");
            let id = Self::id_of(t);
            // SAFETY: LIVE ⟹ entries[id] initialized (I6). The closure receives
            // &K and &mut V via raw-pointer reborrow; nothing else aliases.
            let keep = unsafe {
                let p = shard.entry_ptr_mut(id);
                f(&(*p).key, &mut (*p).value)
            };
            if keep {
                if (old_visited >> read) & 1 != 0 {
                    new_visited |= 1u64 << write;
                }
                if write != read {
                    shard.tags[write] = t;
                    shard.tags[read] = EMPTY;
                }
                write += 1;
            } else {
                // Zero the tag *before* dropping so an intervening panic in
                // Drop (rare but possible) can't leave a stale LIVE tag
                // pointing at an uninitialized slot.
                shard.tags[read] = EMPTY;
                // SAFETY: LIVE ⟹ entries[id] initialized (I6). Tag has just
                // been cleared so the guard's cleanup will not visit this id.
                unsafe { std::ptr::drop_in_place(shard.entry_ptr_mut(id)) };
                if read < old_hand {
                    drops_before_hand += 1;
                }
            }
        }

        // Commit new len + hand + visited bitmap. The remaining tags[write..old_len]
        // are already EMPTY (every iteration that increased write zeroed read,
        // every drop iteration zeroed read), so I4' is preserved.
        shard.len = write;
        shard.visited = new_visited;
        shard.hand = if old_hand >= old_len {
            0
        } else {
            let h = old_hand - drops_before_hand;
            if h >= write { 0 } else { h }
        };

        // Pass 2: restore I8 (live ids = 0..write). Surviving ids are some
        // subset of {0..old_len} of size `write`; remap any id ≥ write down
        // to a free slot id < write via a bitmap pairing. Per-shard capacity
        // is bounded by MAX_PER_SHARD (= 64), so a single u64 holds the
        // occupancy and we never need a Vec/HashSet here.
        if write > 0 {
            let mut occupied: u64 = 0;
            for i in 0..write {
                occupied |= 1u64 << Self::id_of(shard.tags[i]);
            }
            let low_mask: u64 = if write >= 64 {
                u64::MAX
            } else {
                (1u64 << write) - 1
            };
            let mut high = occupied & !low_mask;
            let mut free_low = !occupied & low_mask;
            debug_assert_eq!(
                high.count_ones(),
                free_low.count_ones(),
                "high/low remap counts must agree (counting argument)"
            );
            while high != 0 {
                let h_id = high.trailing_zeros() as usize;
                let l_id = free_low.trailing_zeros() as usize;
                debug_assert!(h_id >= write && l_id < write);
                // SAFETY: h_id is occupied (live), l_id was unoccupied (its
                // bit in `occupied` is 0 ⟹ no live tag references it ⟹
                // entries[l_id] is uninitialized).
                unsafe {
                    let v = std::ptr::read(shard.entry_ptr(h_id));
                    std::ptr::write(shard.entry_ptr_mut(l_id), v);
                }
                let mut found = false;
                for i in 0..write {
                    let t = shard.tags[i];
                    if Self::id_of(t) == h_id {
                        let cleared = t & !Self::ID_MASK;
                        let new_id_field = (l_id as u16) << Self::ID_SHIFT;
                        shard.tags[i] = cleared | new_id_field;
                        found = true;
                        break;
                    }
                }
                debug_assert!(
                    found,
                    "high id {h_id} should be referenced by some live tag"
                );
                high &= high - 1;
                free_low &= free_low - 1;
            }
        }

        // Disarm the panic guard; the borrow of `shard` ends here.
        std::mem::forget(guard);
    }

    /// Drops every live entry and resets the shard to empty. Tags in `tags[0..len]`
    /// are zeroed back to EMPTY (the slack `tags[len..]` is already EMPTY under I4').
    pub(crate) fn clear(&mut self) {
        // Mirror Drop: I4' + I5 ⟹ no skip and no double-drop.
        for i in 0..self.len {
            let t = self.tags[i];
            debug_assert!(t & LIVE != 0, "tags[0..len] must be LIVE under I4'");
            let id = Self::id_of(t);
            // SAFETY: live ⟹ entries[id] initialized (I6).
            unsafe { std::ptr::drop_in_place(self.entry_ptr_mut(id)) };
        }
        for t in &mut self.tags[..self.len] {
            *t = EMPTY;
        }
        self.hand = 0;
        self.len = 0;
        self.visited = 0;
    }
}
