//! `senba::Cache` — library-grade SIEVE implementation built on the j8 series,
//! with automatic padding via the `SlotSize` abstraction.
//!
//! Design details: `docs/reports/2026-05-06-senba-sievecache-design.md`.
//!
//! - Public type: [`Cache`]`<K, V, S = Slot32>`. Shard count is chosen automatically
//!   from `capacity` (smallest power of two with `per_shard <= MAX_PER_SHARD`).
//!   Use [`Cache::with_shards`] to override explicitly.
//! - [`SlotSize`] is a sealed trait; impls are [`Slot16`] / [`Slot32`] (default) / [`Slot64`]
//! - The entries arena uses a **fixed stride of `S::SIZE`** (= automatic padding).
//!   `sizeof(Entry<K, V>) <= S::SIZE` is enforced by const-eval with a friendly error message.
//! - The j8 c-hoist trick (`tag & ID_MASK = id × S::SIZE`) holds identically at slot granularity;
//!   the inner SIMD loop shortcut is reused as-is.
//! - **Shift-on-evict** (the key simplification vs the j-series): each steady-state
//!   `insert` evicts at the SIEVE-chosen position, shifts `tags[pos+1..len]` down by
//!   one, and writes the new tag at `tags[len-1]` (the head end). This keeps
//!   `tags[0..len]` contiguously LIVE *and* preserves the array's correspondence
//!   to `sieve_orig`'s tail→head linked-list order — `tags[0]` is always the oldest
//!   entry, `tags[len-1]` always the newest. No `compact` step is ever needed, the
//!   SIMD `find` window is always exactly `len` wide, and the eviction sequence
//!   matches `sieve_orig` byte-for-byte (oracle equivalence under any trace).
//! - `remove` does the same shift (mirroring `sieve_orig`'s linked-list unlink), and
//!   keeps the id-level swap-to-fill-gap so I8 (live ids = `0..len`) holds.
//!
//! ## Invariants (j8: I1–I8 plus the new in-place I4')
//!
//! - I4': `tags[0..len]` are all LIVE (no holes); `tags[len..]` are all EMPTY
//! - I5: entry_ids referenced by live tags are unique, count = `len`
//! - I6: only for ids in the I5 set is the **`entry` field** of `entries[id]` initialized
//! - I7: I5 set ⊆ `0..capacity`
//! - I8: live ids = `0..len` (maintained during warm-up and restored after remove via swap-to-fill-gap)

use crate::hash::Xxh3Build;
use std::hash::{BuildHasher, Hash};
use std::mem::{ManuallyDrop, MaybeUninit};

const EMPTY: u16 = 0;
const LIVE: u16 = 0x8000;
const VISITED: u16 = 0x4000;
/// AVX2 one chunk = 32 bytes = 16 u16 lanes.
const LANE: usize = 16;
/// Structural upper bound for 6-bit entry_id. per_shard must not exceed this.
pub const MAX_PER_SHARD: usize = 64;

// ---------------- SlotSize sealed trait + ZST markers ----------------

mod sealed {
    pub trait Sealed {}
}

/// Sealed trait that specifies the stride (in bytes) of one slot in the entries arena at the type level.
///
/// `S::SIZE` is always a power of two. `Storage<E>` uses a `#[repr(C)] union` internally,
/// placing the `entry` field at **offset 0**, so that reinterpreting `*const Storage<E>` as
/// `*const E` reaches `E` directly.
pub trait SlotSize: sealed::Sealed + 'static {
    /// Slot stride in bytes for this bracket. Always a power of two.
    const SIZE: usize;
    /// Per-bracket storage cell type. Each impl defines a union to ensure
    /// `size_of::<Storage<E>>() == SIZE`.
    type Storage<E>: Sized;
}

/// `Slot16` bracket: stride = 16 bytes.
/// Typical for small primitive pairs such as `(u32, u32)` or `(u64, u64)`.
pub struct Slot16;
/// `Slot32` (default) bracket: stride = 32 bytes.
/// Typical for string-cache use cases such as `(String, V_small)` or `(Arc<str>, Arc<str>)`.
pub struct Slot32;
/// `Slot64` bracket: stride = 64 bytes.
/// For heavier entries such as `(String, String)` or `(K, V_struct_up_to_56B)`.
pub struct Slot64;

impl sealed::Sealed for Slot16 {}
impl sealed::Sealed for Slot32 {}
impl sealed::Sealed for Slot64 {}

#[repr(C)]
pub union Slot16Storage<E> {
    entry: ManuallyDrop<E>,
    _pad: [u64; 2],
}

#[repr(C)]
pub union Slot32Storage<E> {
    entry: ManuallyDrop<E>,
    _pad: [u64; 4],
}

#[repr(C)]
pub union Slot64Storage<E> {
    entry: ManuallyDrop<E>,
    _pad: [u64; 8],
}

impl SlotSize for Slot16 {
    const SIZE: usize = 16;
    type Storage<E> = Slot16Storage<E>;
}
impl SlotSize for Slot32 {
    const SIZE: usize = 32;
    type Storage<E> = Slot32Storage<E>;
}
impl SlotSize for Slot64 {
    const SIZE: usize = 64;
    type Storage<E> = Slot64Storage<E>;
}

// ---------------- Inner ----------------

struct Entry<K, V> {
    key: K,
    value: V,
}

/// Per-shard SIEVE state. Equivalent to j8's `Inner<K, V>` parameterized by `S`.
struct Inner<K, V, S: SlotSize> {
    capacity: usize,
    /// Parallel array #1: tag array. Size = `round_up(capacity, LANE).max(LANE)`.
    /// Under I4' there are never holes in `tags[0..len]`, so no slack past `capacity`
    /// is needed — the LANE-aligned remainder beyond `len` is permanent EMPTY pad.
    tags: Vec<u16>,
    /// Parallel array #2: entries arena. Size = `capacity` (no slack).
    /// Indexed by the 6-bit id embedded in each tag.
    /// `sizeof(S::Storage<Entry<K, V>>) == S::SIZE` is guaranteed by `_STORAGE_SIZE_OK`.
    entries: Vec<MaybeUninit<S::Storage<Entry<K, V>>>>,
    /// SIEVE hand cursor (`0..=len`), sweeping over `tags[0..len]`.
    hand: usize,
    /// Number of currently live entries (= number of live tags = first index past
    /// the live region in `tags`).
    len: usize,
}

impl<K, V, S: SlotSize> Inner<K, V, S> {
    /// Const-eval: `sizeof(Entry<K, V>) <= S::SIZE`.
    const _SIZE_OK: () = assert!(
        std::mem::size_of::<Entry<K, V>>() <= S::SIZE,
        "senba::Cache: sizeof(Entry<K, V>) exceeds the chosen SlotSize. \
         Try a larger SlotSize (e.g. Slot64)."
    );

    /// Const-eval: `sizeof(Storage<Entry>)` must equal `S::SIZE` exactly.
    /// If `Entry`'s alignment exceeds 8 (e.g. `repr(align(16))`), the union sizeof
    /// rounds up past `SLOT::SIZE`, breaking the c-hoist invariant
    /// (`tag & ID_MASK = id × S::SIZE`). This catches that at compile time.
    const _STORAGE_SIZE_OK: () = assert!(
        std::mem::size_of::<<S as SlotSize>::Storage<Entry<K, V>>>() == S::SIZE,
        "senba::Cache: SlotStorage size differs from SlotSize::SIZE. \
         (likely caused by Entry alignment > 8 byte)"
    );

    /// Bit position of the id field (6 bits) within a tag.
    /// Chosen as `log2(S::SIZE)` so that `id << ID_SHIFT == id × S::SIZE`.
    const ID_SHIFT: u32 = (S::SIZE as u32).trailing_zeros();
    /// Mask covering the id field. Invariant: `tag & ID_MASK == id × S::SIZE`
    /// (= byte offset into the entries arena).
    const ID_MASK: u16 = ((MAX_PER_SHARD - 1) as u16) << Self::ID_SHIFT;
    /// Mask covering the hash field. Always exactly 8 bits scattered:
    /// the non-status field is 14 bits (`0x3FFF`) and `ID_MASK` consumes 6 of them
    /// (because `MAX_PER_SHARD == 64`), leaving `14 - 6 = 8` bits for the hash regardless
    /// of which `SlotSize` is in use.
    const HASH_MASK: u16 = 0x3FFF & !Self::ID_MASK;
    /// Comparison target for SIMD scans: LIVE | HASH_MASK (visited and id are masked out).
    const SCAN_MASK: u16 = LIVE | Self::HASH_MASK;

    /// Extracts the id (0..MAX_PER_SHARD) from a tag. Used by scalar path, drop, and evict.
    #[inline]
    fn id_of(tag: u16) -> usize {
        ((tag & Self::ID_MASK) >> Self::ID_SHIFT) as usize
    }

    /// Raw pointer to the **`entry` field** of `entries[id]`.
    /// Because `#[repr(C)] union { entry: ManuallyDrop<E>, _pad: [u64; N] }` places
    /// the first field at offset 0, the `Storage<E>` pointer is the same as `*const E`.
    /// `MaybeUninit<T>` preserves this layout.
    #[inline]
    fn entry_ptr(&self, id: usize) -> *const Entry<K, V> {
        // Re-anchor the layout invariants at the use site, so that any future code
        // path that touches Entry through Storage (not just `Inner::new`) keeps the
        // const-eval guard active.
        let _: () = Self::_SIZE_OK;
        let _: () = Self::_STORAGE_SIZE_OK;
        self.entries[id].as_ptr() as *const Entry<K, V>
    }

    #[inline]
    fn entry_ptr_mut(&mut self, id: usize) -> *mut Entry<K, V> {
        let _: () = Self::_SIZE_OK;
        let _: () = Self::_STORAGE_SIZE_OK;
        self.entries[id].as_mut_ptr() as *mut Entry<K, V>
    }
}

impl<K, V, S: SlotSize> Inner<K, V, S>
where
    K: Hash + Eq,
{
    fn new(capacity: usize) -> Self {
        // Materialize const asserts (they are not evaluated unless referenced).
        let _: () = Self::_SIZE_OK;
        let _: () = Self::_STORAGE_SIZE_OK;

        assert!(capacity > 0, "capacity must be > 0");
        assert!(
            capacity <= MAX_PER_SHARD,
            "per-shard capacity ({capacity}) must be <= {MAX_PER_SHARD} (6-bit ID limit)"
        );
        // Under I4' the live region is exactly `tags[0..len]` (no scattered holes), so
        // `len <= capacity` and a tags array of `round_up(capacity, LANE).max(LANE)` is
        // both a sufficient upper bound and the SIMD-aligned scan window.
        let order_cap = ((capacity + LANE - 1) & !(LANE - 1)).max(LANE);
        let mut entries = Vec::with_capacity(capacity);
        entries.resize_with(capacity, MaybeUninit::uninit);
        Self {
            capacity,
            tags: vec![EMPTY; order_cap],
            entries,
            hand: 0,
            len: 0,
        }
    }

    /// Folds the top 8 bits of a 64-bit hash into the tag's hash field (same shape as j8).
    #[inline]
    fn needle_from_hash(hash: u64) -> u16 {
        let h = (hash >> 56) as u8;
        let s = Self::ID_SHIFT;
        let spread = if s >= 8 {
            h as u16
        } else {
            let low_mask: u8 = ((1u32 << s) - 1) as u8;
            let low = (h & low_mask) as u16;
            let high = ((h & !low_mask) as u16) << 6;
            low | high
        };
        LIVE | spread
    }

    fn find(&self, key: &K, needle: u16, has_avx2_bmi1: bool) -> Option<usize> {
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
    fn find_scalar(&self, key: &K, needle: u16) -> Option<usize> {
        for (i, &t) in self.tags[..self.len].iter().enumerate() {
            if (t & Self::SCAN_MASK) == needle {
                let id = Self::id_of(t);
                // SAFETY: a live tag implies entries[id] is initialized (I5/I6).
                let e = unsafe { &*self.entry_ptr(id) };
                if &e.key == key {
                    return Some(i);
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
    /// check; `Inner::find` performs it via the cached `has_avx2_bmi1` flag set in
    /// `Cache::new`.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_avx2(&self, key: &K, needle: u16) -> Option<usize> {
        // Re-anchor layout invariants at this use site (the c-hoist arithmetic below
        // assumes `sizeof(Storage<Entry>) == S::SIZE`, which `_STORAGE_SIZE_OK` enforces).
        let _: () = Self::_SIZE_OK;
        let _: () = Self::_STORAGE_SIZE_OK;
        use std::arch::x86_64::*;
        // Round `len` up to LANE. Tags beyond `len` are EMPTY (= 0) and the LIVE-bit
        // check would skip them anyway, but bounding the scan at the rounded-up live
        // region keeps the SIMD path competitive at low fill ratios.
        // `tags.len()` is itself LANE-aligned at construction, so this never exceeds it.
        let limit = (self.len + LANE - 1) & !(LANE - 1);
        debug_assert!(limit <= self.tags.len());
        let tags_ptr = self.tags.as_ptr();
        let tags_byte_ptr = tags_ptr as *const u8;
        // Hold entries as a byte pointer. sizeof(Storage<Entry>) == S::SIZE (fixed),
        // so `tag & ID_MASK` is directly the byte offset into the arena.
        // Storage's first field is entry at offset 0, so we reach Entry directly.
        let entries_byte_ptr = self.entries.as_ptr() as *const u8;
        let needle_v = _mm256_set1_epi16(needle as i16);
        let mask_v = _mm256_set1_epi16(Self::SCAN_MASK as i16);
        let id_mask_u32 = Self::ID_MASK as u32;

        let mut i = 0usize;
        while i < limit {
            let v = unsafe { _mm256_loadu_si256(tags_ptr.add(i) as *const __m256i) };
            let masked = _mm256_and_si256(v, mask_v);
            let cmp = _mm256_cmpeq_epi16(masked, needle_v);
            let mut mask = _mm256_movemask_epi8(cmp) as u32;

            let chunk_byte_ptr = unsafe { tags_byte_ptr.add(i * 2) };

            while mask != 0 {
                let bit = mask.trailing_zeros() as usize;
                let tag = unsafe { *(chunk_byte_ptr.add(bit) as *const u16) } as u32;
                let id_bytes = (tag & id_mask_u32) as usize;
                // SAFETY: live needle ⟹ tag live ⟹ entries[id] initialized (I6).
                // id_bytes = id × S::SIZE and id < capacity ⟹ in bounds.
                // Storage is #[repr(C)] with entry at offset 0 ⟹ Entry reachable directly.
                let entry_ptr = unsafe { entries_byte_ptr.add(id_bytes) as *const Entry<K, V> };
                let e = unsafe { &*entry_ptr };
                if &e.key == key {
                    let lane = bit >> 1;
                    return Some(i + lane);
                }
                mask = _blsr_u32(mask);
                mask = _blsr_u32(mask);
            }
            i += LANE;
        }
        None
    }

    fn contains(&self, key: &K, hash: u64, has_avx2_bmi1: bool) -> bool {
        self.find(key, Self::needle_from_hash(hash), has_avx2_bmi1)
            .is_some()
    }

    fn get(&mut self, key: &K, hash: u64, has_avx2_bmi1: bool) -> Option<&V> {
        let needle = Self::needle_from_hash(hash);
        let pos = self.find(key, needle, has_avx2_bmi1)?;
        self.tags[pos] |= VISITED;
        let id = Self::id_of(self.tags[pos]);
        // SAFETY: pos was confirmed live by find.
        let e = unsafe { &*self.entry_ptr(id) };
        Some(&e.value)
    }

    /// Non-promoting lookup. Same as `get` but does not set the VISITED bit,
    /// so peeked entries do not survive an extra SIEVE sweep.
    fn peek(&self, key: &K, hash: u64, has_avx2_bmi1: bool) -> Option<&V> {
        let needle = Self::needle_from_hash(hash);
        let pos = self.find(key, needle, has_avx2_bmi1)?;
        let id = Self::id_of(self.tags[pos]);
        // SAFETY: pos was confirmed live by find.
        let e = unsafe { &*self.entry_ptr(id) };
        Some(&e.value)
    }

    /// On hit: set VISITED and return `&value`. On miss: evaluate `f`, insert,
    /// and return `&value` of the freshly inserted entry. The new entry always
    /// lives at `tags[self.len - 1]` (steady-state evict path writes there;
    /// warm-up path post-increments `self.len`), which avoids a second `find`.
    fn get_or_insert_with<F>(&mut self, key: K, hash: u64, has_avx2_bmi1: bool, f: F) -> &V
    where
        F: FnOnce() -> V,
    {
        let needle = Self::needle_from_hash(hash);
        if let Some(pos) = self.find(&key, needle, has_avx2_bmi1) {
            self.tags[pos] |= VISITED;
            let id = Self::id_of(self.tags[pos]);
            // SAFETY: pos was confirmed live by find.
            let e = unsafe { &*self.entry_ptr(id) };
            return &e.value;
        }
        let value = f();
        let _evicted = self.insert(key, value, hash, has_avx2_bmi1);
        let pos = self.len - 1;
        let id = Self::id_of(self.tags[pos]);
        // SAFETY: insert just wrote a live tag at write_pos = self.len - 1
        // (warm-up: pos = old self.len then len += 1; evict: write_pos = last = len - 1).
        let e = unsafe { &*self.entry_ptr(id) };
        &e.value
    }

    fn insert(&mut self, key: K, value: V, hash: u64, has_avx2_bmi1: bool) -> Option<(K, V)> {
        let needle = Self::needle_from_hash(hash);
        if let Some(pos) = self.find(&key, needle, has_avx2_bmi1) {
            let id = Self::id_of(self.tags[pos]);
            // SAFETY: find confirmed the tag is live.
            let e = unsafe { &mut *self.entry_ptr_mut(id) };
            e.value = value;
            self.tags[pos] |= VISITED;
            return None;
        }

        // New entry. Warm-up extends the live region by one (`pos = self.len`);
        // steady state evicts at the SIEVE-chosen position, shifts the tail down,
        // and writes the new tag at `tags[len-1]` (the head end). Both paths end
        // with `tags[0..len]` contiguously LIVE, so no compaction is ever needed.
        let (evicted, write_pos, entry_id) = if self.len < self.capacity {
            let pos = self.len;
            let id = self.len as u16;
            self.len += 1;
            (None, pos, id)
        } else {
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

    /// SIEVE victim search over `tags[0..len]`. Two clearing passes (hand→len then
    /// 0→hand) cover the whole live region; if every tag was VISITED, both passes
    /// return None but every VISITED bit is now cleared, so any position is a
    /// valid victim — we pick `self.hand`.
    fn find_evict_pos(&mut self) -> usize {
        debug_assert!(self.len > 0 && self.len == self.capacity);
        if self.hand >= self.len {
            self.hand = 0;
        }
        self.scan_evict(self.hand, self.len)
            .or_else(|| self.scan_evict(0, self.hand))
            .unwrap_or(self.hand)
    }

    fn scan_evict(&mut self, lo: usize, hi: usize) -> Option<usize> {
        debug_assert!(lo <= hi && hi <= self.len);
        for i in lo..hi {
            let t = self.tags[i];
            // I4': tags[0..len] are all LIVE, so no EMPTY-skip branch needed.
            debug_assert!(t & LIVE != 0, "tags[0..len] must be LIVE under I4'");
            if t & VISITED != 0 {
                self.tags[i] = t & !VISITED;
            } else {
                return Some(i);
            }
        }
        None
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
    fn remove(&mut self, key: &K, hash: u64, has_avx2_bmi1: bool) -> Option<V> {
        let needle = Self::needle_from_hash(hash);
        let pos = self.find(key, needle, has_avx2_bmi1)?;
        let removed_id = Self::id_of(self.tags[pos]);

        // (1) Read Entry out of entries[removed_id]. After this, entries[removed_id]
        // is logically uninitialized.
        // SAFETY: live ⟹ entries[removed_id] initialized (I6).
        let entry = unsafe { std::ptr::read(self.entry_ptr(removed_id)) };

        // (2) Shift tags down, marking the new tail as EMPTY (preserves I4').
        // After this, the live region is `tags[0..self.len - 1]`.
        let last = self.len - 1;
        self.tags.copy_within(pos + 1..self.len, pos);
        self.tags[last] = EMPTY;
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
}

impl<K, V, S: SlotSize> Drop for Inner<K, V, S> {
    fn drop(&mut self) {
        // Enumerate live tags, extract their ids, and drop entries[id].
        // I4' (no holes) + I5 (unique ids) ⟹ no skip and no double-drop.
        for i in 0..self.len {
            let t = self.tags[i];
            debug_assert!(t & LIVE != 0, "tags[0..len] must be LIVE under I4'");
            let id = Self::id_of(t);
            // SAFETY: live ⟹ entries[id] initialized (I6).
            unsafe { std::ptr::drop_in_place(self.entry_ptr_mut(id)) };
        }
    }
}

// ---------------- Public type Cache ----------------

/// Publishable SIEVE cache. The entry stride is specified at the type level via `SlotSize`.
/// The number of shards is chosen at construction time from `capacity`
/// (see [`Cache::new`]); use [`Cache::with_shards`] to override.
///
/// ```
/// use senba::Cache;
///
/// // default Slot32: Entry<u64, String> (sizeof=32) fits exactly
/// let mut c: Cache<u64, String> = Cache::new(8);
/// c.insert(1, "hello".into());
/// assert_eq!(c.get(&1), Some(&"hello".to_string()));
/// assert_eq!(c.remove(&1), Some("hello".to_string()));
/// assert_eq!(c.get(&1), None);
/// ```
pub struct Cache<K, V, S: SlotSize = Slot32> {
    shards: Box<[Inner<K, V, S>]>,
    /// `shards.len() - 1`. Cached so `shard_of_hash` is a single AND.
    shard_mask: usize,
    hasher: Xxh3Build,
    /// AVX2 + BMI1 availability, resolved once in `new` so the SIMD dispatch in
    /// `Inner::find` is a single boolean load instead of a re-entry into
    /// `is_x86_feature_detected!` on every cache op. BMI1 is implied by AVX2 on
    /// every x86_64 CPU shipped to date, so detecting AVX2 suffices.
    has_avx2_bmi1: bool,
}

impl<K, V, S> Cache<K, V, S>
where
    K: Hash + Eq,
    S: SlotSize,
{
    /// Creates a cache with `capacity` total entries. The shard count is the
    /// smallest power of two `N` such that `ceil(capacity / N) <= MAX_PER_SHARD`,
    /// i.e. `N = next_pow2(ceil(capacity / MAX_PER_SHARD))` (clamped to ≥ 1).
    /// The 6-bit per-shard id field then accommodates every entry without
    /// further tuning.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        let n_min = capacity.div_ceil(MAX_PER_SHARD).max(1);
        let shards = n_min.next_power_of_two();
        Self::with_shards(capacity, shards)
    }

    /// Creates a cache with an explicit shard count. `shards` must be a power
    /// of two, `>= 1`, and small enough that `ceil(capacity / shards) <= MAX_PER_SHARD`
    /// holds. Mainly useful for benchmarking / oracle comparison; prefer
    /// [`Cache::new`] in production code.
    pub fn with_shards(capacity: usize, shards: usize) -> Self {
        assert!(shards > 0, "shards must be > 0");
        assert!(
            shards.is_power_of_two(),
            "shards ({shards}) must be a power of two so shard select can be a bit mask"
        );
        assert!(
            capacity >= shards,
            "capacity ({capacity}) must be >= shards ({shards}) so each shard has cap >= 1"
        );
        let base = capacity / shards;
        let extra = capacity % shards;
        let inners: Vec<Inner<K, V, S>> = (0..shards)
            .map(|i| {
                let cap_i = base + if i < extra { 1 } else { 0 };
                Inner::new(cap_i)
            })
            .collect();
        let has_avx2_bmi1 = {
            #[cfg(target_arch = "x86_64")]
            {
                std::is_x86_feature_detected!("avx2")
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                false
            }
        };
        Self {
            shards: inners.into_boxed_slice(),
            shard_mask: shards - 1,
            hasher: Xxh3Build,
            has_avx2_bmi1,
        }
    }

    pub fn capacity(&self) -> usize {
        self.shards.iter().map(|s| s.capacity).sum()
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.len == 0)
    }

    /// Number of shards in this cache (always a power of two).
    pub fn shards(&self) -> usize {
        self.shard_mask + 1
    }

    pub fn contains_key(&self, key: &K) -> bool {
        let h = self.hasher.hash_one(key);
        self.shards[self.shard_of_hash(h)].contains(key, h, self.has_avx2_bmi1)
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        let h = self.hasher.hash_one(key);
        let i = self.shard_of_hash(h);
        self.shards[i].get(key, h, self.has_avx2_bmi1)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        let h = self.hasher.hash_one(&key);
        let i = self.shard_of_hash(h);
        self.shards[i].insert(key, value, h, self.has_avx2_bmi1)
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let h = self.hasher.hash_one(key);
        let i = self.shard_of_hash(h);
        self.shards[i].remove(key, h, self.has_avx2_bmi1)
    }

    /// Non-promoting lookup: returns a reference to the value without setting
    /// the SIEVE VISITED bit. Use this when you want to inspect an entry
    /// without affecting its eviction priority.
    pub fn peek(&self, key: &K) -> Option<&V> {
        let h = self.hasher.hash_one(key);
        self.shards[self.shard_of_hash(h)].peek(key, h, self.has_avx2_bmi1)
    }

    /// Returns an iterator over `(&K, &V)` pairs across all shards.
    /// Iteration order is unspecified and may change between releases — SIEVE
    /// has no LRU/MRU concept, and shards are walked in `shard_of_hash` order.
    /// Iteration does not set VISITED bits, so it does not affect eviction.
    pub fn iter(&self) -> Iter<'_, K, V, S> {
        Iter {
            shards: &self.shards,
            shard_idx: 0,
            slot_idx: 0,
        }
    }

    /// Returns a reference to the value for `key`, or inserts the result of
    /// `f()` and returns a reference to it. The closure is only evaluated on
    /// a miss. On a hit, the entry's VISITED bit is set (same as `get`).
    /// Inserting may evict another entry; the evicted `(K, V)` is dropped
    /// (no listener API).
    pub fn get_or_insert_with<F>(&mut self, key: K, f: F) -> &V
    where
        F: FnOnce() -> V,
    {
        let h = self.hasher.hash_one(&key);
        let i = self.shard_of_hash(h);
        self.shards[i].get_or_insert_with(key, h, self.has_avx2_bmi1, f)
    }

    #[inline]
    fn shard_of_hash(&self, hash: u64) -> usize {
        (hash as usize) & self.shard_mask
    }
}

/// Iterator over a [`Cache`] yielding `(&K, &V)` pairs. Created by [`Cache::iter`].
pub struct Iter<'a, K, V, S: SlotSize> {
    shards: &'a [Inner<K, V, S>],
    shard_idx: usize,
    slot_idx: usize,
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
            let id = Inner::<K, V, S>::id_of(sh.tags[i]);
            // SAFETY: tags[0..len] are live (I4'), so entries[id] is initialized (I6).
            // The lifetime 'a comes from `shards: &'a [Inner<...>]`, so the returned
            // references stay valid for the iterator's lifetime.
            let e = unsafe { &*sh.entry_ptr(id) };
            return Some((&e.key, &e.value));
        }
    }
}

// `CacheImpl` intentionally does **not** expose `remove` (`is_empty` has a default
// impl on the trait). All sibling variants (sieve_orig, sieve_v*, sieve_j*) follow
// the same convention, so cross-variant bench / oracle drivers stay symmetric.
// `Cache::remove` is available on the inherent impl above when needed directly.
impl<K, V, S> crate::CacheImpl<K, V> for Cache<K, V, S>
where
    K: Hash + Eq,
    S: SlotSize,
{
    fn new(capacity: usize) -> Self {
        Self::new(capacity)
    }
    fn capacity(&self) -> usize {
        self.capacity()
    }
    fn len(&self) -> usize {
        self.len()
    }
    fn get(&mut self, key: &K) -> Option<&V> {
        self.get(key)
    }
    fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        self.insert(key, value)
    }
    fn contains_key(&self, key: &K) -> bool {
        self.contains_key(key)
    }
}

// ---------------- tests ----------------

#[cfg(test)]
mod tests;
