//! Per-shard SIEVE state (`Shard`) — the algorithmic core. The publishable
//! [`Cache`](super::Cache) is a thin sharded wrapper around a slice of these.
//!
//! The bit-layout invariants (I4'–I8) and the c-hoist trick are documented in
//! the parent module's header (`super`).

use std::borrow::Borrow;
use std::hash::Hash;
use std::mem::MaybeUninit;

use super::SlotSize;

pub(crate) const EMPTY: u16 = 0;
pub(crate) const LIVE: u16 = 0x8000;
pub(crate) const VISITED: u16 = 0x4000;
/// AVX2 one chunk = 32 bytes = 16 u16 lanes.
const LANE: usize = 16;
/// Structural upper bound for 6-bit entry_id. per_shard must not exceed this.
pub const MAX_PER_SHARD: usize = 64;

pub(crate) struct Entry<K, V> {
    pub(crate) key: K,
    pub(crate) value: V,
}

/// One AVX2-load worth of tags. `align(32)` makes the address of every chunk
/// (and therefore the start of the flat `[u16]` view) suitable for
/// `vmovdqa` / `_mm256_load_si256` rather than the unaligned variant.
#[repr(C, align(32))]
#[derive(Clone, Copy)]
pub(crate) struct TagsChunk(pub [u16; LANE]);

/// `Vec<TagsChunk>` storage that derefs as a flat `&[u16]` view of length
/// `chunks.len() * LANE`. The flat view is bit-for-bit equivalent to the prior
/// `Vec<u16>` layout (same stride, same indexing), so all scalar paths through
/// `self.tags[i]` keep working unchanged via `Deref` / `DerefMut`. The point
/// of the wrapper is the alignment contract: the underlying chunk allocation
/// is 32-byte aligned, which lets `find_avx2` use aligned loads.
pub(crate) struct AlignedTags {
    chunks: Vec<TagsChunk>,
}

impl AlignedTags {
    /// `order_cap` must be a non-zero multiple of `LANE`.
    fn zeroed(order_cap: usize) -> Self {
        debug_assert!(order_cap > 0 && order_cap % LANE == 0);
        let n_chunks = order_cap / LANE;
        Self {
            chunks: vec![TagsChunk([EMPTY; LANE]); n_chunks],
        }
    }
}

impl std::ops::Deref for AlignedTags {
    type Target = [u16];
    #[inline]
    fn deref(&self) -> &[u16] {
        let n = self.chunks.len() * LANE;
        // SAFETY: TagsChunk is `#[repr(C, align(32))] struct(_)([u16; LANE])`,
        // so a contiguous Vec<TagsChunk> is layout-equivalent to [u16; n_chunks*LANE].
        unsafe { std::slice::from_raw_parts(self.chunks.as_ptr().cast::<u16>(), n) }
    }
}

impl std::ops::DerefMut for AlignedTags {
    #[inline]
    fn deref_mut(&mut self) -> &mut [u16] {
        let n = self.chunks.len() * LANE;
        // SAFETY: see Deref impl.
        unsafe { std::slice::from_raw_parts_mut(self.chunks.as_mut_ptr().cast::<u16>(), n) }
    }
}

/// Per-shard SIEVE state. Equivalent to j8's `Inner<K, V>` parameterized by `S`.
pub(crate) struct Shard<K, V, S: SlotSize> {
    pub(crate) capacity: usize,
    /// Parallel array #1: tag array. Size = `round_up(capacity, LANE).max(LANE)`.
    /// Under I4' there are never holes in `tags[0..len]`, so no slack past `capacity`
    /// is needed — the LANE-aligned remainder beyond `len` is permanent EMPTY pad.
    /// Stored as `Vec<TagsChunk>` so the start address is 32-byte aligned (each
    /// chunk = one AVX2 lane); derefs as `&[u16]` for the scalar paths.
    pub(crate) tags: AlignedTags,
    /// Parallel array #2: entries arena. Size = `capacity` (no slack).
    /// Indexed by the 6-bit id embedded in each tag.
    /// `sizeof(S::Storage<Entry<K, V>>) == S::SIZE` is guaranteed by `_STORAGE_SIZE_OK`.
    pub(crate) entries: Vec<MaybeUninit<S::Storage<Entry<K, V>>>>,
    /// SIEVE hand cursor (`0..=len`), sweeping over `tags[0..len]`.
    pub(crate) hand: usize,
    /// Number of currently live entries (= number of live tags = first index past
    /// the live region in `tags`).
    pub(crate) len: usize,
    /// Per-shard observability counters. Plain `u64` rather than `AtomicU64`
    /// because every mutating op already requires `&mut self`; on x86 a plain
    /// `add [mem], 1` is one uop on a dependency chain disjoint from the
    /// returned value, so OoO retires it for free in steady state.
    pub(crate) hits: u64,
    pub(crate) misses: u64,
    pub(crate) insertions: u64,
    pub(crate) evictions: u64,
}

impl<K, V, S: SlotSize> Shard<K, V, S> {
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
    pub(crate) const ID_SHIFT: u32 = (S::SIZE as u32).trailing_zeros();
    /// Mask covering the id field. Invariant: `tag & ID_MASK == id × S::SIZE`
    /// (= byte offset into the entries arena).
    pub(crate) const ID_MASK: u16 = ((MAX_PER_SHARD - 1) as u16) << Self::ID_SHIFT;
    /// Mask covering the hash field. Always exactly 8 bits scattered:
    /// the non-status field is 14 bits (`0x3FFF`) and `ID_MASK` consumes 6 of them
    /// (because `MAX_PER_SHARD == 64`), leaving `14 - 6 = 8` bits for the hash regardless
    /// of which `SlotSize` is in use.
    pub(crate) const HASH_MASK: u16 = 0x3FFF & !Self::ID_MASK;
    /// Comparison target for SIMD scans: LIVE | HASH_MASK (visited and id are masked out).
    pub(crate) const SCAN_MASK: u16 = LIVE | Self::HASH_MASK;

    /// Extracts the id (0..MAX_PER_SHARD) from a tag. Used by scalar path, drop, and evict.
    #[inline]
    pub(crate) fn id_of(tag: u16) -> usize {
        ((tag & Self::ID_MASK) >> Self::ID_SHIFT) as usize
    }

    /// Raw pointer to the **`entry` field** of `entries[id]`.
    /// Because `#[repr(C)] union { entry: ManuallyDrop<E>, _pad: [u64; N] }` places
    /// the first field at offset 0, the `Storage<E>` pointer is the same as `*const E`.
    /// `MaybeUninit<T>` preserves this layout.
    #[inline]
    pub(crate) fn entry_ptr(&self, id: usize) -> *const Entry<K, V> {
        // Re-anchor the layout invariants at the use site, so that any future code
        // path that touches Entry through Storage (not just `Shard::new`) keeps the
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

impl<K, V, S: SlotSize> Shard<K, V, S>
where
    K: Hash + Eq,
{
    pub(crate) fn new(capacity: usize) -> Self {
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
            tags: AlignedTags::zeroed(order_cap),
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
    pub(crate) fn needle_from_hash(hash: u64) -> u16 {
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
    /// check; `Shard::find` performs it via the cached `has_avx2_bmi1` flag set in
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
                    let tag = *(chunk_byte_ptr.add(bit) as *const u16) as u32;
                    let id_bytes = (tag & id_mask_u32) as usize;
                    // live needle ⟹ tag live ⟹ entries[id] initialized (I6).
                    // id_bytes = id × S::SIZE and id < capacity ⟹ in bounds.
                    // Storage is #[repr(C)] with entry at offset 0 ⟹ Entry reachable directly.
                    let entry_ptr = entries_byte_ptr.add(id_bytes) as *const Entry<K, V>;
                    let e = &*entry_ptr;
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
    }

    pub(crate) fn contains<Q>(&self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> bool
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        self.find(key, Self::needle_from_hash(hash), has_avx2_bmi1)
            .is_some()
    }

    pub(crate) fn get<Q>(&mut self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<&V>
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

    pub(crate) fn get_mut<Q>(&mut self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<&mut V>
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
    pub(crate) fn peek<Q>(&self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<&V>
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
    pub(crate) fn peek_mut<Q>(&mut self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<&mut V>
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

    pub(crate) fn insert(
        &mut self,
        key: K,
        value: V,
        hash: u64,
        has_avx2_bmi1: bool,
    ) -> Option<(K, V)> {
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
    pub(crate) fn remove<Q>(&mut self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<V>
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
            }
        }
        let guard = Guard { shard: self };
        let shard = &mut *guard.shard;

        // Pass 1: walk tags[0..old_len], decide keep/drop, compact survivors
        // into tags[0..write] in place. Each iteration is structured so that
        // if `f` panics, the only mutation already committed is to the slot
        // currently being read — and that slot is left in a state the guard's
        // Drop can clean up uniformly (LIVE tag ⟹ initialized entry).
        let mut write = 0usize;
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

        // Commit new len + hand. The remaining tags[write..old_len] are
        // already EMPTY (every iteration that increased write zeroed read,
        // every drop iteration zeroed read), so I4' is preserved.
        shard.len = write;
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
    }
}

impl<K, V, S> Clone for Shard<K, V, S>
where
    K: Hash + Eq + Clone,
    V: Clone,
    S: SlotSize,
{
    fn clone(&self) -> Self {
        // Clone every live (key, value) into an owned Vec first. If any user
        // Clone impl panics partway, the partial Vec drops cleanly and we
        // never touch the destination Shard — so the destination cannot end
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

        let mut new = Shard::<K, V, S>::new(self.capacity);
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

impl<K, V, S: SlotSize> Drop for Shard<K, V, S> {
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
