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
use std::borrow::Borrow;
use std::fmt;
use std::hash::{BuildHasher, Hash};
use std::marker::PhantomData;
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
    /// Per-shard observability counters. Plain `u64` rather than `AtomicU64`
    /// because every mutating op already requires `&mut self`; on x86 a plain
    /// `add [mem], 1` is one uop on a dependency chain disjoint from the
    /// returned value, so OoO retires it for free in steady state.
    hits: u64,
    misses: u64,
    insertions: u64,
    evictions: u64,
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
            hits: 0,
            misses: 0,
            insertions: 0,
            evictions: 0,
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

    fn find<Q>(&self, key: &Q, needle: u16, has_avx2_bmi1: bool) -> Option<usize>
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
    fn find_scalar<Q>(&self, key: &Q, needle: u16) -> Option<usize>
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
    unsafe fn find_avx2<Q>(&self, key: &Q, needle: u16) -> Option<usize>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
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
                if e.key.borrow() == key {
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

    fn contains<Q>(&self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> bool
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        self.find(key, Self::needle_from_hash(hash), has_avx2_bmi1)
            .is_some()
    }

    fn get<Q>(&mut self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let needle = Self::needle_from_hash(hash);
        let pos = match self.find(key, needle, has_avx2_bmi1) {
            Some(p) => {
                self.hits += 1;
                p
            }
            None => {
                self.misses += 1;
                return None;
            }
        };
        self.tags[pos] |= VISITED;
        let id = Self::id_of(self.tags[pos]);
        // SAFETY: pos was confirmed live by find.
        let e = unsafe { &*self.entry_ptr(id) };
        Some(&e.value)
    }

    fn get_mut<Q>(&mut self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let needle = Self::needle_from_hash(hash);
        let pos = match self.find(key, needle, has_avx2_bmi1) {
            Some(p) => {
                self.hits += 1;
                p
            }
            None => {
                self.misses += 1;
                return None;
            }
        };
        self.tags[pos] |= VISITED;
        let id = Self::id_of(self.tags[pos]);
        // SAFETY: pos was confirmed live by find; the &mut self borrow makes the
        // returned &mut V the only outstanding borrow into entries[id].
        let e = unsafe { &mut *self.entry_ptr_mut(id) };
        Some(&mut e.value)
    }

    /// Non-promoting lookup. Same as `get` but does not set the VISITED bit,
    /// so peeked entries do not survive an extra SIEVE sweep.
    fn peek<Q>(&self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let needle = Self::needle_from_hash(hash);
        let pos = self.find(key, needle, has_avx2_bmi1)?;
        let id = Self::id_of(self.tags[pos]);
        // SAFETY: pos was confirmed live by find.
        let e = unsafe { &*self.entry_ptr(id) };
        Some(&e.value)
    }

    /// Non-promoting `&mut V` lookup. Like `get_mut` but does not set VISITED.
    fn peek_mut<Q>(&mut self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let needle = Self::needle_from_hash(hash);
        let pos = self.find(key, needle, has_avx2_bmi1)?;
        let id = Self::id_of(self.tags[pos]);
        // SAFETY: pos was confirmed live by find; the &mut self borrow makes the
        // returned &mut V the only outstanding borrow into entries[id].
        let e = unsafe { &mut *self.entry_ptr_mut(id) };
        Some(&mut e.value)
    }

    /// Promoting lookup that returns `(&K, &V)`. Sets VISITED on hit (same
    /// as `get`).
    fn get_key_value<Q>(&mut self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let needle = Self::needle_from_hash(hash);
        let pos = match self.find(key, needle, has_avx2_bmi1) {
            Some(p) => {
                self.hits += 1;
                p
            }
            None => {
                self.misses += 1;
                return None;
            }
        };
        self.tags[pos] |= VISITED;
        let id = Self::id_of(self.tags[pos]);
        // SAFETY: pos was confirmed live by find.
        let e = unsafe { &*self.entry_ptr(id) };
        Some((&e.key, &e.value))
    }

    /// Non-promoting variant of `get_key_value`.
    fn peek_key_value<Q>(&self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let needle = Self::needle_from_hash(hash);
        let pos = self.find(key, needle, has_avx2_bmi1)?;
        let id = Self::id_of(self.tags[pos]);
        // SAFETY: pos was confirmed live by find.
        let e = unsafe { &*self.entry_ptr(id) };
        Some((&e.key, &e.value))
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
            self.hits += 1;
            self.tags[pos] |= VISITED;
            let id = Self::id_of(self.tags[pos]);
            // SAFETY: pos was confirmed live by find.
            let e = unsafe { &*self.entry_ptr(id) };
            return &e.value;
        }
        self.misses += 1;
        let value = f();
        // `insert` increments `insertions` (and `evictions` if it overflows
        // capacity). The miss path never hits the replace branch, so this
        // does not double-count.
        let _evicted = self.insert(key, value, hash, has_avx2_bmi1);
        let pos = self.len - 1;
        let id = Self::id_of(self.tags[pos]);
        // SAFETY: insert just wrote a live tag at write_pos = self.len - 1
        // (warm-up: pos = old self.len then len += 1; evict: write_pos = last = len - 1).
        let e = unsafe { &*self.entry_ptr(id) };
        &e.value
    }

    fn insert(&mut self, key: K, value: V, hash: u64, has_avx2_bmi1: bool) -> Option<(K, V)> {
        self.insertions += 1;
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
    fn remove<Q>(&mut self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
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

    /// Filters entries in place via `f(&K, &mut V) -> bool`. Single-pass
    /// compaction over `tags[0..len]` plus a bitmap-based id remap to restore I8
    /// (live ids = `0..len`). Avoids the per-deletion `find` + memmove +
    /// id-swap that a naive `iter`+`remove` loop would pay (`O(k·n)` → `O(n)`
    /// with the per-shard `n ≤ MAX_PER_SHARD` cap making the id remap a fixed
    /// `≤ 64×64` bitscan).
    fn retain<F>(&mut self, f: &mut F)
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
            inner: &'a mut Inner<K, V, S>,
        }
        impl<K, V, S: SlotSize> Drop for Guard<'_, K, V, S> {
            fn drop(&mut self) {
                for i in 0..self.inner.tags.len() {
                    let t = self.inner.tags[i];
                    if t & LIVE != 0 {
                        let id = Inner::<K, V, S>::id_of(t);
                        // SAFETY: LIVE bit ⟹ entries[id] initialized (I6).
                        unsafe { std::ptr::drop_in_place(self.inner.entry_ptr_mut(id)) };
                        self.inner.tags[i] = EMPTY;
                    }
                }
                self.inner.len = 0;
                self.inner.hand = 0;
            }
        }
        let guard = Guard { inner: self };
        let inner = &mut *guard.inner;

        // Pass 1: walk tags[0..old_len], decide keep/drop, compact survivors
        // into tags[0..write] in place. Each iteration is structured so that
        // if `f` panics, the only mutation already committed is to the slot
        // currently being read — and that slot is left in a state the guard's
        // Drop can clean up uniformly (LIVE tag ⟹ initialized entry).
        let mut write = 0usize;
        let mut drops_before_hand = 0usize;
        for read in 0..old_len {
            let t = inner.tags[read];
            debug_assert!(t & LIVE != 0, "tags[0..len] must be LIVE under I4'");
            let id = Self::id_of(t);
            // SAFETY: LIVE ⟹ entries[id] initialized (I6). The closure receives
            // &K and &mut V via raw-pointer reborrow; nothing else aliases.
            let keep = unsafe {
                let p = inner.entry_ptr_mut(id);
                f(&(*p).key, &mut (*p).value)
            };
            if keep {
                if write != read {
                    inner.tags[write] = t;
                    inner.tags[read] = EMPTY;
                }
                write += 1;
            } else {
                // Zero the tag *before* dropping so an intervening panic in
                // Drop (rare but possible) can't leave a stale LIVE tag
                // pointing at an uninitialized slot.
                inner.tags[read] = EMPTY;
                // SAFETY: LIVE ⟹ entries[id] initialized (I6). Tag has just
                // been cleared so the guard's cleanup will not visit this id.
                unsafe { std::ptr::drop_in_place(inner.entry_ptr_mut(id)) };
                if read < old_hand {
                    drops_before_hand += 1;
                }
            }
        }

        // Commit new len + hand. The remaining tags[write..old_len] are
        // already EMPTY (every iteration that increased write zeroed read,
        // every drop iteration zeroed read), so I4' is preserved.
        inner.len = write;
        inner.hand = if old_hand >= old_len {
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
                occupied |= 1u64 << Self::id_of(inner.tags[i]);
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
                    let v = std::ptr::read(inner.entry_ptr(h_id));
                    std::ptr::write(inner.entry_ptr_mut(l_id), v);
                }
                let mut found = false;
                for i in 0..write {
                    let t = inner.tags[i];
                    if Self::id_of(t) == h_id {
                        let cleared = t & !Self::ID_MASK;
                        let new_id_field = (l_id as u16) << Self::ID_SHIFT;
                        inner.tags[i] = cleared | new_id_field;
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

        // Disarm the panic guard; the borrow of `inner` ends here.
        std::mem::forget(guard);
    }

    /// Drops every live entry and resets the shard to empty. Tags in `tags[0..len]`
    /// are zeroed back to EMPTY (the slack `tags[len..]` is already EMPTY under I4').
    fn clear(&mut self) {
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
    }
}

impl<K, V, S> Clone for Inner<K, V, S>
where
    K: Hash + Eq + Clone,
    V: Clone,
    S: SlotSize,
{
    fn clone(&self) -> Self {
        // Clone every live (key, value) into an owned Vec first. If any user
        // Clone impl panics partway, the partial Vec drops cleanly and we
        // never touch the destination Inner — so the destination cannot end
        // up with a LIVE tag pointing at an uninitialized slot.
        let mut cloned: Vec<(usize, Entry<K, V>)> = Vec::with_capacity(self.len);
        for i in 0..self.len {
            let t = self.tags[i];
            debug_assert!(t & LIVE != 0, "tags[0..len] must be LIVE under I4'");
            let id = Self::id_of(t);
            // SAFETY: live ⟹ entries[id] initialized (I6).
            let src = unsafe { &*self.entry_ptr(id) };
            cloned.push((
                id,
                Entry {
                    key: src.key.clone(),
                    value: src.value.clone(),
                },
            ));
        }

        let mut new = Inner::<K, V, S>::new(self.capacity);
        // tags arrays have identical length (both = round_up(capacity, LANE).max(LANE)).
        new.tags.copy_from_slice(&self.tags);
        new.hand = self.hand;
        new.len = self.len;
        new.hits = self.hits;
        new.misses = self.misses;
        new.insertions = self.insertions;
        new.evictions = self.evictions;
        for (id, entry) in cloned {
            // SAFETY: id matches a LIVE tag in the freshly-built `new`; the
            // slot was MaybeUninit::uninit and no other write has touched it.
            unsafe {
                std::ptr::write(new.entry_ptr_mut(id), entry);
            }
        }
        new
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

// ---------------- Stats ----------------

/// Lifetime counters for a [`Cache`]. Returned by [`Cache::stats`].
///
/// All fields are monotonically increasing across the lifetime of the cache.
/// Counts are aggregated across all shards at call time.
///
/// Semantics:
///
/// - `hits` / `misses` count **promoting** lookups only — `get`, `get_mut`,
///   `get_key_value`, and the lookup half of `get_or_insert_with`. Probes
///   that do not affect SIEVE eviction (`peek*`, `contains_key`) and the
///   internal `find` calls inside `insert` are intentionally excluded so
///   that `hits + misses` equals the number of user-facing lookup ops.
/// - `insertions` counts every successful call to [`Cache::insert`] (both
///   replace and new-entry paths) plus the miss-path insert inside
///   [`Cache::get_or_insert_with`].
/// - `evictions` counts only **capacity-driven** evictions inside `insert`.
///   Explicit removals (`remove`, `clear`, `retain`) do not increment it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub hits: u64,
    pub misses: u64,
    pub insertions: u64,
    pub evictions: u64,
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
pub struct Cache<K, V, S: SlotSize = Slot32, H: BuildHasher = Xxh3Build> {
    shards: Box<[Inner<K, V, S>]>,
    /// `shards.len() - 1`. Cached so `shard_of_hash` is a single AND.
    shard_mask: usize,
    hasher: H,
    /// AVX2 + BMI1 availability, resolved once in `new` so the SIMD dispatch in
    /// `Inner::find` is a single boolean load instead of a re-entry into
    /// `is_x86_feature_detected!` on every cache op. BMI1 is implied by AVX2 on
    /// every x86_64 CPU shipped to date, so detecting AVX2 suffices.
    has_avx2_bmi1: bool,
}

impl<K, V, S> Cache<K, V, S, Xxh3Build>
where
    K: Hash + Eq,
    S: SlotSize,
{
    /// Creates a cache with `capacity` total entries and the default
    /// [`Xxh3Build`] hasher. The shard count is the smallest power of two `N`
    /// such that `ceil(capacity / N) <= MAX_PER_SHARD`, i.e.
    /// `N = next_pow2(ceil(capacity / MAX_PER_SHARD))` (clamped to ≥ 1).
    /// The 6-bit per-shard id field then accommodates every entry without
    /// further tuning.
    pub fn new(capacity: usize) -> Self {
        Self::with_hasher(capacity, Xxh3Build)
    }

    /// Creates a cache with an explicit shard count and the default
    /// [`Xxh3Build`] hasher. `shards` must be a power of two, `>= 1`, and
    /// small enough that `ceil(capacity / shards) <= MAX_PER_SHARD` holds.
    /// Mainly useful for benchmarking / oracle comparison; prefer
    /// [`Cache::new`] in production code.
    pub fn with_shards(capacity: usize, shards: usize) -> Self {
        Self::with_shards_and_hasher(capacity, shards, Xxh3Build)
    }
}

impl<K, V, S, H> Cache<K, V, S, H>
where
    K: Hash + Eq,
    S: SlotSize,
    H: BuildHasher,
{
    /// Creates a cache with `capacity` total entries and the supplied
    /// [`BuildHasher`]. Auto-shards as in [`Cache::new`].
    pub fn with_hasher(capacity: usize, hasher: H) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        let n_min = capacity.div_ceil(MAX_PER_SHARD).max(1);
        let shards = n_min.next_power_of_two();
        Self::with_shards_and_hasher(capacity, shards, hasher)
    }

    /// Creates a cache with an explicit shard count and the supplied
    /// [`BuildHasher`]. `shards` must be a power of two, `>= 1`, and small
    /// enough that `ceil(capacity / shards) <= MAX_PER_SHARD` holds.
    pub fn with_shards_and_hasher(capacity: usize, shards: usize, hasher: H) -> Self {
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
            hasher,
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

    /// Returns aggregated [`Stats`] counters across every shard. See the
    /// [`Stats`] doc for what each field counts.
    pub fn stats(&self) -> Stats {
        let mut s = Stats::default();
        for sh in self.shards.iter() {
            s.hits += sh.hits;
            s.misses += sh.misses;
            s.insertions += sh.insertions;
            s.evictions += sh.evictions;
        }
        s
    }

    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        self.shards[self.shard_of_hash(h)].contains(key, h, self.has_avx2_bmi1)
    }

    pub fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        let i = self.shard_of_hash(h);
        self.shards[i].get(key, h, self.has_avx2_bmi1)
    }

    /// Returns a mutable reference to the value for `key`. Sets the SIEVE
    /// VISITED bit on hit (same as `get`), so in-place updates count as
    /// access for eviction purposes.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        let i = self.shard_of_hash(h);
        self.shards[i].get_mut(key, h, self.has_avx2_bmi1)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        let h = self.hasher.hash_one(&key);
        let i = self.shard_of_hash(h);
        self.shards[i].insert(key, value, h, self.has_avx2_bmi1)
    }

    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        let i = self.shard_of_hash(h);
        self.shards[i].remove(key, h, self.has_avx2_bmi1)
    }

    /// Non-promoting lookup: returns a reference to the value without setting
    /// the SIEVE VISITED bit. Use this when you want to inspect an entry
    /// without affecting its eviction priority.
    pub fn peek<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        self.shards[self.shard_of_hash(h)].peek(key, h, self.has_avx2_bmi1)
    }

    /// Non-promoting `&mut V` lookup. Same as `get_mut` but does not set
    /// VISITED, so in-place updates do not affect SIEVE eviction priority.
    /// Useful for housekeeping writes (counters, timestamps) that should not
    /// count as logical access.
    pub fn peek_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        let i = self.shard_of_hash(h);
        self.shards[i].peek_mut(key, h, self.has_avx2_bmi1)
    }

    /// Like `get`, but also returns a reference to the stored key. Sets
    /// VISITED on hit. Useful when looking up via `Borrow<Q>` and the
    /// canonical `&K` is wanted (e.g. `Cache<String, V>` looked up with
    /// `&str`).
    pub fn get_key_value<Q>(&mut self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        let i = self.shard_of_hash(h);
        self.shards[i].get_key_value(key, h, self.has_avx2_bmi1)
    }

    /// Non-promoting variant of `get_key_value`: returns `(&K, &V)` without
    /// setting VISITED.
    pub fn peek_key_value<Q>(&self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        self.shards[self.shard_of_hash(h)].peek_key_value(key, h, self.has_avx2_bmi1)
    }

    /// Drops every entry, leaving the cache empty. Capacity, shard layout,
    /// and the SIEVE hand are reset; subsequent inserts behave as if on a
    /// freshly constructed cache.
    pub fn clear(&mut self) {
        for sh in self.shards.iter_mut() {
            sh.clear();
        }
    }

    /// Retains only the entries for which `f(&k, &mut v)` returns `true`.
    /// Order of visitation is unspecified. Survivors keep their VISITED state
    /// — `retain` is a non-promoting maintenance operation and does not
    /// affect SIEVE eviction priority for the entries it leaves behind
    /// (mirrors `iter` / `peek`). If `f` panics the cache is left empty but
    /// in a consistent state; the panic resumes after cleanup.
    ///
    /// Linear in the number of live entries: a single in-place compaction
    /// pass per shard, with no per-deletion hash lookup or memmove (unlike
    /// calling `remove` in a loop, which is `O(k·n)` per shard).
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&K, &mut V) -> bool,
    {
        for sh in self.shards.iter_mut() {
            sh.retain(&mut f);
        }
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

    /// Returns an iterator over `(&K, &mut V)` pairs across all shards.
    /// Iteration order matches [`Cache::iter`] and is non-promoting (no
    /// VISITED bit is set on visited entries). Mutating values through this
    /// iterator does not change SIEVE eviction priority.
    pub fn iter_mut(&mut self) -> IterMut<'_, K, V, S> {
        let n = self.shards.len();
        IterMut {
            shards: self.shards.as_mut_ptr(),
            n_shards: n,
            shard_idx: 0,
            slot_idx: 0,
            _marker: PhantomData,
        }
    }

    /// Returns an iterator over `&K` for every live entry. Same order and
    /// non-promoting semantics as [`Cache::iter`].
    pub fn keys(&self) -> Keys<'_, K, V, S> {
        Keys { iter: self.iter() }
    }

    /// Returns an iterator over `&V` for every live entry. Same order and
    /// non-promoting semantics as [`Cache::iter`].
    pub fn values(&self) -> Values<'_, K, V, S> {
        Values { iter: self.iter() }
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

impl<K, V, S, H> Clone for Cache<K, V, S, H>
where
    K: Hash + Eq + Clone,
    V: Clone,
    S: SlotSize,
    H: BuildHasher + Clone,
{
    fn clone(&self) -> Self {
        Self {
            shards: self.shards.to_vec().into_boxed_slice(),
            shard_mask: self.shard_mask,
            hasher: self.hasher.clone(),
            has_avx2_bmi1: self.has_avx2_bmi1,
        }
    }
}

impl<K, V, S, H> fmt::Debug for Cache<K, V, S, H>
where
    K: fmt::Debug + Hash + Eq,
    V: fmt::Debug,
    S: SlotSize,
    H: BuildHasher,
{
    /// Renders the cache as a map of its current entries plus capacity / shard
    /// metadata. Iteration order is unspecified (`Cache::iter` order); the
    /// VISITED bit is **not** set, so `Debug` printing does not affect SIEVE
    /// eviction priority.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cache")
            .field("capacity", &self.capacity())
            .field("len", &self.len())
            .field("shards", &self.shards())
            .field("entries", &DebugEntries(self))
            .finish()
    }
}

struct DebugEntries<'a, K, V, S: SlotSize, H: BuildHasher>(&'a Cache<K, V, S, H>);

impl<K, V, S, H> fmt::Debug for DebugEntries<'_, K, V, S, H>
where
    K: fmt::Debug + Hash + Eq,
    V: fmt::Debug,
    S: SlotSize,
    H: BuildHasher,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.0.iter()).finish()
    }
}

impl<'a, K, V, S, H> IntoIterator for &'a Cache<K, V, S, H>
where
    K: Hash + Eq,
    S: SlotSize,
    H: BuildHasher,
{
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V, S>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, K, V, S, H> IntoIterator for &'a mut Cache<K, V, S, H>
where
    K: Hash + Eq,
    S: SlotSize,
    H: BuildHasher,
{
    type Item = (&'a K, &'a mut V);
    type IntoIter = IterMut<'a, K, V, S>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<K, V, S, H> Extend<(K, V)> for Cache<K, V, S, H>
where
    K: Hash + Eq,
    S: SlotSize,
    H: BuildHasher,
{
    /// Inserts every `(K, V)` from `iter` via [`Cache::insert`]. Pairs evicted
    /// by capacity pressure during the loop are dropped silently; if you need
    /// to observe them, call `insert` yourself in a loop.
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.insert(k, v);
        }
    }
}

impl<'a, K, V, S, H> Extend<(&'a K, &'a V)> for Cache<K, V, S, H>
where
    K: Hash + Eq + Copy,
    V: Copy,
    S: SlotSize,
    H: BuildHasher,
{
    fn extend<I: IntoIterator<Item = (&'a K, &'a V)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.insert(*k, *v);
        }
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

/// Mutable iterator over a [`Cache`] yielding `(&K, &mut V)` pairs. Created by [`Cache::iter_mut`].
///
/// Holds a raw pointer to the cache's shard array plus a `PhantomData<&'a mut [Inner]>`
/// to encode the exclusive borrow at the type level. The implementation walks
/// shards via raw pointer arithmetic and reads `len` / `tags[i]` through
/// `addr_of!` projections so that no intermediate `&mut Inner` ever exists
/// while a previously-yielded `&'a mut V` is alive — `&mut Inner` would
/// otherwise claim unique access to bytes the caller still holds a borrow into.
pub struct IterMut<'a, K, V, S: SlotSize> {
    shards: *mut Inner<K, V, S>,
    n_shards: usize,
    shard_idx: usize,
    slot_idx: usize,
    _marker: PhantomData<&'a mut [Inner<K, V, S>]>,
}

// SAFETY: IterMut is morally `&'a mut [Inner<K, V, S>]` — same Send/Sync
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
            // SAFETY: shards points into the cache's Box<[Inner]> for which we
            // hold an exclusive borrow (encoded in `_marker`). shard_idx is
            // bounded by n_shards.
            let sh: *mut Inner<K, V, S> = unsafe { self.shards.add(self.shard_idx) };
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
            // Vec's data pointer; the brief shared reborrow of `Vec<u16>` to
            // call `as_ptr` covers the Vec metadata bytes inside Inner, which
            // are disjoint from the entries arena's heap allocation where any
            // outstanding `&mut V` lives.
            let tag = unsafe {
                let tags_field = std::ptr::addr_of!((*sh).tags);
                let data = (*tags_field).as_ptr();
                *data.add(i)
            };
            let id = Inner::<K, V, S>::id_of(tag);
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
    iter: Iter<'a, K, V, S>,
}

impl<'a, K, V, S: SlotSize> Iterator for Keys<'a, K, V, S> {
    type Item = &'a K;
    fn next(&mut self) -> Option<&'a K> {
        self.iter.next().map(|(k, _)| k)
    }
}

/// Iterator over a [`Cache`]'s values. Created by [`Cache::values`].
pub struct Values<'a, K, V, S: SlotSize> {
    iter: Iter<'a, K, V, S>,
}

impl<'a, K, V, S: SlotSize> Iterator for Values<'a, K, V, S> {
    type Item = &'a V;
    fn next(&mut self) -> Option<&'a V> {
        self.iter.next().map(|(_, v)| v)
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

#[cfg(test)]
mod tests;
