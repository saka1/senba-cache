//! Per-shard SIEVE state (`Shard`) — the algorithmic core. The publishable
//! [`Cache`](super::Cache) is a thin sharded wrapper around a slice of these.
//!
//! The bit-layout invariants (I4'–I8) and the c-hoist trick are documented in
//! the parent module's header (`super`).
//!
//! ## Layer split
//!
//! `Shard` is built up across four files via sibling `impl` blocks; this file
//! holds the type, invariants, addressing primitives, ctor, `Clone`, `Drop`.
//! The sibling files extend the same `impl Shard<K, V, S>` (with or without
//! `where K: Hash + Eq`):
//!
//! - `scan.rs` — Layer A: `find` / `find_scalar` / `find_avx2` /
//!   `find_evict_pos` / `needle_from_hash`
//! - `state.rs` — Layer B: `insert` / `remove` / `clear` / `retain`
//! - `lookup.rs` — Layer C: `contains` / `get*` / `peek*` / `get_key_value` /
//!   `peek_key_value` / `get_or_insert_with` (+ private `find_and_touch`
//!   helper that folds the promoting-lookup bookkeeping)

use std::hash::Hash;
use std::mem::MaybeUninit;
use std::num::NonZeroU16;

use super::SlotSize;

mod lookup;
mod scan;
mod state;

pub(crate) const EMPTY: u16 = 0;
pub(crate) const LIVE: u16 = 0x8000;
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

/// Shift the visited bitmap to mirror `tags.copy_within(pos+1..len, pos)`:
/// bits at positions `[pos+1, 64)` move down by one to `[pos, 63)`, the
/// original bit at `pos` is dropped, and the new bit at position `63` is 0
/// (the source position 64 carried no bit).
///
/// Pure u64 form. `pos ∈ [0, 63]` ⟹ `1 << pos` is well-defined, and `>> 1`
/// is safe so the previous `u128` reg-pair (`shld/shrd`) is gone. Avoiding
/// u128 here removes 9 `shld/shrd` instructions across the 4 `Shard::insert`
/// monomorphizations in the perf-gate bench (see
/// `2026-05-16-find-evict-pos-cut-a-results.md` §1.3 asm survey).
///
/// Corner cases (verified by oracle + tests/eviction):
/// - `pos == 0`: `pos_mask = 0`, `low = 0`, `high = visited >> 1`. Bit 0
///   dropped, all higher bits shift down by one. ✓
/// - `pos == 63`: `pos_mask = (1<<63) - 1`, `low = visited & [0,63)`,
///   `high = (visited >> 1) & bit63 = 0` (`>> 1` clears the top bit anyway).
///   Result `low` = `visited` with bit 63 cleared. ✓
#[inline]
fn shift_visited_down_in_place(visited: &mut u64, pos: usize) {
    debug_assert!(pos < 64);
    let pos_mask = (1u64 << pos).wrapping_sub(1); // bits [0, pos)
    let low = *visited & pos_mask;
    let high = (*visited >> 1) & !pos_mask;
    *visited = low | high;
}

/// Per-shard SIEVE state. Equivalent to j8's `Inner<K, V>` parameterized by `S`.
///
/// **Layout (`#[repr(C)]`, locked by `_LAYOUT_OK` const-eval below).** Field
/// order is chosen so the read-side hot path (`tags.ptr`, `entries.ptr`, `len`,
/// `visited`) is fully on cache line 1; `hand` (touched only on evict / new
/// insert) and the four observability counters live on line 2.
///
/// | offset | field    | size | line |
/// |-------:|----------|-----:|-----:|
/// | 0      | tags     |   24 | 1    |
/// | 24     | entries  |   24 | 1    |
/// | 48     | len      |    8 | 1    |
/// | 56     | visited  |    8 | 1    |
/// | 64     | hand     |    8 | 2    |
/// | 72..   | counters | 4×8  | 2    |
///
/// Total = 104 B. `capacity` is intentionally **not** a field: it equals
/// `entries.len()` (set in `new`, never resized) and is exposed via
/// `Shard::capacity()` whose load lives on the same line as `entries.ptr`.
#[repr(C)]
pub(crate) struct Shard<K, V, S: SlotSize> {
    /// Parallel array #1: tag array. Size = `round_up(capacity, LANE).max(LANE)`.
    /// Under I4' there are never holes in `tags[0..len]`, so no slack past `capacity`
    /// is needed — the LANE-aligned remainder beyond `len` is permanent EMPTY pad.
    /// Stored as `Vec<TagsChunk>` so the start address is 32-byte aligned (each
    /// chunk = one AVX2 lane); derefs as `&[u16]` for the scalar paths.
    pub(crate) tags: AlignedTags,
    /// Parallel array #2: entries arena. Size = `capacity` (no slack), set in
    /// `new` via `resize_with` and never resized afterwards. `entries.len()` is
    /// therefore the per-shard capacity (see `Shard::capacity()`).
    /// Indexed by the 6-bit id embedded in each tag.
    /// `sizeof(S::Storage<Entry<K, V>>) == S::SIZE` is guaranteed by `_STORAGE_SIZE_OK`.
    pub(crate) entries: Vec<MaybeUninit<S::Storage<Entry<K, V>>>>,
    /// Number of currently live entries (= number of live tags = first index past
    /// the live region in `tags`).
    pub(crate) len: usize,
    /// Per-slot VISITED bits, one per position in `tags[0..len]`. Bit `i` is set
    /// iff the entry at `tags[i]` was promoted (hit) since its last sweep.
    /// `MAX_PER_SHARD == 64` ⟹ a single `u64` suffices.
    /// Was previously `tag & VISITED` packed into the `u16` tag; promoting it
    /// out reclaimed one bit for the hash field (8 → 9 bits) and turned the
    /// SIEVE victim search from `O(len)` `scan_evict` into a single bit-find.
    /// Co-located with `len` on cache line 1 so the on-hit `|=` and the
    /// `find_evict_pos` bit-twiddle pay no extra line beyond the find scan.
    pub(crate) visited: u64,
    /// SIEVE hand cursor (`0..=len`), sweeping over `tags[0..len]`. Only touched
    /// on evict / new-entry insert, so it sits on line 2 — find-only callers
    /// (`get` / `contains`) never load this line.
    pub(crate) hand: usize,
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

    /// Const-eval: `TagsChunk` must be 32-byte aligned. `find_avx2` issues
    /// `_mm256_load_si256` against `tags.as_ptr()`, which is sound only because
    /// `Vec<TagsChunk>` inherits `repr(C, align(32))` on its element type. If a
    /// future refactor drops the align attribute, the runtime `debug_assert!` in
    /// `find_avx2` would catch it in debug builds but release builds would
    /// silently execute UB on misaligned chunks. Anchor the invariant at compile
    /// time so it cannot be lost.
    const _TAGSCHUNK_ALIGN_OK: () = assert!(
        std::mem::align_of::<TagsChunk>() == 32,
        "senba::Cache: TagsChunk must be 32-byte aligned for vmovdqa"
    );

    /// Const-eval: read-side hot path (`tags`, `entries`, `len`, `visited`)
    /// must fit on cache line 1 (`offset < 64`), and `hand` must be on line 2.
    /// This is the load-bearing cache-layout contract for `Shard` — see the
    /// struct doc table above. `Vec<MaybeUninit<S::Storage<...>>>` is 24 B
    /// (ptr/len/cap) for any `K, V, S` so the tail offsets are independent of
    /// the type parameters.
    const _LAYOUT_OK: () = {
        assert!(std::mem::offset_of!(Self, tags) == 0);
        assert!(std::mem::offset_of!(Self, entries) == 24);
        assert!(std::mem::offset_of!(Self, len) == 48);
        assert!(std::mem::offset_of!(Self, visited) == 56);
        assert!(std::mem::offset_of!(Self, hand) == 64);
    };

    /// Per-shard capacity. Equals `entries.len()` because `entries` is sized to
    /// `capacity` in `new` and never resized. The load shares cache line 1
    /// with `tags.ptr` and `entries.ptr`, so callers on the hot path pay no
    /// extra line.
    #[inline]
    pub(crate) fn capacity(&self) -> usize {
        self.entries.len()
    }

    /// Bit position of the id field (6 bits) within a tag.
    /// Chosen as `log2(S::SIZE)` so that `id << ID_SHIFT == id × S::SIZE`.
    pub(crate) const ID_SHIFT: u32 = (S::SIZE as u32).trailing_zeros();
    /// Mask covering the id field. Invariant: `tag & ID_MASK == id × S::SIZE`
    /// (= byte offset into the entries arena).
    pub(crate) const ID_MASK: u16 = ((MAX_PER_SHARD - 1) as u16) << Self::ID_SHIFT;
    /// Mask covering the hash field. Always exactly 9 bits scattered:
    /// the non-LIVE field is 15 bits (`0x7FFF`) and `ID_MASK` consumes 6 of them
    /// (because `MAX_PER_SHARD == 64`), leaving `15 - 6 = 9` bits for the hash regardless
    /// of which `SlotSize` is in use. (Was 8 bits when VISITED was packed into the tag;
    /// promoting VISITED out to a per-shard bitmap reclaimed that bit for the hash.)
    pub(crate) const HASH_MASK: u16 = 0x7FFF & !Self::ID_MASK;
    /// Comparison target for SIMD scans: LIVE | HASH_MASK (id is masked out; VISITED
    /// is no longer part of the tag and so doesn't need masking).
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

    /// Hot-path helper: pointer to `Entry` via the byte offset already encoded in
    /// `tag & ID_MASK` (= `id × S::SIZE`). Skips the `id_of(tag) → << ID_SHIFT`
    /// shift round-trip that LLVM's InstCombine empirically refuses to fold.
    /// See `docs/reports/2026-05-08-find-avx2-caller-merge.md` §3.3 (A3).
    ///
    /// # Safety
    /// `tag` must be a live tag (LIVE bit set) read from `self.tags[..self.len]`.
    /// `NonZeroU16` is the right type because live tags are always non-zero
    /// (LIVE = 0x8000), and using it on the return path is what keeps `find`'s
    /// `Option<(usize, NonZeroU16)>` 16-byte and avoids sret.
    #[inline]
    unsafe fn entry_ptr_from_tag(&self, tag: NonZeroU16) -> *const Entry<K, V> {
        let _: () = Self::_SIZE_OK;
        let _: () = Self::_STORAGE_SIZE_OK;
        let off = (tag.get() & Self::ID_MASK) as usize;
        debug_assert!(tag.get() & LIVE != 0);
        debug_assert!(off < self.capacity() * S::SIZE);
        unsafe { (self.entries.as_ptr() as *const u8).add(off) as *const Entry<K, V> }
    }

    /// `&mut` variant of `entry_ptr_from_tag`. Same SAFETY contract.
    #[inline]
    unsafe fn entry_ptr_mut_from_tag(&mut self, tag: NonZeroU16) -> *mut Entry<K, V> {
        let _: () = Self::_SIZE_OK;
        let _: () = Self::_STORAGE_SIZE_OK;
        let off = (tag.get() & Self::ID_MASK) as usize;
        debug_assert!(tag.get() & LIVE != 0);
        debug_assert!(off < self.capacity() * S::SIZE);
        unsafe { (self.entries.as_mut_ptr() as *mut u8).add(off) as *mut Entry<K, V> }
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
        let _: () = Self::_TAGSCHUNK_ALIGN_OK;
        let _: () = Self::_LAYOUT_OK;

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
            tags: AlignedTags::zeroed(order_cap),
            entries,
            len: 0,
            visited: 0,
            hand: 0,
            hits: 0,
            misses: 0,
            insertions: 0,
            evictions: 0,
        }
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

        let mut new = Shard::<K, V, S>::new(self.capacity());
        // tags arrays have identical length (both = round_up(capacity, LANE).max(LANE)).
        new.tags.copy_from_slice(&self.tags);
        new.hand = self.hand;
        new.len = self.len;
        new.visited = self.visited;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Slot16, Slot32, Slot64, SlotSize};

    /// Bit-field exclusivity for Slot32 (default, Entry<u64,u64>=16).
    /// Shard<u64, u64, Slot32>: ID_SHIFT = 5, ID_MASK = 0x07e0, HASH_MASK = 0x781f.
    /// (HASH_MASK gained one bit vs the original layout when VISITED moved out
    /// of the tag into a per-shard u64 bitmap.)
    #[test]
    fn bit_layout_exclusivity_slot32() {
        type I = Shard<u64, u64, Slot32>;
        assert_eq!(I::ID_SHIFT, 5);
        assert_eq!(I::ID_MASK, 0x07e0);
        assert_eq!(I::HASH_MASK, 0x781f);
        assert_eq!(I::SCAN_MASK, LIVE | I::HASH_MASK);
        assert_eq!(I::SCAN_MASK, 0xf81f);

        // After dropping VISITED, the only remaining status bit is LIVE; the
        // 15-bit non-LIVE region is partitioned exactly by ID_MASK + HASH_MASK.
        assert_eq!(LIVE | I::ID_MASK | I::HASH_MASK, 0xFFFF);
        assert_eq!(LIVE & I::ID_MASK, 0);
        assert_eq!(LIVE & I::HASH_MASK, 0);
        assert_eq!(I::ID_MASK & I::HASH_MASK, 0);
        assert_eq!(I::HASH_MASK.count_ones(), 9);

        // c-hoist invariant: embedding id into a tag gives `tag & ID_MASK = id × S::SIZE`.
        for id in 0..MAX_PER_SHARD {
            let tag_id_field = (id as u16) << I::ID_SHIFT;
            assert_eq!((tag_id_field & I::ID_MASK) as usize, id * Slot32::SIZE);
        }
    }

    #[test]
    fn bit_layout_slot16() {
        type I = Shard<u32, u32, Slot16>;
        assert_eq!(I::ID_SHIFT, 4);
        assert_eq!(I::ID_MASK, 0x03f0);
        assert_eq!(I::HASH_MASK, 0x7c0f);
        assert_eq!(I::HASH_MASK.count_ones(), 9);
    }

    #[test]
    fn bit_layout_slot64() {
        type I = Shard<u64, u64, Slot64>;
        assert_eq!(I::ID_SHIFT, 6);
        assert_eq!(I::ID_MASK, 0x0fc0);
        assert_eq!(I::HASH_MASK, 0x703f);
        assert_eq!(I::HASH_MASK.count_ones(), 9);
    }
}
