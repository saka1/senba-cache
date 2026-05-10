//! `Shard` SIMD/scalar scan + SIEVE bit-scan primitives.
//!
//! Layer A (primitives) of the shard module split:
//! - `find` / `find_scalar` / `find_avx2`: scan over `tags[0..len]` for a key
//!   match. Used by both `state.rs` (insert / remove) and `lookup.rs` (get,
//!   peek, get_or_insert_with, ...) via `find_and_touch`.
//! - `find_evict_pos`: SIEVE victim search via bit-twiddles on `self.visited`.
//!   Called only from `state.rs::insert`.
//! - `needle_from_hash`: hash-bits → tag-needle folding shared across find paths.
//!
//! The shape and contracts are unchanged from the pre-split single-file form;
//! see `crate::shard` (mod.rs) for the bit-layout invariants (I4'..I8) and
//! the c-hoist trick.
//!
//! `find_*` methods don't require `K: Hash` (only `K: Borrow<Q>` + `Q: Eq`),
//! so this impl block intentionally drops the `where K: Hash + Eq` constraint
//! present on the user-facing impl block in `mod.rs`.

use std::borrow::Borrow;
use std::num::NonZeroU16;

use super::{Entry, LANE, Shard};
use crate::SlotSize;

/// `find` returns `Option<(usize, NonZeroU16)>`. Wrapping the tag in
/// `NonZeroU16` (live tags always have LIVE = 0x8000 set, so they are
/// non-zero) lets niche optimization fold the Option's discriminant into
/// the tag's all-zero pattern, keeping `sizeof(Option<(usize, NonZeroU16)>)
/// == 16` so the value rides registers (`rax` + `rdx` on x86_64 SysV)
/// instead of being returned via sret. See
/// `docs/reports/2026-05-08-find-avx2-caller-merge.md` §5 OQ-1.
const _FIND_RET_FITS_REGISTERS: () = assert!(
    std::mem::size_of::<Option<(usize, NonZeroU16)>>() == 16,
    "find return type must fit in 2 registers (16 byte) to avoid sret"
);

impl<K, V, S: SlotSize> Shard<K, V, S> {
    /// Folds the top 9 bits of a 64-bit hash into the tag's hash field. The hash
    /// field is split by ID_MASK: the low `ID_SHIFT` bits sit at `[0, ID_SHIFT)`
    /// and the remaining bits sit at `[ID_SHIFT + 6, 15)` (just under the LIVE bit).
    /// Since `ID_SHIFT ≤ 6` for every supported `SlotSize` and 9 hash bits never
    /// exceeds `ID_SHIFT + (15 - (ID_SHIFT + 6)) = 9`, the spread is bijective for
    /// all three brackets (Slot16/32/64).
    #[inline]
    pub(crate) fn needle_from_hash(hash: u64) -> u16 {
        let h = ((hash >> 55) as u16) & 0x1FF;
        let s = Self::ID_SHIFT;
        let low_mask: u16 = (1u16 << s) - 1;
        let low = h & low_mask;
        let high = (h & !low_mask) << 6;
        super::LIVE | low | high
    }

    /// Returns `(pos, tag)` of the matching slot, with the tag wrapped in
    /// `NonZeroU16` so the whole `Option` is 16 byte and rides registers
    /// (no sret). Live tags are always non-zero (LIVE bit set), so the
    /// `NonZeroU16::new_unchecked` calls below are sound at the type level.
    #[inline]
    pub(super) fn find<Q>(
        &self,
        key: &Q,
        needle: u16,
        has_avx2_bmi1: bool,
    ) -> Option<(usize, NonZeroU16)>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        #[cfg(target_arch = "x86_64")]
        {
            if has_avx2_bmi1 {
                // SAFETY: `has_avx2_bmi1` was set from `is_x86_feature_detected!("avx2")` at
                // Cache construction (see `Cache::new`), which also implies BMI1 on every
                // CPU that ships AVX2. The detection result is valid for the process
                // lifetime, so caching it is sound.
                return unsafe { self.find_avx2(key, needle) };
            }
        }
        let _ = has_avx2_bmi1; // avoid unused-arg warning on non-x86_64
        self.find_scalar(key, needle)
    }

    #[inline]
    fn find_scalar<Q>(&self, key: &Q, needle: u16) -> Option<(usize, NonZeroU16)>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        for (i, &t) in self.tags[..self.len].iter().enumerate() {
            if (t & Self::SCAN_MASK) == needle {
                let id = Self::id_of(t);
                // SAFETY: a live tag implies entries[id] is initialized (I5/I6).
                let e = unsafe { &*self.entry_ptr(id) };
                if e.key.borrow() == key {
                    // SAFETY: scan match implies LIVE bit set, so t != 0.
                    return Some((i, unsafe { NonZeroU16::new_unchecked(t) }));
                }
            }
        }
        None
    }

    /// AVX2 + BMI1 scan of `tags[..]` against SCAN_MASK. Same shape as j8;
    /// the c-hoist trick (`tag & ID_MASK = id × S::SIZE`) holds at slot granularity.
    ///
    /// # Safety
    ///
    /// The host CPU must support both AVX2 and BMI1 (BMI1 is implied by AVX2 on every
    /// x86_64 part shipped to date). The caller is responsible for the runtime feature
    /// check; `Shard::find` performs it via the cached `has_avx2_bmi1` flag set in
    /// `Cache::new`.
    #[cfg(target_arch = "x86_64")]
    #[inline]
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_avx2<Q>(&self, key: &Q, needle: u16) -> Option<(usize, NonZeroU16)>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        // Re-anchor layout invariants at this use site (the c-hoist arithmetic below
        // assumes `sizeof(Storage<Entry>) == S::SIZE`, which `_STORAGE_SIZE_OK` enforces;
        // the aligned load below assumes 32-byte alignment, which `_TAGSCHUNK_ALIGN_OK`
        // enforces structurally — the `debug_assert_eq!` further down only catches
        // runtime mishaps under `cfg(debug_assertions)`).
        let _: () = Self::_SIZE_OK;
        let _: () = Self::_STORAGE_SIZE_OK;
        let _: () = Self::_TAGSCHUNK_ALIGN_OK;
        use std::arch::x86_64::*;
        // Round `len` up to LANE. Tags beyond `len` are EMPTY (= 0) and the LIVE-bit
        // check would skip them anyway, but bounding the scan at the rounded-up live
        // region keeps the SIMD path competitive at low fill ratios.
        // `tags.len()` is itself LANE-aligned at construction, so this never exceeds it.
        let limit = (self.len + LANE - 1) & !(LANE - 1);
        debug_assert!(limit <= self.tags.len());
        let tags_ptr = self.tags.as_ptr();
        // `AlignedTags` storage is `Vec<TagsChunk>` with `align(32)` on the chunk,
        // so the flat `*const u16` view starts on a 32-byte boundary. The aligned
        // load below relies on that, plus `i += LANE` (= 32-byte stride).
        debug_assert_eq!(
            (tags_ptr as usize) & 31,
            0,
            "tags storage must be 32-byte aligned for vmovdqa"
        );
        let tags_byte_ptr = tags_ptr as *const u8;
        // Hold entries as a byte pointer. sizeof(Storage<Entry>) == S::SIZE (fixed),
        // so `tag & ID_MASK` is directly the byte offset into the arena.
        // Storage's first field is entry at offset 0, so we reach Entry directly.
        let entries_byte_ptr = self.entries.as_ptr() as *const u8;
        // SAFETY: AVX2/BMI1 are guaranteed by the `#[target_feature]` contract on
        // this fn; the caller has ensured the host CPU supports them.
        unsafe {
            let needle_v = _mm256_set1_epi16(needle as i16);
            let mask_v = _mm256_set1_epi16(Self::SCAN_MASK as i16);
            let id_mask_u32 = Self::ID_MASK as u32;

            let mut i = 0usize;
            while i < limit {
                // i is always a multiple of LANE, so `tags_ptr.add(i)` advances
                // by a multiple of 32 bytes from the (32-aligned) base.
                let v = _mm256_load_si256(tags_ptr.add(i) as *const __m256i);
                let masked = _mm256_and_si256(v, mask_v);
                let cmp = _mm256_cmpeq_epi16(masked, needle_v);
                let mut mask = _mm256_movemask_epi8(cmp) as u32;

                let chunk_byte_ptr = tags_byte_ptr.add(i * 2);

                while mask != 0 {
                    let bit = mask.trailing_zeros() as usize;
                    let tag = *(chunk_byte_ptr.add(bit) as *const u16);
                    let id_bytes = (tag as u32 & id_mask_u32) as usize;
                    // live needle ⟹ tag live ⟹ entries[id] initialized (I6).
                    // id_bytes = id × S::SIZE and id < capacity ⟹ in bounds.
                    // Storage is #[repr(C)] with entry at offset 0 ⟹ Entry reachable directly.
                    let entry_ptr = entries_byte_ptr.add(id_bytes) as *const Entry<K, V>;
                    let e = &*entry_ptr;
                    if e.key.borrow() == key {
                        let lane = bit >> 1;
                        // SAFETY: SCAN_MASK match against a needle that has LIVE set
                        // ⟹ this tag has LIVE set ⟹ tag != 0.
                        return Some((i + lane, NonZeroU16::new_unchecked(tag)));
                    }
                    mask = _blsr_u32(mask);
                    mask = _blsr_u32(mask);
                }
                i += LANE;
            }
            None
        }
    }

    /// SIEVE victim search over `tags[0..len]`, encoded as bit-twiddles on
    /// `self.visited`. Two passes (hand→len then 0→hand) cover the whole live
    /// region; if every position was visited, both passes find nothing but the
    /// bitmap is now zeroed in `[0, len)`, so any position is a valid victim —
    /// we pick `self.hand`.
    ///
    /// Per-shard `len ≤ MAX_PER_SHARD = 64`, so a single `u64` covers the
    /// occupancy and `trailing_zeros` finds the first un-visited bit in a
    /// single instruction. Each clearing pass becomes one `&= !mask`. The old
    /// linear walk in `scan_evict` is gone.
    pub(super) fn find_evict_pos(&mut self) -> usize {
        debug_assert!(self.len > 0 && self.len == self.capacity());
        if self.hand >= self.len {
            self.hand = 0;
        }
        let len = self.len;
        let hand = self.hand;
        // `len` is in `1..=64`; build a `len`-wide live mask without overflow.
        let live_mask: u64 = if len >= 64 { !0u64 } else { (1u64 << len) - 1 };
        // `hand < len ≤ 64` ⟹ `hand ≤ 63`, so `1 << hand` is well-defined.
        let below_hand: u64 = (1u64 << hand) - 1; // bits in [0, hand)
        let above_hand: u64 = live_mask & !below_hand; // bits in [hand, len)

        // Pass 1: first un-visited bit in [hand, len).
        let high_search = !self.visited & above_hand;
        if high_search != 0 {
            let victim = high_search.trailing_zeros() as usize;
            // Walked-over positions [hand, victim) were visited; clear them.
            // (Bits at victim and beyond are untouched, mirroring `scan_evict`'s
            // early-return semantics.)
            let walked = ((1u64 << victim) - 1) & !below_hand;
            self.visited &= !walked;
            return victim;
        }
        // [hand, len) all visited; clear that range of the bitmap.
        self.visited &= !above_hand;

        // Pass 2: first un-visited bit in [0, hand).
        let low_search = !self.visited & below_hand;
        if low_search != 0 {
            let victim = low_search.trailing_zeros() as usize;
            let walked = (1u64 << victim) - 1;
            self.visited &= !walked;
            return victim;
        }
        // All visited. Clear the rest and pick `hand` as the arbitrary victim.
        self.visited &= !below_hand;
        hand
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Slot16, Slot32, Slot64};

    /// Hash spread injectivity across all three brackets, over the full 9-bit
    /// hash field (was 8 bits before the VISITED-bitmap refactor).
    #[test]
    fn needle_spread_is_injective_all_slots() {
        for slot_id in 0..3 {
            let mut seen = std::collections::HashSet::new();
            for h in 0..=511u64 {
                // `needle_from_hash` reads bits [55, 64) of the input hash.
                let needle = match slot_id {
                    0 => Shard::<u64, u64, Slot16>::needle_from_hash(h << 55),
                    1 => Shard::<u64, u64, Slot32>::needle_from_hash(h << 55),
                    2 => Shard::<u64, u64, Slot64>::needle_from_hash(h << 55),
                    _ => unreachable!(),
                };
                assert!(seen.insert(needle), "slot {slot_id} hash {h} collides");
            }
            assert_eq!(seen.len(), 512);
        }
    }
}
