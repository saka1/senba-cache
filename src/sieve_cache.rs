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

    #[inline]
    fn shard_of_hash(&self, hash: u64) -> usize {
        (hash as usize) & self.shard_mask
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
mod tests {
    use super::*;

    /// Reference value used by tests that historically assumed the 8-shard default.
    /// The auto-shard policy in `Cache::new` may pick a different count (it depends
    /// on `capacity`), but every length / capacity assertion below treats this as
    /// "the multiplier we used to size the test cache" rather than as the actual
    /// shard count of the cache under test.
    const TEST_SHARDS: usize = 8;

    // sizeof(Entry<u64, u64>) = 16 → fits Slot16 / Slot32 / Slot64.
    // sizeof(Entry<i32, i32>) = 8  → fits all three (with slack).

    #[test]
    fn cache_initially_empty() {
        let cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.capacity(), TEST_SHARDS * 4);
        assert!(cache.is_empty());
    }

    #[test]
    fn insert_then_get() {
        let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        assert!(cache.insert(1, 10).is_none());
        assert_eq!(cache.get(&1), Some(&10));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn get_missing_returns_none() {
        let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        cache.insert(1, 10);
        assert_eq!(cache.get(&2), None);
    }

    #[test]
    fn contains_key_reflects_insertions() {
        let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        assert!(!cache.contains_key(&1));
        cache.insert(1, 10);
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
    }

    #[test]
    fn insert_existing_key_updates_value() {
        let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        cache.insert(1, 10);
        assert!(cache.insert(1, 20).is_none());
        assert_eq!(cache.get(&1), Some(&20));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn evicts_oldest_when_full_and_unvisited() {
        let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((1, 10)));
        assert_eq!(cache.len(), 2);
        assert!(!cache.contains_key(&1));
        assert!(cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }

    #[test]
    fn visited_entry_survives_first_pass() {
        let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((2, 20)));
    }

    #[test]
    fn all_visited_clears_bits_then_evicts() {
        let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        cache.get(&2);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((1, 10)));
    }

    #[test]
    fn total_capacity_is_respected_under_churn() {
        let cap = TEST_SHARDS * 16;
        let mut cache: Cache<u64, u64> = Cache::new(cap);
        for k in 0..10_000u64 {
            cache.insert(k, k);
            assert!(cache.len() <= cap);
        }
        assert_eq!(cache.len(), cap);
    }

    #[test]
    fn churn_keeps_a_full_capacity_set() {
        let cap = TEST_SHARDS * 16;
        let mut cache: Cache<u64, u64> = Cache::new(cap);
        for k in 0..50_000u64 {
            cache.insert(k, k * 3);
        }
        assert_eq!(cache.len(), cap);
        let mut alive = 0;
        for k in 0..50_000u64 {
            if cache.get(&k) == Some(&(k * 3)) {
                alive += 1;
            }
        }
        assert_eq!(alive, cap);
    }

    /// Verifies bit-field exclusivity for Slot32 (default, Entry<u64,u64>=16).
    /// Inner<u64, u64, Slot32>: ID_SHIFT = 5, ID_MASK = 0x07e0, HASH_MASK = 0x381f.
    #[test]
    fn bit_layout_exclusivity_slot32() {
        type I = Inner<u64, u64, Slot32>;
        assert_eq!(I::ID_SHIFT, 5);
        assert_eq!(I::ID_MASK, 0x07e0);
        assert_eq!(I::HASH_MASK, 0x381f);
        assert_eq!(I::SCAN_MASK, LIVE | I::HASH_MASK);
        assert_eq!(I::SCAN_MASK, 0xb81f);

        assert_eq!(LIVE | VISITED | I::ID_MASK | I::HASH_MASK, 0xFFFF);
        assert_eq!(LIVE & VISITED, 0);
        assert_eq!(LIVE & I::ID_MASK, 0);
        assert_eq!(LIVE & I::HASH_MASK, 0);
        assert_eq!(VISITED & I::ID_MASK, 0);
        assert_eq!(VISITED & I::HASH_MASK, 0);
        assert_eq!(I::ID_MASK & I::HASH_MASK, 0);

        // c-hoist invariant: embedding id into a tag gives `tag & ID_MASK = id × S::SIZE`.
        for id in 0..MAX_PER_SHARD {
            let tag_id_field = (id as u16) << I::ID_SHIFT;
            assert_eq!((tag_id_field & I::ID_MASK) as usize, id * Slot32::SIZE);
        }
    }

    #[test]
    fn bit_layout_slot16() {
        type I = Inner<u32, u32, Slot16>;
        assert_eq!(I::ID_SHIFT, 4);
        assert_eq!(I::ID_MASK, 0x03f0);
        assert_eq!(I::HASH_MASK, 0x3c0f);
    }

    #[test]
    fn bit_layout_slot64() {
        type I = Inner<u64, u64, Slot64>;
        assert_eq!(I::ID_SHIFT, 6);
        assert_eq!(I::ID_MASK, 0x0fc0);
        assert_eq!(I::HASH_MASK, 0x303f);
    }

    /// Hash spread injectivity across all three brackets.
    #[test]
    fn needle_spread_is_injective_all_slots() {
        for slot_id in 0..3 {
            let mut seen = std::collections::HashSet::new();
            for h in 0..=255u64 {
                let needle = match slot_id {
                    0 => Inner::<u64, u64, Slot16>::needle_from_hash(h << 56),
                    1 => Inner::<u64, u64, Slot32>::needle_from_hash(h << 56),
                    2 => Inner::<u64, u64, Slot64>::needle_from_hash(h << 56),
                    _ => unreachable!(),
                };
                assert!(seen.insert(needle), "slot {slot_id} hash {h} collides");
            }
            assert_eq!(seen.len(), 256);
        }
    }

    #[test]
    fn slot16_small_entry() {
        // sizeof(Entry<u32, u32>) = 8 ≤ 16
        let mut c: Cache<u32, u32, Slot16> = Cache::new(TEST_SHARDS * 4);
        for k in 0..100u32 {
            c.insert(k, k * 7);
        }
        assert_eq!(c.len(), TEST_SHARDS * 4);
    }

    #[test]
    fn slot32_default_string_value() {
        // sizeof(Entry<u64, String>) = 32 (8 + 24)
        let mut c: Cache<u64, String> = Cache::new(TEST_SHARDS * 2);
        for k in 0..40u64 {
            c.insert(k, format!("v{k}"));
        }
        assert_eq!(c.len(), TEST_SHARDS * 2);
    }

    #[test]
    fn slot64_string_string() {
        // sizeof(Entry<String, String>) = 48 ≤ 64
        let cap = TEST_SHARDS * 2;
        let mut c: Cache<String, String, Slot64> = Cache::new(cap);
        for k in 0..200u64 {
            c.insert(format!("k{k}"), format!("v{k}"));
        }
        assert_eq!(c.len(), cap);
        // Recently inserted keys should survive (SIEVE selects within each shard).
        let alive = (0..200u64)
            .filter(|k| c.get(&format!("k{k}")) == Some(&format!("v{k}")))
            .count();
        assert_eq!(alive, cap);
    }

    #[test]
    fn remove_basic() {
        let mut c: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        c.insert(1, 100);
        c.insert(2, 200);
        c.insert(3, 300);
        assert_eq!(c.remove(&2), Some(200));
        assert_eq!(c.get(&2), None);
        assert_eq!(c.get(&1), Some(&100));
        assert_eq!(c.get(&3), Some(&300));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn remove_missing_returns_none() {
        let mut c: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        c.insert(1, 100);
        assert_eq!(c.remove(&999), None);
        assert_eq!(c.len(), 1);
    }

    /// After remove, I8 (live ids = 0..len) must be restored so that
    /// the warm-up branch (`entry_id = self.len`) works correctly on the next insert.
    #[test]
    fn remove_then_insert_reuses_id() {
        let mut c: Cache<u64, u64, Slot32> = Cache::with_shards(4, 1);
        c.insert(1, 100);
        c.insert(2, 200);
        c.insert(3, 300);
        c.insert(4, 400);
        assert_eq!(c.len(), 4);

        // remove reduces len to 3; swap-to-fill-gap restores I8.
        assert_eq!(c.remove(&2), Some(200));
        assert_eq!(c.len(), 3);

        // Insert a 5th entry via the warm-up branch (no eviction expected).
        assert_eq!(c.insert(5, 500), None);
        assert_eq!(c.len(), 4);

        // 1, 3, 4, 5 are live; 2 is gone.
        assert_eq!(c.get(&1), Some(&100));
        assert_eq!(c.get(&2), None);
        assert_eq!(c.get(&3), Some(&300));
        assert_eq!(c.get(&4), Some(&400));
        assert_eq!(c.get(&5), Some(&500));
    }

    /// Removing the entry with the maximum id (no swap needed).
    #[test]
    fn remove_max_id_no_swap() {
        let mut c: Cache<u64, u64, Slot32> = Cache::with_shards(4, 1);
        c.insert(1, 100);
        c.insert(2, 200);
        c.insert(3, 300);
        // With warm-up ordering, key 3 gets id=2 (the max).
        assert_eq!(c.remove(&3), Some(300));
        assert_eq!(c.len(), 2);
        assert_eq!(c.get(&1), Some(&100));
        assert_eq!(c.get(&2), Some(&200));
        assert_eq!(c.get(&3), None);
    }

    /// Repeated remove → insert cycles must not corrupt state.
    #[test]
    fn remove_insert_churn() {
        let mut c: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        for k in 0..100u64 {
            c.insert(k, k * 11);
        }
        // Remove all even keys.
        for k in (0..100u64).step_by(2) {
            let _ = c.remove(&k);
        }
        // Only odd keys may remain (up to capacity).
        let alive: usize = (1..100u64)
            .step_by(2)
            .filter(|k| c.get(k) == Some(&(k * 11)))
            .count();
        assert!(alive > 0);
        // New inserts must succeed.
        for k in 200..220u64 {
            c.insert(k, k);
        }
        assert!(c.len() <= TEST_SHARDS * 4);
    }

    /// Cross-checks insert/get behavior against sieve_orig (oracle) with a single shard.
    #[test]
    fn matches_sieve_orig_externally_1shard() {
        use crate::sieve_orig::SieveCache as Orig;
        let cap = 64usize;
        let mut a: Orig<u64, u64> = Orig::new(cap);
        let mut b: Cache<u64, u64, Slot32> = Cache::with_shards(cap, 1);
        for k in 0..10_000u64 {
            let key = (k.wrapping_mul(2654435761)) % 256;
            let _ = a.insert(key, key);
            let _ = b.insert(key, key);
        }
        for k in 0..256u64 {
            assert_eq!(
                a.get(&k).copied(),
                b.get(&k).copied(),
                "1-shard mismatch with sieve_orig at key {k}"
            );
        }
    }

    /// All three brackets (Slot16/32/64) must match sieve_orig semantics for Entry<u64,u64>.
    #[test]
    fn matches_sieve_orig_per_slot() {
        use crate::sieve_orig::SieveCache as Orig;
        let cap = 32usize;
        let mut oracle: Orig<u64, u64> = Orig::new(cap);
        let mut s16: Cache<u64, u64, Slot16> = Cache::with_shards(cap, 1);
        let mut s32: Cache<u64, u64, Slot32> = Cache::with_shards(cap, 1);
        let mut s64: Cache<u64, u64, Slot64> = Cache::with_shards(cap, 1);
        for k in 0..5_000u64 {
            let key = (k.wrapping_mul(2654435761)) % 128;
            oracle.insert(key, key);
            s16.insert(key, key);
            s32.insert(key, key);
            s64.insert(key, key);
        }
        for k in 0..128u64 {
            let g = oracle.get(&k).copied();
            assert_eq!(s16.get(&k).copied(), g, "Slot16 mismatch key={k}");
            assert_eq!(s32.get(&k).copied(), g, "Slot32 mismatch key={k}");
            assert_eq!(s64.get(&k).copied(), g, "Slot64 mismatch key={k}");
        }
    }

    /// Cross-checks remove behavior against sieve_orig with interleaved operations.
    #[test]
    fn remove_during_churn_oracle_match() {
        use crate::sieve_orig::SieveCache as Orig;
        let cap = 32usize;
        let mut a: Orig<u64, u64> = Orig::new(cap);
        let mut b: Cache<u64, u64, Slot32> = Cache::with_shards(cap, 1);
        for k in 0..3_000u64 {
            let key = (k.wrapping_mul(2654435761)) % 128;
            let ai = a.insert(key, key);
            let bi = b.insert(key, key);
            assert_eq!(ai, bi, "insert eviction mismatch step={k} key={key}");
            if k % 5 == 0 {
                let rk = (k.wrapping_mul(11400714819323198485)) % 128;
                let ar = a.remove(&rk);
                let br = b.remove(&rk);
                assert_eq!(ar, br, "remove mismatch step={k} key={rk}");
            }
        }
        for k in 0..128u64 {
            assert_eq!(
                a.get(&k).copied(),
                b.get(&k).copied(),
                "oracle mismatch key={k}"
            );
        }
    }

    #[test]
    #[should_panic]
    fn capacity_below_shards_panics() {
        // Auto-`new` would happily build a 1-shard cache here, so only the
        // explicit `with_shards` path enforces this invariant now.
        let _: Cache<u64, u64> = Cache::with_shards(TEST_SHARDS - 1, TEST_SHARDS);
    }

    #[test]
    #[should_panic]
    fn zero_capacity_panics() {
        let _: Cache<u64, u64> = Cache::new(0);
    }

    /// `Cache::new` must pick a shard count consistent with `MAX_PER_SHARD = 64`.
    #[test]
    fn auto_shards_match_capacity_brackets() {
        let cases: &[(usize, usize)] = &[
            (1, 1),
            (64, 1),
            (65, 2),
            (128, 2),
            (129, 4),
            (512, 8),
            (513, 16),
        ];
        for &(cap, expected_shards) in cases {
            let c: Cache<u64, u64> = Cache::new(cap);
            assert_eq!(
                c.shards(),
                expected_shards,
                "auto-shards mismatch at capacity={cap}"
            );
            assert_eq!(c.capacity(), cap);
        }
    }

    #[test]
    #[should_panic]
    fn per_shard_above_max_panics() {
        let _: Cache<u64, u64, Slot32> = Cache::with_shards(65, 1);
    }

    /// Regression test for the `find_avx2` `limit = round_up(tail, LANE)` change.
    /// At low fill ratios `tail << tags.len()`, so the SIMD path's bound diverges
    /// from the scalar path. Both must still agree with `sieve_orig` on every key,
    /// covering misses (no key in tags) and hits (key present at small `tail`).
    #[test]
    fn find_avx2_low_fill_matches_oracle() {
        use crate::sieve_orig::SieveCache as Orig;
        // capacity 64 → order_cap = round_up(128, 16) = 128. With only a handful of
        // keys inserted, `tail` stays well below `tags.len()` and the SIMD upper
        // bound differs sharply from the old `tags.len()` bound.
        let cap = 64usize;
        let mut a: Orig<u64, u64> = Orig::new(cap);
        let mut b: Cache<u64, u64, Slot32> = Cache::with_shards(cap, 1);
        // Keep fill ratio well below capacity throughout so `tail` stays small.
        for k in 0..8u64 {
            a.insert(k, k * 7);
            b.insert(k, k * 7);
        }
        // Probe absent keys (forces the scan to traverse the full `tail` window
        // without an early hit) and present keys (verifies hits via the SIMD path).
        for k in 0..256u64 {
            assert_eq!(
                a.get(&k).copied(),
                b.get(&k).copied(),
                "low-fill SIMD bound regression at key {k}"
            );
        }
    }

    /// Verifies cross-shard counts of dropped entries — catches regressions where
    /// `swap-to-fill-gap` or `Drop` would double-drop or leak. Uses an explicit
    /// drop counter (the existing `drop_runs_for_live_entries_only` test relies on
    /// `String` only as a smoke test and does not assert anything observable).
    #[test]
    fn drop_count_matches_inserts_minus_evictions() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Bomb {
            ctr: Arc<AtomicUsize>,
        }
        impl Drop for Bomb {
            fn drop(&mut self) {
                self.ctr.fetch_add(1, Ordering::Relaxed);
            }
        }

        let drops = Arc::new(AtomicUsize::new(0));
        let cap = TEST_SHARDS * 4;
        let mut evicted = 0usize;
        let mut explicit_removed = 0usize;
        {
            let mut cache: Cache<u64, Bomb> = Cache::new(cap);
            // 1) Insert N > cap distinct keys → triggers evictions, each dropped here.
            for k in 0..(cap as u64 * 3) {
                if let Some((_k, _v)) = cache.insert(k, Bomb { ctr: drops.clone() }) {
                    // Evicted Bomb drops at end of this statement.
                    evicted += 1;
                }
            }
            // 2) Explicit removes also drop their Bomb on the returned-Option drop.
            for k in 0..(cap as u64 / 2) {
                if cache.remove(&k).is_some() {
                    explicit_removed += 1;
                }
            }
            // Cache drop happens at end of scope; remaining live Bombs drop there.
        }
        let total_inserted = cap as u64 * 3;
        assert_eq!(
            drops.load(Ordering::Relaxed) as u64,
            total_inserted,
            "expected each inserted Bomb to drop exactly once \
             (evicted {evicted}, explicit_removed {explicit_removed}, total {total_inserted})"
        );
    }

    #[test]
    fn drop_runs_for_live_entries_only() {
        // String values exercise drop correctness (no double-drop, no leak).
        let mut cache: Cache<u64, String> = Cache::new(TEST_SHARDS * 2);
        for k in 0..64u64 {
            cache.insert(k, format!("value-{k}"));
        }
        assert_eq!(cache.len(), TEST_SHARDS * 2);
        // remove also exercises the drop path.
        for k in 0..16u64 {
            let _ = cache.remove(&k);
        }
        // Remaining entries are dropped when Cache goes out of scope.
    }
}
