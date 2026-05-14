//! Per-shard concurrent SIEVE state machine, ported from the `sieve_r4` research
//! variant (`research/src/experimental/sieve_r4.rs`). Replaces the earlier
//! `c17s`-based lift; see `docs/reports/2026-05-15-r4-vs-c17s.md` for the
//! comparison and `docs/reports/2026-05-14-arc-less-concurrent-design.md` for
//! the soundness derivation.
//!
//! ## Reader / writer protocol (1-tier seqlock + path_c_epoch)
//!
//! Each `Entry<K, V>` carries an `AtomicU32` version (offset 0, `repr(C,
//! align(32))`). Readers validate via `version_even → ptr::read → version_re
//! load matches v1` and bail otherwise. Writers (Path A: lock-free
//! in-place value update; Path B/C: warmup install / evict + shift +
//! install under per-shard `Mutex`) wrap their mutations in `CAS(v, v+1) →
//! write → store(v+2)`.
//!
//! `path_c_epoch` is a coarser seqlock that catches the false-miss readers
//! see when a Path C shift transient (tag at pos goes `LIVE → EMPTY → next
//! tag`) hides a still-live candidate inside one AVX2 chunk. Readers sample
//! it before/after a scan; a change (or any in-flight `Racing` candidate)
//! triggers a retry.
//!
//! ## Tag layout (16 bits)
//!
//! ```text
//!   bit 15:                LIVE (set ⇒ the entry id this tag references is initialised)
//!   bits ID_SHIFT..+6:     ID  (6 bits, MAX_PER_SHARD = 64)
//!   remaining 9 bits:      HASH (carried into the SIMD comparison)
//! ```
//!
//! `SCAN_MASK = LIVE | HASH_MASK` (ID excluded). VERSION-in-tag is **not**
//! used (the entry seqlock subsumes its role), so reader needle-match
//! collides only on the 9 hash bits.
//!
//! ## Soundness model: `ManuallyDrop<V>` + `crossbeam-epoch` deferred reclaim
//!
//! Each entry stores `key: ManuallyDrop<K>` and `value: ManuallyDrop<V>`.
//! The reader's `ptr::read(entry_ptr)` is a bit-wise copy of the live entry
//! into a `ManuallyDrop<Entry<K, V>>` local — the local's `Drop` is
//! suppressed so the bit-copy never double-frees the live K / V owned by
//! the arena.
//!
//! 1. Reader pins the local epoch on entry to `get_by_hash` (`pin_for::<V>`
//!    — folded away by `needs_drop::<V>` for `V: Copy`).
//! 2. Reader bit-copies, validates the seqlock (`v1 == v2`), then clones
//!    `V` out of `*buf.value`. The clone allocates a fresh `V` independent
//!    of the arena.
//! 3. Writers that overwrite `entries[id]` (Path A in-place value update,
//!    `writer_update_in_place`) extract the old `V` via raw `ptr::read`
//!    through `&mut ManuallyDrop<V> as *mut V`, then `defer_drop_if_needed`
//!    the old `V` past every reader pin via `Guard::defer_unchecked`.
//! 4. The Path C evict path (`writer_evict_and_install`) and `remove`
//!    `ManuallyDrop::take` both K and V out of the slot; the *cloned* V
//!    flows back to the caller while the *original* V (and, on `remove`,
//!    K) is deferred. The pin held by any in-flight reader keeps the
//!    deferred drop suspended until the reader's `V::clone` finishes.
//!
//! Race β (clone-mid-flight UAF on V) and race γ (the symmetric UAF on K)
//! are both structurally closed: writer_evict_and_install and `remove` use
//! `ManuallyDrop::take` to move K and V out of the slot, and the deferred
//! `on_evict` callback (or the symmetric `defer_drop_kv_if_needed` for
//! `remove`) keeps the moved-out allocations alive past every in-flight
//! reader pin. The bit-copy local a reader holds therefore always
//! references valid memory until the reader's own `V::clone` (and `K::eq`)
//! finishes.
//!
//! The runtime cost is `~3–5 ns` per reader-hit for the `epoch::pin` when
//! `V: !Copy`; zero when `V: Copy` (the pin folds away via
//! `needs_drop::<V>()`).

use crossbeam_epoch as epoch;
use parking_lot::Mutex;
use std::borrow::Borrow;
use std::cell::UnsafeCell;
use std::hash::Hash;
use std::hint;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::sync::atomic::{
    AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering, compiler_fence, fence,
};

/// `EMPTY` tag (LIVE bit cleared). Used for the Path C shift transient and
/// for the permanent pad lanes past `capacity`. Path A never writes EMPTY.
const EMPTY: u16 = 0;
/// LIVE bit. Set when the tag's referenced entry id is initialised.
const LIVE: u16 = 0x8000;
/// AVX2 chunk = 32 bytes = 16 `u16` lanes.
const LANE: usize = 16;
/// 6-bit entry id ceiling. The per-shard capacity must be `<= MAX_PER_SHARD`.
pub(crate) const MAX_PER_SHARD: usize = 64;

/// Wrapper around `Option<epoch::Guard>` that folds away on `V: Copy`.
/// `pin_for::<V>()` returns `None` when `V` has no destructor — the guard
/// allocation and the corresponding `epoch::pin` syscall are dead code that
/// LLVM eliminates via `needs_drop::<V>()` const-fold on the call site.
#[allow(dead_code)]
enum EpochGuardWrapper {
    Some(epoch::Guard),
    None,
}

#[inline(always)]
fn needs_epoch<V>() -> bool {
    std::mem::needs_drop::<V>()
}

#[inline(always)]
fn pin_for<V>() -> EpochGuardWrapper {
    if needs_epoch::<V>() {
        EpochGuardWrapper::Some(epoch::pin())
    } else {
        EpochGuardWrapper::None
    }
}

/// Defer `v`'s drop past every in-flight reader pin. When `V: Copy` (no
/// destructor) this folds to `mem::forget`, which is the correct no-op for
/// a value the arena no longer owns.
///
/// SAFETY of the underlying `defer_unchecked`: `crossbeam-epoch` requires
/// the deferred closure to be `Send + 'static`. The `V: Send + 'static`
/// trait bound on the parent `impl` block satisfies that.
#[inline]
fn defer_drop_if_needed<V: Send + 'static>(v: V) {
    if needs_epoch::<V>() {
        let guard = epoch::pin();
        // SAFETY: `V: Send + 'static` makes the closure capture sound for
        // off-thread reclaim by the epoch GC.
        unsafe {
            guard.defer_unchecked(move || drop(v));
        }
    } else {
        std::mem::forget(v);
    }
}

/// Defer both K and V drops together. If either has a destructor, both are
/// captured in a single deferred closure (one `epoch::pin`, one queue
/// entry). If neither has one, both are `mem::forget`'d.
#[inline]
fn defer_drop_kv_if_needed<K, V>(k: K, v: V)
where
    K: Send + 'static,
    V: Send + 'static,
{
    if std::mem::needs_drop::<K>() || std::mem::needs_drop::<V>() {
        let guard = epoch::pin();
        // SAFETY: `K, V: Send + 'static` make the closure capture sound.
        unsafe {
            guard.defer_unchecked(move || {
                drop(k);
                drop(v);
            });
        }
    } else {
        std::mem::forget(k);
        std::mem::forget(v);
    }
}

const fn id_shift_from_entry_size(s: usize) -> u32 {
    assert!(
        s.is_power_of_two(),
        "senba::concurrent::Cache: sizeof(Entry<K,V>) must be a power of two"
    );
    assert!(
        s <= 256,
        "senba::concurrent::Cache: sizeof(Entry<K,V>) must be <= 256"
    );
    s.trailing_zeros()
}

const fn id_mask_from_shift(id_shift: u32) -> u16 {
    ((MAX_PER_SHARD - 1) as u16) << id_shift
}

const fn hash_mask_from_id_mask(id_mask: u16) -> u16 {
    0x7FFF & !id_mask
}

/// Single entry in the per-shard arena.
///
/// `repr(C, align(32))` makes the size a power of two regardless of the
/// natural alignment of `K` / `V`. `version` lives at offset 0 so the
/// seqlock load is the first cache line touched. `key` and `value` are
/// wrapped in `ManuallyDrop` so writers can move them out via
/// `ManuallyDrop::take` (Path C / `remove`) without the arena's `Drop`
/// running an extra time, and so readers' bit-copy locals never run a
/// duplicate destructor.
#[repr(C, align(32))]
struct Entry<K, V> {
    /// Even = stable, odd = writer in flight. Path A and Path C write the
    /// `value` (and Path C the `key`) inside a `CAS(v, v+1) → write →
    /// store(v+2)` envelope.
    version: AtomicU32,
    key: ManuallyDrop<K>,
    value: ManuallyDrop<V>,
}

enum Probe<V> {
    Found(V),
    Miss,
    Racing,
}

type EntriesArena<K, V> = UnsafeCell<Box<[MaybeUninit<Entry<K, V>>]>>;

/// Writer-hot state co-located on a single cache line. Path A reads
/// `visited` (relaxed) and never touches the rest; Path B/C take the
/// `writer` mutex first and then drive `hand` / `len` / `path_c_epoch`.
///
/// `WriterState` itself is boxed so the [`Mutex`] payload is a single
/// pointer regardless of `K` / `V` (the `Vec<u16>` free list would
/// otherwise push the struct past 64 B on its own).
#[repr(C, align(64))]
struct ShardHot {
    writer: Mutex<Box<WriterState>>,
    /// Bitmap, one bit per pos in `tags[0..len]`. Readers `fetch_or`,
    /// writers `fetch_and`.
    visited: AtomicU64,
    /// Number of live tags. `tags[0..len]` is the SIEVE-ordered ring;
    /// `tags[len..]` is the pad zone.
    len: AtomicUsize,
    /// Bumped at the end of every Path C completion. Readers snapshot
    /// before/after each scan to detect a shift transient.
    path_c_epoch: AtomicU64,
}

const _: () = {
    assert!(std::mem::size_of::<ShardHot>() == 64);
    assert!(std::mem::align_of::<ShardHot>() == 64);
};

struct WriterState {
    /// SIEVE hand pointer. Walks `0..cap` cyclically during Path C.
    hand: usize,
    /// Entry ids freed by `remove`. Reused before `next_fresh_id` is
    /// incremented further. Path A and Path C never push here; only Path B
    /// pops, and only `remove` pushes.
    free_ids: Vec<u16>,
    /// Lowest entry id that has never been used. Bumped only when
    /// `free_ids` is empty and a fresh slot is needed.
    next_fresh_id: u16,
}

/// One shard of the concurrent SIEVE cache.
pub(crate) struct Shard<K, V> {
    capacity: usize,
    tags: Box<[AtomicU16]>,
    entries: EntriesArena<K, V>,
    hot: ShardHot,
}

// SAFETY: writes to `entries[id]` are guarded by `Entry::version` (Path A)
// or by `hot.writer` (Path B/C). The `UnsafeCell` is the only `!Sync` field
// at the type level; the protocol above turns it into a properly
// synchronised arena. Readers bit-copy via `ptr::read` (raw memory copy,
// not `&K` / `&V` access) and only call `K::eq` / `V::clone` on the
// thread-local bit-copy, so we don't propagate a `Sync` requirement onto
// K / V — `Send` is sufficient.
unsafe impl<K: Send, V: Send> Send for Shard<K, V> {}
unsafe impl<K: Send, V: Send> Sync for Shard<K, V> {}

impl<K, V> Shard<K, V> {
    const ENTRY_SIZE: usize = std::mem::size_of::<Entry<K, V>>();
    const ID_SHIFT: u32 = id_shift_from_entry_size(Self::ENTRY_SIZE);
    const ID_MASK: u16 = id_mask_from_shift(Self::ID_SHIFT);
    const HASH_MASK: u16 = hash_mask_from_id_mask(Self::ID_MASK);
    const SCAN_MASK: u16 = LIVE | Self::HASH_MASK;

    #[inline]
    fn id_of(tag: u16) -> usize {
        ((tag & Self::ID_MASK) >> Self::ID_SHIFT) as usize
    }

    #[inline]
    fn vbit_mask(pos: usize) -> u64 {
        debug_assert!(pos < 64, "vbit_mask: pos {pos} >= 64 (per-shard cap limit)");
        1u64 << pos
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) fn len(&self) -> usize {
        self.hot.len.load(Ordering::Acquire)
    }

    #[inline]
    fn entries_ptr(&self) -> *const MaybeUninit<Entry<K, V>> {
        unsafe { (*self.entries.get()).as_ptr() }
    }

    #[inline]
    fn entries_mut_ptr(&self) -> *mut MaybeUninit<Entry<K, V>> {
        // SAFETY: `UnsafeCell::get` does not need `&mut self`; callers must
        // uphold the writer-mutex / version-CAS exclusion themselves.
        unsafe { (*self.entries.get()).as_mut_ptr() }
    }
}

impl<K, V> Shard<K, V> {
    pub(crate) fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        assert!(
            capacity <= MAX_PER_SHARD,
            "per-shard capacity ({capacity}) must be <= {MAX_PER_SHARD} (6-bit ID limit)"
        );
        let order_cap = ((capacity + LANE - 1) & !(LANE - 1)).max(LANE);

        let mut tags_vec: Vec<AtomicU16> = Vec::with_capacity(order_cap);
        for _ in 0..order_cap {
            tags_vec.push(AtomicU16::new(EMPTY));
        }

        let mut entries_vec: Vec<MaybeUninit<Entry<K, V>>> = Vec::with_capacity(capacity);
        entries_vec.resize_with(capacity, MaybeUninit::uninit);

        Self {
            capacity,
            tags: tags_vec.into_boxed_slice(),
            entries: UnsafeCell::new(entries_vec.into_boxed_slice()),
            hot: ShardHot {
                writer: Mutex::new(Box::new(WriterState {
                    hand: 0,
                    free_ids: Vec::new(),
                    next_fresh_id: 0,
                })),
                visited: AtomicU64::new(0),
                len: AtomicUsize::new(0),
                path_c_epoch: AtomicU64::new(0),
            },
        }
    }
}

impl<K, V> Shard<K, V>
where
    K: Hash + Eq + Send + 'static,
    V: Clone + Send + 'static,
{
    /// Spread the top 9 bits of `hash` over the 9 HASH bits of the tag.
    /// The HASH region is **not** contiguous (the ID region cuts through
    /// it), so the spread splits along the ID boundary.
    #[inline]
    fn needle_from_hash(hash: u64) -> u16 {
        let h9 = ((hash >> 55) as u16) & 0x01FF;
        let s = Self::ID_SHIFT;
        let spread = if s >= 9 {
            h9
        } else {
            let low_mask: u16 = ((1u32 << s) - 1) as u16;
            let low = h9 & low_mask;
            let high = (h9 & !low_mask) << 6;
            low | high
        };
        LIVE | spread
    }

    pub(crate) fn contains<Q>(&self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.get_by_hash::<Q>(key, hash, has_avx2_bmi1).is_some()
    }

    pub(crate) fn get_by_hash<Q>(&self, key: &Q, hash: u64, has_avx2_bmi1: bool) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        const MAX_READER_RETRY: usize = 4;
        let needle = Self::needle_from_hash(hash);
        // Pin the local epoch for the duration of the lookup. The pin
        // folds away when `V: Copy` (no destructor ⇒ no defer to wait on).
        // For `V: !Copy`, the pin keeps the arena's K / V allocations
        // alive past a concurrent writer's `defer_unchecked` drop so the
        // local bit-copy's `V::clone` can dereference safely.
        let _guard = pin_for::<V>();
        for attempt in 0..MAX_READER_RETRY {
            let epoch_before = self.hot.path_c_epoch.load(Ordering::Acquire);
            let (v, racing) = self.find_get::<Q>(key, needle, has_avx2_bmi1);
            if let Some(v) = v {
                return Some(v);
            }
            let epoch_after = self.hot.path_c_epoch.load(Ordering::Acquire);
            if !racing && epoch_before == epoch_after {
                return None;
            }
            if attempt + 1 < MAX_READER_RETRY {
                hint::spin_loop();
            }
        }
        None
    }

    /// Reader scan dispatcher: AVX2 fast path when the host advertises it,
    /// scalar fallback otherwise. EMPTY-lane detection is intentionally
    /// absent in both twins — Path A never writes EMPTY, and Path C
    /// transients are caught via `path_c_epoch`. The `pos < len` filter is
    /// similarly skipped (the pad zone is permanent EMPTY so the LIVE-bit
    /// prefix in `SCAN_MASK` rejects it).
    #[inline]
    fn find_get<Q>(&self, key: &Q, needle: u16, has_avx2_bmi1: bool) -> (Option<V>, bool)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        #[cfg(target_arch = "x86_64")]
        {
            if has_avx2_bmi1 {
                // SAFETY: `has_avx2_bmi1` was set from
                // `is_x86_feature_detected!("avx2")` in
                // `Cache::with_shards_and_hasher`. AVX2 implies BMI1 on
                // every shipping x86_64 part. The detection result is
                // valid for the process lifetime, so caching it is sound.
                return unsafe { self.find_get_avx2::<Q>(key, needle) };
            }
        }
        let _ = has_avx2_bmi1; // avoid unused-arg warning on non-x86_64
        self.find_get_scalar::<Q>(key, needle)
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_get_avx2<Q>(&self, key: &Q, needle: u16) -> (Option<V>, bool)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        use std::arch::x86_64::*;

        let tags_ptr = self.tags.as_ptr() as *const u16;
        let needle_v = _mm256_set1_epi16(needle as i16);
        let mask_v = _mm256_set1_epi16(Self::SCAN_MASK as i16);

        let limit = self.tags.len();

        let mut i = 0usize;
        let mut racing = false;
        while i < limit {
            // SAFETY: `limit == self.tags.len()` is `next_multiple_of(LANE)`
            // (see `Shard::new`), so every iteration's 32-byte load stays
            // inside the allocation. Unaligned load is intentional; tags
            // is 64-byte aligned via repr(align), so this is also aligned
            // in practice, but loadu is the conservative choice.
            let v = unsafe { _mm256_loadu_si256(tags_ptr.add(i) as *const __m256i) };
            let masked = _mm256_and_si256(v, mask_v);
            let cmp = _mm256_cmpeq_epi16(masked, needle_v);
            let mut mask = _mm256_movemask_epi8(cmp) as u32;

            while mask != 0 {
                let bit = mask.trailing_zeros() as usize;
                let lane = bit >> 1;
                let pos = i + lane;
                match self.try_candidate::<Q>(pos, key, needle) {
                    Probe::Found(val) => return (Some(val), false),
                    Probe::Racing => racing = true,
                    Probe::Miss => {}
                }
                mask = _blsr_u32(mask);
                mask = _blsr_u32(mask);
            }
            i += LANE;
        }
        (None, racing)
    }

    /// Portable twin of [`Self::find_get_avx2`]. Iterates `tags[..]`
    /// (full length including pad lanes — the AVX2 twin scans the same
    /// range; pad lanes are permanent EMPTY and rejected by the
    /// `SCAN_MASK` LIVE prefix).
    fn find_get_scalar<Q>(&self, key: &Q, needle: u16) -> (Option<V>, bool)
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let limit = self.tags.len();
        let mut racing = false;
        for pos in 0..limit {
            // Ordering: the AVX2 twin loads 16 tags via
            // `_mm256_loadu_si256` (unordered). Synchronisation is done
            // by `try_candidate` via its `Acquire` re-load of the tag
            // and the entry version. A `Relaxed` load here matches the
            // AVX2 path's lack of ordering on the initial scan; any
            // candidate observed is re-validated inside `try_candidate`
            // before any K::eq / V::clone runs.
            let t = self.tags[pos].load(Ordering::Relaxed);
            if (t & Self::SCAN_MASK) != needle {
                continue;
            }
            match self.try_candidate::<Q>(pos, key, needle) {
                Probe::Found(val) => return (Some(val), false),
                Probe::Racing => racing = true,
                Probe::Miss => {}
            }
        }
        (None, racing)
    }

    #[inline]
    fn try_candidate<Q>(&self, pos: usize, key: &Q, needle: u16) -> Probe<V>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let t1 = self.tags[pos].load(Ordering::Acquire);
        if (t1 & Self::SCAN_MASK) != needle {
            return Probe::Miss;
        }
        let id = Self::id_of(t1);
        let entries_base = self.entries_ptr();
        let entry_ptr = unsafe { entries_base.add(id) as *const Entry<K, V> };

        let v1 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
        if v1 & 1 != 0 {
            return Probe::Racing;
        }
        // SAFETY: ManuallyDrop suppresses the local Drop. `entries[id]` keeps
        // ownership of the live K and V; the bit-copy local never drops them.
        let buf: ManuallyDrop<Entry<K, V>> =
            unsafe { ManuallyDrop::new(std::ptr::read(entry_ptr)) };
        // Prevent the IR-level non-atomic load (the `ptr::read` body) from
        // being reordered past the `v2` atomic load. On x86 this is a
        // codegen no-op (TSO hides it), but on weaker models or under
        // aggressive LLVM optimisation we need an explicit fence.
        compiler_fence(Ordering::Acquire);
        let v2 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
        if v1 != v2 {
            return Probe::Racing;
        }
        // Validated: `buf` is a consistent snapshot. Safe to call K::eq and
        // V::clone on the deref'd ManuallyDrop fields.
        if <K as Borrow<Q>>::borrow(&*buf.key) == key {
            let v: V = (*buf.value).clone();
            let mask = Self::vbit_mask(pos);
            if self.hot.visited.load(Ordering::Relaxed) & mask == 0 {
                self.hot.visited.fetch_or(mask, Ordering::Relaxed);
            }
            return Probe::Found(v);
        }
        Probe::Miss
    }

    /// Insert `(key, value)`. On a Path C eviction `on_evict` is called with
    /// the evicted (K, V). The call is deferred past in-flight reader pins
    /// when `K` or `V` carries a destructor, and synchronous otherwise.
    /// `on_evict` is never invoked on Path A (in-place update) or Path B
    /// (warmup install).
    pub(crate) fn insert<F>(&self, key: K, value: V, hash: u64, has_avx2_bmi1: bool, on_evict: F)
    where
        F: FnOnce(K, V) + Send + 'static,
    {
        let needle = Self::needle_from_hash(hash);
        match self.try_path_a(&key, needle, value, has_avx2_bmi1) {
            Ok(()) => {
                drop(key);
            }
            Err(value) => self.path_bc(key, value, needle, on_evict),
        }
    }

    /// Path A: lock-free in-place value update for an existing key. Tag
    /// stays untouched (LIVE/ID/HASH all stable); only `entries[id].value`
    /// flips, guarded by the entry-version CAS envelope. The old V's drop
    /// is deferred through `crossbeam-epoch` so any reader who observed it
    /// via `ptr::read` finishes cloning before the heap is freed.
    ///
    /// Single-shot: on CAS contention or lookup miss we hand the value back
    /// to the caller, which falls through to `path_bc` under the mutex.
    /// `path_bc` is the definitive winner for any racing case.
    fn try_path_a(&self, key: &K, needle: u16, value: V, has_avx2_bmi1: bool) -> Result<(), V> {
        let mut value_holder = ManuallyDrop::new(value);
        let Some((pos, id, v_snap)) = self.find_lockfree_for_path_a(key, needle, has_avx2_bmi1)
        else {
            let v = unsafe { ManuallyDrop::take(&mut value_holder) };
            return Err(v);
        };
        let entry_ptr = unsafe { self.entries_mut_ptr().add(id) as *mut Entry<K, V> };
        let version_ref = unsafe { &(*entry_ptr).version };
        if version_ref
            .compare_exchange(
                v_snap,
                v_snap.wrapping_add(1),
                Ordering::Acquire,
                Ordering::Acquire,
            )
            .is_err()
        {
            let v = unsafe { ManuallyDrop::take(&mut value_holder) };
            return Err(v);
        }
        let new_value: V = unsafe { ManuallyDrop::take(&mut value_holder) };
        // SAFETY: odd version excludes both readers and other writers for
        // the duration of the in-place write. The `ManuallyDrop<V>` slot
        // and the raw `V` share the same layout, so casting through
        // `*mut V` and using raw `ptr::read` / `ptr::write` is sound.
        let value_ptr = unsafe { &mut (*entry_ptr).value as *mut ManuallyDrop<V> as *mut V };
        let old_value: V = unsafe { std::ptr::read(value_ptr) };
        unsafe { std::ptr::write(value_ptr, new_value) };
        version_ref.store(v_snap.wrapping_add(2), Ordering::Release);
        let mask = Self::vbit_mask(pos);
        self.hot.visited.fetch_or(mask, Ordering::Relaxed);
        // Defer the old V's drop past every in-flight reader pin.
        defer_drop_if_needed::<V>(old_value);
        Ok(())
    }

    /// Path A pre-scan dispatcher: AVX2 fast path when the host advertises
    /// it, scalar fallback otherwise. See [`Self::find_get`] for the
    /// dispatch contract.
    #[inline]
    fn find_lockfree_for_path_a(
        &self,
        key: &K,
        needle: u16,
        has_avx2_bmi1: bool,
    ) -> Option<(usize, usize, u32)> {
        #[cfg(target_arch = "x86_64")]
        {
            if has_avx2_bmi1 {
                // SAFETY: see `Shard::find_get` — same provenance for the
                // `has_avx2_bmi1` flag.
                return unsafe { self.find_lockfree_for_path_a_avx2(key, needle) };
            }
        }
        let _ = has_avx2_bmi1;
        self.find_lockfree_for_path_a_scalar(key, needle)
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_lockfree_for_path_a_avx2(
        &self,
        key: &K,
        needle: u16,
    ) -> Option<(usize, usize, u32)> {
        use std::arch::x86_64::*;

        let entries_base = self.entries_ptr();
        let tags_ptr = self.tags.as_ptr() as *const u16;
        let needle_v = _mm256_set1_epi16(needle as i16);
        let mask_v = _mm256_set1_epi16(Self::SCAN_MASK as i16);

        let limit = self.tags.len();
        let mut i = 0usize;
        while i < limit {
            // SAFETY: identical bound to `find_get_avx2`; see that
            // function for the full reasoning.
            let v = unsafe { _mm256_loadu_si256(tags_ptr.add(i) as *const __m256i) };
            let masked = _mm256_and_si256(v, mask_v);
            let cmp = _mm256_cmpeq_epi16(masked, needle_v);
            let mut mask = _mm256_movemask_epi8(cmp) as u32;

            while mask != 0 {
                let bit = mask.trailing_zeros() as usize;
                let lane = bit >> 1;
                let pos = i + lane;
                let t1 = self.tags[pos].load(Ordering::Acquire);
                if (t1 & Self::SCAN_MASK) == needle
                    && let Some(found) = self.try_path_a_candidate(pos, t1, key, entries_base)
                {
                    return Some(found);
                }
                mask = _blsr_u32(mask);
                mask = _blsr_u32(mask);
            }
            i += LANE;
        }
        None
    }

    /// Portable twin of [`Self::find_lockfree_for_path_a_avx2`]. The
    /// double-load shape (Relaxed pre-filter → Acquire re-load) mirrors
    /// the AVX2 path's `_mm256_loadu_si256` (unordered, used only for
    /// candidate filtering) followed by the `Acquire` load at `t1` —
    /// the second load is what pairs with the writer's `Release` stores
    /// in `path_bc` for the seqlock contract.
    fn find_lockfree_for_path_a_scalar(&self, key: &K, needle: u16) -> Option<(usize, usize, u32)> {
        let entries_base = self.entries_ptr();
        let limit = self.tags.len();
        for pos in 0..limit {
            let t = self.tags[pos].load(Ordering::Relaxed);
            if (t & Self::SCAN_MASK) != needle {
                continue;
            }
            let t1 = self.tags[pos].load(Ordering::Acquire);
            if (t1 & Self::SCAN_MASK) == needle
                && let Some(found) = self.try_path_a_candidate(pos, t1, key, entries_base)
            {
                return Some(found);
            }
        }
        None
    }

    #[inline]
    fn try_path_a_candidate(
        &self,
        pos: usize,
        t1: u16,
        key: &K,
        entries_base: *const MaybeUninit<Entry<K, V>>,
    ) -> Option<(usize, usize, u32)> {
        let id = Self::id_of(t1);
        let entry_ptr = unsafe { entries_base.add(id) as *const Entry<K, V> };
        let v1 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
        if v1 & 1 != 0 {
            return None;
        }
        let buf: ManuallyDrop<Entry<K, V>> =
            unsafe { ManuallyDrop::new(std::ptr::read(entry_ptr)) };
        compiler_fence(Ordering::Acquire);
        let v2 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
        if v1 != v2 {
            return None;
        }
        if &*buf.key == key {
            return Some((pos, id, v1));
        }
        None
    }

    fn path_bc<F>(&self, key: K, value: V, needle: u16, on_evict: F)
    where
        F: FnOnce(K, V) + Send + 'static,
    {
        let mut state = self.hot.writer.lock();

        if let Some((pos, id)) = self.writer_find(&key, needle) {
            self.writer_update_in_place(pos, id, key, value);
            // `on_evict` is dropped here without being invoked (no eviction
            // happened on a key-update path).
            return;
        }

        let len = self.hot.len.load(Ordering::Relaxed);
        if len < self.capacity {
            self.writer_warmup_install(&mut state, len, key, value, needle);
            return;
        }

        self.writer_evict_and_install(&mut state, key, value, needle, on_evict);
    }

    fn writer_find(&self, key: &K, needle: u16) -> Option<(usize, usize)> {
        let entries_base = self.entries_ptr();
        let len = self.hot.len.load(Ordering::Relaxed);
        for pos in 0..len {
            loop {
                let t = self.tags[pos].load(Ordering::Acquire);
                // We hold the writer mutex, and only writers store EMPTY
                // (via Path C / remove shifts). All those stores happen
                // before `len` is decremented, so `pos < len` precludes
                // EMPTY here. An EMPTY observation would mean a structural
                // invariant break — fail loud rather than spin.
                debug_assert!(
                    t != EMPTY,
                    "writer_find observed EMPTY at pos {pos} < len {len}"
                );
                if (t & LIVE) == 0 {
                    break;
                }
                if (t & Self::SCAN_MASK) != needle {
                    break;
                }
                let id = Self::id_of(t);
                let entry_ptr = unsafe { entries_base.add(id) as *const Entry<K, V> };
                let mut v;
                loop {
                    v = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
                    if v & 1 == 0 {
                        break;
                    }
                    hint::spin_loop();
                }
                let buf: ManuallyDrop<Entry<K, V>> =
                    unsafe { ManuallyDrop::new(std::ptr::read(entry_ptr)) };
                let v2 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
                if v != v2 {
                    continue;
                }
                let t2 = self.tags[pos].load(Ordering::Acquire);
                if t != t2 || (t2 & LIVE) == 0 {
                    continue;
                }
                if &*buf.key == key {
                    return Some((pos, id));
                }
                break;
            }
        }
        None
    }

    /// Q-borrow variant of [`Self::writer_find`] used by `remove`.
    fn writer_find_q<Q>(&self, key: &Q, needle: u16) -> Option<(usize, usize)>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let entries_base = self.entries_ptr();
        let len = self.hot.len.load(Ordering::Relaxed);
        for pos in 0..len {
            loop {
                let t = self.tags[pos].load(Ordering::Acquire);
                debug_assert!(
                    t != EMPTY,
                    "writer_find_q observed EMPTY at pos {pos} < len {len}"
                );
                if (t & LIVE) == 0 {
                    break;
                }
                if (t & Self::SCAN_MASK) != needle {
                    break;
                }
                let id = Self::id_of(t);
                let entry_ptr = unsafe { entries_base.add(id) as *const Entry<K, V> };
                let mut v;
                loop {
                    v = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
                    if v & 1 == 0 {
                        break;
                    }
                    hint::spin_loop();
                }
                let buf: ManuallyDrop<Entry<K, V>> =
                    unsafe { ManuallyDrop::new(std::ptr::read(entry_ptr)) };
                let v2 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
                if v != v2 {
                    continue;
                }
                let t2 = self.tags[pos].load(Ordering::Acquire);
                if t != t2 || (t2 & LIVE) == 0 {
                    continue;
                }
                if <K as Borrow<Q>>::borrow(&*buf.key) == key {
                    return Some((pos, id));
                }
                break;
            }
        }
        None
    }

    fn writer_update_in_place(&self, pos: usize, id: usize, key: K, value: V) {
        let entry_ptr = unsafe { self.entries_mut_ptr().add(id) as *mut Entry<K, V> };
        let version_ref = unsafe { &(*entry_ptr).version };
        let v_claimed = loop {
            let v = version_ref.load(Ordering::Acquire);
            if v & 1 == 0
                && version_ref
                    .compare_exchange(v, v.wrapping_add(1), Ordering::Acquire, Ordering::Acquire)
                    .is_ok()
            {
                break v.wrapping_add(1);
            }
            hint::spin_loop();
        };
        // SAFETY: odd version excludes both readers and Path A writers.
        let value_ptr = unsafe { &mut (*entry_ptr).value as *mut ManuallyDrop<V> as *mut V };
        let old_value: V = unsafe { std::ptr::read(value_ptr) };
        unsafe { std::ptr::write(value_ptr, value) };
        drop(key);
        version_ref.store(v_claimed.wrapping_add(1), Ordering::Release);
        let mask = Self::vbit_mask(pos);
        self.hot.visited.fetch_or(mask, Ordering::Relaxed);
        // Defer the old V's drop past every in-flight reader pin.
        defer_drop_if_needed::<V>(old_value);
    }

    fn writer_warmup_install(
        &self,
        state: &mut WriterState,
        len: usize,
        key: K,
        value: V,
        needle: u16,
    ) {
        let entry_id = match state.free_ids.pop() {
            Some(id) => id,
            None => {
                let id = state.next_fresh_id;
                state.next_fresh_id = id.wrapping_add(1);
                id
            }
        };
        // SAFETY: writer mutex serialises us; `entry_id` was either fresh
        // (uninit) or reclaimed by `remove` (its K / V were taken out via
        // `ManuallyDrop::take` and deferred for drop before the id was
        // pushed onto `free_ids`, so the slot is logically uninit).
        //
        // Re-using a reclaimed id resets `version` from its prior even
        // value `X` back to 0. This is sound only because no reader can
        // reach this slot through the tag scan between the `ptr::write`
        // below and the `tags[len].store(new_tag)` further down:
        //   1. `remove` overwrote every tag pointing at this id (the shift
        //      collapses the freed slot, terminating with the `EMPTY`
        //      sentinel at the new tail) before pushing the id onto
        //      `free_ids`.
        //   2. The new tag is published AFTER `ptr::write` completes, via
        //      the `Release` store on `tags[len]` below (paired with the
        //      `fence(Release)` for the entry write).
        //   3. Therefore any successful reader tag-match here observes the
        //      fresh entry (version 0, key, value) — never the stale
        //      `X`-versioned ghost.
        unsafe {
            let slot_ptr = self.entries_mut_ptr().add(entry_id as usize) as *mut Entry<K, V>;
            std::ptr::write(
                slot_ptr,
                Entry {
                    version: AtomicU32::new(0),
                    key: ManuallyDrop::new(key),
                    value: ManuallyDrop::new(value),
                },
            );
        }
        let mask = Self::vbit_mask(len);
        self.hot.visited.fetch_and(!mask, Ordering::Relaxed);
        let new_tag = LIVE | (entry_id << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        fence(Ordering::Release);
        self.tags[len].store(new_tag, Ordering::Release);
        self.hot.len.store(len + 1, Ordering::Release);
    }

    fn writer_evict_and_install<F>(
        &self,
        state: &mut WriterState,
        key: K,
        value: V,
        needle: u16,
        on_evict: F,
    ) where
        F: FnOnce(K, V) + Send + 'static,
    {
        let cap = self.capacity;
        debug_assert_eq!(self.hot.len.load(Ordering::Relaxed), cap);
        if state.hand >= cap {
            state.hand = 0;
        }
        let evict_pos = self
            .scan_evict(state.hand, cap)
            .or_else(|| self.scan_evict(0, state.hand))
            .unwrap_or(state.hand);
        let evict_tag = self.read_live_tag_with_spin(evict_pos);
        let evict_id = Self::id_of(evict_tag);

        let evict_entry_ptr = unsafe { self.entries_mut_ptr().add(evict_id) as *mut Entry<K, V> };
        let evict_version_ref = unsafe { &(*evict_entry_ptr).version };
        let v_claimed = loop {
            let v = evict_version_ref.load(Ordering::Acquire);
            if v & 1 == 0
                && evict_version_ref
                    .compare_exchange(v, v.wrapping_add(1), Ordering::Acquire, Ordering::Acquire)
                    .is_ok()
            {
                break v.wrapping_add(1);
            }
            hint::spin_loop();
        };

        // SAFETY: odd version excludes readers / Path A.
        let evicted_key: K = unsafe { ManuallyDrop::take(&mut (*evict_entry_ptr).key) };
        let evicted_value: V = unsafe { ManuallyDrop::take(&mut (*evict_entry_ptr).value) };

        for i in evict_pos..(cap - 1) {
            let next_tag = self.read_live_tag_with_spin(i + 1);
            let s_mask = Self::vbit_mask(i + 1);
            let d_mask = Self::vbit_mask(i);
            let was_visited = self.hot.visited.load(Ordering::Relaxed) & s_mask != 0;
            self.hot.visited.fetch_and(!s_mask, Ordering::Relaxed);
            if was_visited {
                self.hot.visited.fetch_or(d_mask, Ordering::Relaxed);
            } else {
                self.hot.visited.fetch_and(!d_mask, Ordering::Relaxed);
            }
            self.tags[i].store(EMPTY, Ordering::Release);
            fence(Ordering::Release);
            self.tags[i].store(next_tag, Ordering::Release);
        }
        self.tags[cap - 1].store(EMPTY, Ordering::Release);

        // SAFETY: odd version still held; the tag at `cap-1` is now EMPTY
        // so readers cannot reach this slot through SIMD scan. Re-install
        // the new K, V into the ManuallyDrop slots.
        unsafe {
            std::ptr::write(&mut (*evict_entry_ptr).key, ManuallyDrop::new(key));
            std::ptr::write(&mut (*evict_entry_ptr).value, ManuallyDrop::new(value));
        }
        evict_version_ref.store(v_claimed.wrapping_add(1), Ordering::Release);

        let mask = Self::vbit_mask(cap - 1);
        self.hot.visited.fetch_and(!mask, Ordering::Relaxed);
        let new_tag = LIVE | ((evict_id as u16) << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        fence(Ordering::Release);
        self.tags[cap - 1].store(new_tag, Ordering::Release);

        state.hand = if evict_pos < cap - 1 { evict_pos } else { 0 };

        self.hot.path_c_epoch.fetch_add(1, Ordering::Release);

        // Hand the evicted (K, V) to the user callback. When either K or V
        // has a destructor, defer the call past every in-flight reader pin
        // so the bit-copies held by `try_candidate` finish their `K::eq`
        // and `V::clone` against still-live allocations (race β + race γ).
        // For Copy K/V we run the closure synchronously — there's no heap
        // for a reader to UAF on.
        if std::mem::needs_drop::<K>() || std::mem::needs_drop::<V>() {
            let guard = epoch::pin();
            // SAFETY: `K, V: Send + 'static` and `F: Send + 'static` make
            // the closure capture sound for off-thread reclaim.
            unsafe {
                guard.defer_unchecked(move || on_evict(evicted_key, evicted_value));
            }
        } else {
            on_evict(evicted_key, evicted_value);
        }
    }

    fn scan_evict(&self, lo: usize, hi: usize) -> Option<usize> {
        for i in lo..hi {
            let t = loop {
                let t = self.tags[i].load(Ordering::Acquire);
                if t == EMPTY {
                    hint::spin_loop();
                    continue;
                }
                break t;
            };
            debug_assert!(
                t & LIVE != 0,
                "scan_evict: tags[{i}] was unexpectedly EMPTY/dead after spin (t = {t:#x})"
            );
            let mask = Self::vbit_mask(i);
            if self.hot.visited.load(Ordering::Relaxed) & mask != 0 {
                self.hot.visited.fetch_and(!mask, Ordering::Relaxed);
            } else {
                return Some(i);
            }
        }
        None
    }

    fn read_live_tag_with_spin(&self, pos: usize) -> u16 {
        loop {
            let t = self.tags[pos].load(Ordering::Acquire);
            if t == EMPTY {
                hint::spin_loop();
                continue;
            }
            return t;
        }
    }

    /// `remove`: cold path under the writer mutex. Locates `key`, claims
    /// the entry's version, takes K / V out of the slot, shifts the tail
    /// of `tags` left by one, decrements `len`, and pushes the freed entry
    /// id onto the free list for the next warmup install. The cloned V is
    /// returned; the original K and V are deferred past in-flight reader
    /// pins (closing race β on V and race γ on K for this path).
    pub(crate) fn remove<Q>(&self, key: &Q, hash: u64) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        let needle = Self::needle_from_hash(hash);
        let mut state = self.hot.writer.lock();
        let (pos, id) = self.writer_find_q::<Q>(key, needle)?;

        let entry_ptr = unsafe { self.entries_mut_ptr().add(id) as *mut Entry<K, V> };
        let version_ref = unsafe { &(*entry_ptr).version };
        let v_claimed = loop {
            let v = version_ref.load(Ordering::Acquire);
            if v & 1 == 0
                && version_ref
                    .compare_exchange(v, v.wrapping_add(1), Ordering::Acquire, Ordering::Acquire)
                    .is_ok()
            {
                break v.wrapping_add(1);
            }
            hint::spin_loop();
        };
        // SAFETY: odd version excludes readers / Path A.
        let removed_key: K = unsafe { ManuallyDrop::take(&mut (*entry_ptr).key) };
        let removed_value: V = unsafe { ManuallyDrop::take(&mut (*entry_ptr).value) };
        let removed_value_for_caller: V = removed_value.clone();
        version_ref.store(v_claimed.wrapping_add(1), Ordering::Release);

        let len = self.hot.len.load(Ordering::Relaxed);
        for i in pos..(len - 1) {
            let next_tag = self.read_live_tag_with_spin(i + 1);
            let s_mask = Self::vbit_mask(i + 1);
            let d_mask = Self::vbit_mask(i);
            let was_visited = self.hot.visited.load(Ordering::Relaxed) & s_mask != 0;
            self.hot.visited.fetch_and(!s_mask, Ordering::Relaxed);
            if was_visited {
                self.hot.visited.fetch_or(d_mask, Ordering::Relaxed);
            } else {
                self.hot.visited.fetch_and(!d_mask, Ordering::Relaxed);
            }
            self.tags[i].store(EMPTY, Ordering::Release);
            fence(Ordering::Release);
            self.tags[i].store(next_tag, Ordering::Release);
        }
        self.tags[len - 1].store(EMPTY, Ordering::Release);
        let tail_mask = Self::vbit_mask(len - 1);
        self.hot.visited.fetch_and(!tail_mask, Ordering::Relaxed);
        self.hot.len.store(len - 1, Ordering::Release);
        self.hot.path_c_epoch.fetch_add(1, Ordering::Release);

        state.free_ids.push(id as u16);
        if state.hand >= self.hot.len.load(Ordering::Relaxed) {
            state.hand = 0;
        }
        // Defer both K and V drops past in-flight reader pins. The cloned
        // V handed back to the caller is independent.
        defer_drop_kv_if_needed::<K, V>(removed_key, removed_value);
        Some(removed_value_for_caller)
    }

    #[cfg(test)]
    pub(crate) fn live_count(&self) -> usize {
        let len = self.hot.len.load(Ordering::Acquire);
        let mut n = 0;
        for i in 0..len {
            let t = self.tags[i].load(Ordering::Acquire);
            if t & LIVE != 0 {
                n += 1;
            }
        }
        n
    }

    #[cfg(test)]
    pub(crate) fn live_ids(&self) -> Vec<usize> {
        let len = self.hot.len.load(Ordering::Acquire);
        let mut ids = Vec::new();
        for i in 0..len {
            let t = self.tags[i].load(Ordering::Acquire);
            if t & LIVE != 0 {
                ids.push(Self::id_of(t));
            }
        }
        ids
    }

    #[cfg(test)]
    pub(crate) fn tag_at(&self, pos: usize) -> u16 {
        self.tags[pos].load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn path_c_epoch_snapshot(&self) -> u64 {
        self.hot.path_c_epoch.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn visited_snapshot(&self) -> u64 {
        self.hot.visited.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn entry_version(&self, id: usize) -> u32 {
        let entry_ptr = unsafe { self.entries_ptr().add(id) as *const Entry<K, V> };
        unsafe { (*entry_ptr).version.load(Ordering::Acquire) }
    }
}

impl<K, V> Drop for Shard<K, V> {
    fn drop(&mut self) {
        // Flush any pending epoch-deferred K / V drops scheduled by Path A
        // / Path C / `remove`. Without this, a process-exit before the
        // GC's next collection would leak the deferred allocations.
        epoch::pin().flush();
        let len = self.hot.len.load(Ordering::Relaxed);
        let entries_mut = self.entries.get();
        for i in 0..len {
            let t = self.tags[i].load(Ordering::Relaxed);
            if t & LIVE != 0 {
                let id = Self::id_of(t);
                // SAFETY: LIVE ⇒ entries[id] is initialised. The slot
                // holds `ManuallyDrop<K>` and `ManuallyDrop<V>` so we must
                // explicitly drop each field — `assume_init_drop` on the
                // whole `Entry` would be a no-op for the ManuallyDrop
                // fields and would leak K / V.
                unsafe {
                    let entry: &mut Entry<K, V> = (*entries_mut)[id].assume_init_mut();
                    ManuallyDrop::drop(&mut entry.key);
                    ManuallyDrop::drop(&mut entry.value);
                }
            }
        }
    }
}

// Compile-time sanity: the public `MAX_PER_SHARD` matches the layout-derived ceiling.
const _: () = {
    assert!(MAX_PER_SHARD == 64);
    assert!(LIVE == 0x8000);
};

#[cfg(test)]
impl<K, V> Shard<K, V>
where
    K: Hash + Eq,
    V: Copy,
{
    pub(crate) fn id_shift() -> u32 {
        Self::ID_SHIFT
    }
    pub(crate) fn id_mask() -> u16 {
        Self::ID_MASK
    }
    pub(crate) fn hash_mask() -> u16 {
        Self::HASH_MASK
    }
    pub(crate) fn scan_mask() -> u16 {
        Self::SCAN_MASK
    }
    pub(crate) fn id_of_pub(tag: u16) -> usize {
        Self::id_of(tag)
    }
    pub(crate) fn vbit_mask_pub(pos: usize) -> u64 {
        Self::vbit_mask(pos)
    }
    pub(crate) fn live_bit_pub() -> u16 {
        LIVE
    }
    pub(crate) fn entry_size_pub() -> usize {
        Self::ENTRY_SIZE
    }
    pub(crate) fn shard_hot_size_pub() -> usize {
        std::mem::size_of::<ShardHot>()
    }
    pub(crate) fn shard_hot_align_pub() -> usize {
        std::mem::align_of::<ShardHot>()
    }
}
