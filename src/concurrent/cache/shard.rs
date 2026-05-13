//! Per-shard concurrent SIEVE state machine, ported from the `c17s` research
//! variant (`research/src/experimental/sieve_c17s.rs`).
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
//! used (c17s removed it; the entry seqlock subsumes its role), so reader
//! needle-match collides only on the 9 hash bits.
//!
//! ## Soundness model: `Arc<V>` + `crossbeam-epoch`
//!
//! The reader's `ptr::read(entry_ptr)` is a bit-wise copy of the live
//! `Entry<K, Arc<V>>` — its `value: Arc<V>` field is a bit-copy `Arc`
//! sharing the live `ArcInner`'s refcount. To clone `V` out of it safely
//! the reader needs the `ArcInner` to stay allocated past the writer's
//! drop. We get that from `crossbeam-epoch`:
//!
//! 1. Reader pins the local epoch on entry (`get_by_hash`).
//! 2. After the seqlock validates, reader bumps the strong count
//!    (`Arc::increment_strong_count` on the bit-copy's inner pointer)
//!    before calling `V::clone` — `Arc::from_raw` reconstructs the owned
//!    handle and drops it after the clone, decrementing back.
//! 3. Writers (Path A in-place replace, Path C evict + install, `remove`)
//!    `Guard::defer_unchecked` the old `Arc<V>`'s drop instead of
//!    decrementing immediately. The deferred drop fires only after every
//!    pin held at the moment of `defer` is released, so any reader who
//!    had observed the old `Arc` has finished cloning before the
//!    `ArcInner` is freed.
//!
//! The result: `V: Clone + Send + Sync + 'static` is sound (matching
//! `moka::sync::Cache<K, V>`), with the `Arc<V>` indirection adding a
//! single pointer-sized field per entry and ~3–5 ns of `epoch::pin`
//! overhead on the hot reader path.

use crossbeam_epoch as epoch;
use parking_lot::Mutex;
use std::borrow::Borrow;
use std::cell::UnsafeCell;
use std::hash::Hash;
use std::hint;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering, fence};

/// `EMPTY` tag (LIVE bit cleared). Used for the Path C shift transient and
/// for the permanent pad lanes past `capacity`. Path A never writes EMPTY.
const EMPTY: u16 = 0;
/// LIVE bit. Set when the tag's referenced entry id is initialised.
const LIVE: u16 = 0x8000;
/// AVX2 chunk = 32 bytes = 16 `u16` lanes.
const LANE: usize = 16;
/// 6-bit entry id ceiling. The per-shard capacity must be `<= MAX_PER_SHARD`.
pub(crate) const MAX_PER_SHARD: usize = 64;

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
/// natural alignment of `K` / `Arc<V>`. `version` lives at offset 0 so the
/// seqlock load is the first cache line touched. `value` is `Arc<V>` so
/// readers can extend the value's lifetime past a writer's drop via
/// epoch-deferred reclamation (module doc).
#[repr(C, align(32))]
struct Entry<K, V> {
    /// Even = stable, odd = writer in flight. Path A and Path C write the
    /// `value` (and Path C the `key`) inside a `CAS(v, v+1) → write →
    /// store(v+2)` envelope.
    version: AtomicU32,
    key: K,
    value: Arc<V>,
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
// synchronised arena. `Arc<V>: Send + Sync` requires `V: Send + Sync`, so
// that lifts to the Shard as well.
unsafe impl<K: Send + Sync, V: Send + Sync> Send for Shard<K, V> {}
unsafe impl<K: Send + Sync, V: Send + Sync> Sync for Shard<K, V> {}

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
        assert!(
            std::is_x86_feature_detected!("avx2"),
            "senba::concurrent::Cache: AVX2 required (compile-time gated to x86_64+non-miri but runtime CPU lacks AVX2)"
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
    K: Hash + Eq,
    V: Clone + Send + Sync + 'static,
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

    pub(crate) fn contains<Q>(&self, key: &Q, hash: u64) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.get_by_hash::<Q>(key, hash).is_some()
    }

    pub(crate) fn get_by_hash<Q>(&self, key: &Q, hash: u64) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        const MAX_READER_RETRY: usize = 4;
        let needle = Self::needle_from_hash(hash);
        // Pin the local epoch for the duration of the lookup. Any
        // `Arc<V>` we read out of `entries[id]` keeps its `ArcInner`
        // alive past a concurrent writer's `defer_unchecked` drop as long
        // as this pin is held.
        let _guard = epoch::pin();
        for attempt in 0..MAX_READER_RETRY {
            let epoch_before = self.hot.path_c_epoch.load(Ordering::Acquire);
            // SAFETY: AVX2 verified in `Shard::new`.
            let (v, racing) = unsafe { self.find_get::<Q>(key, needle) };
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

    /// AVX2 reader scan. EMPTY-lane detection is intentionally absent — Path A
    /// never writes EMPTY, and Path C transients are caught via `path_c_epoch`.
    /// The `pos < len` filter is similarly skipped (the pad zone is permanent
    /// EMPTY so the LIVE-bit prefix in `SCAN_MASK` rejects it).
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_get<Q>(&self, key: &Q, needle: u16) -> (Option<V>, bool)
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
        // ownership of the live K and `Arc<V>`; the local copy must not
        // double-free (we'd otherwise decrement the strong count twice).
        let buf: ManuallyDrop<Entry<K, V>> =
            unsafe { ManuallyDrop::new(std::ptr::read(entry_ptr)) };
        let v2 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
        if v1 != v2 {
            return Probe::Racing;
        }
        if <K as Borrow<Q>>::borrow(&buf.key) == key {
            // The bit-copy `buf.value: Arc<V>` shares the original
            // `ArcInner`'s refcount. Bump the strong count first, then
            // reconstruct an owned `Arc<V>` from the same pointer and
            // clone `V` out of it. The epoch pin in `get_by_hash` keeps
            // the `ArcInner` allocated even if a writer concurrently
            // schedules its drop via `Guard::defer_unchecked`.
            let arc_ptr: *const V = Arc::as_ptr(&buf.value);
            // SAFETY: epoch pin held by the caller (`get_by_hash`) holds
            // back any writer-deferred drop of this `ArcInner`. The
            // pointer was obtained from a `&Arc<V>` we just read out of
            // a seqlock-validated `Entry<K, V>`.
            unsafe { Arc::increment_strong_count(arc_ptr) };
            let owned: Arc<V> = unsafe { Arc::from_raw(arc_ptr) };
            let v: V = (*owned).clone();
            // `owned` drops here, decrementing the strong count we just
            // added. The bit-copy in `buf` stays inside `ManuallyDrop`
            // and never decrements, so the original refcount is
            // unchanged.
            let mask = Self::vbit_mask(pos);
            if self.hot.visited.load(Ordering::Relaxed) & mask == 0 {
                self.hot.visited.fetch_or(mask, Ordering::Relaxed);
            }
            return Probe::Found(v);
        }
        Probe::Miss
    }

    pub(crate) fn insert(&self, key: K, value: V, hash: u64) -> Option<(K, V)> {
        let needle = Self::needle_from_hash(hash);
        match self.try_path_a(&key, needle, value) {
            Ok(()) => {
                drop(key);
                None
            }
            Err(value) => self.path_bc(key, value, needle),
        }
    }

    /// Path A: lock-free in-place value update for an existing key. Tag
    /// stays untouched (LIVE/ID/HASH all stable); only `entries[id].value`
    /// flips, guarded by the entry-version CAS envelope. The old `Arc<V>`
    /// drop is deferred through `crossbeam-epoch` so any reader who
    /// observed it via `ptr::read` finishes cloning before the
    /// `ArcInner` is freed.
    fn try_path_a(&self, key: &K, needle: u16, value: V) -> Result<(), V> {
        const MAX_RETRY: usize = 1;
        let mut value_holder = ManuallyDrop::new(value);
        for _ in 0..MAX_RETRY {
            // SAFETY: AVX2 verified in `Shard::new`.
            let found = unsafe { self.find_lockfree_for_path_a(key, needle) };
            let (pos, id, v_snap) = match found {
                Some(x) => x,
                None => {
                    let v = unsafe { ManuallyDrop::take(&mut value_holder) };
                    return Err(v);
                }
            };
            let entry_ptr = unsafe { self.entries_mut_ptr().add(id) as *mut Entry<K, V> };
            let version_ref = unsafe { &(*entry_ptr).version };
            match version_ref.compare_exchange(
                v_snap,
                v_snap.wrapping_add(1),
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Ok(_) => {}
                Err(_) => continue,
            }
            let new_value = unsafe { ManuallyDrop::take(&mut value_holder) };
            let new_arc = Arc::new(new_value);
            // SAFETY: odd version excludes both readers and other writers
            // for the duration of the in-place write.
            let old_arc: Arc<V> = unsafe { std::ptr::read(&(*entry_ptr).value) };
            unsafe {
                std::ptr::write(&mut (*entry_ptr).value, new_arc);
            }
            version_ref.store(v_snap.wrapping_add(2), Ordering::Release);
            let mask = Self::vbit_mask(pos);
            self.hot.visited.fetch_or(mask, Ordering::Relaxed);
            // Defer the old Arc's drop past every in-flight reader pin.
            let guard = epoch::pin();
            // SAFETY: the closure captures `old_arc` by move and is run on
            // a thread that has pinned epoch ≥ the pin epoch at this call,
            // matching crossbeam-epoch's reclamation contract.
            unsafe { guard.defer_unchecked(move || drop(old_arc)) };
            return Ok(());
        }
        let v = unsafe { ManuallyDrop::take(&mut value_holder) };
        Err(v)
    }

    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_lockfree_for_path_a(&self, key: &K, needle: u16) -> Option<(usize, usize, u32)> {
        use std::arch::x86_64::*;

        let entries_base = self.entries_ptr();
        let tags_ptr = self.tags.as_ptr() as *const u16;
        let needle_v = _mm256_set1_epi16(needle as i16);
        let mask_v = _mm256_set1_epi16(Self::SCAN_MASK as i16);

        let limit = self.tags.len();
        let mut i = 0usize;
        while i < limit {
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
        let v2 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
        if v1 != v2 {
            return None;
        }
        if buf.key == *key {
            return Some((pos, id, v1));
        }
        None
    }

    fn path_bc(&self, key: K, value: V, needle: u16) -> Option<(K, V)> {
        let mut state = self.hot.writer.lock();

        if let Some((pos, id)) = self.writer_find(&key, needle) {
            self.writer_update_in_place(pos, id, key, value);
            return None;
        }

        let len = self.hot.len.load(Ordering::Relaxed);
        if len < self.capacity {
            self.writer_warmup_install(&mut state, len, key, value, needle);
            return None;
        }

        Some(self.writer_evict_and_install(&mut state, key, value, needle))
    }

    fn writer_find(&self, key: &K, needle: u16) -> Option<(usize, usize)> {
        let entries_base = self.entries_ptr();
        let len = self.hot.len.load(Ordering::Relaxed);
        for pos in 0..len {
            loop {
                let t = self.tags[pos].load(Ordering::Acquire);
                if t == EMPTY {
                    hint::spin_loop();
                    continue;
                }
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
                if buf.key == *key {
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
                if t == EMPTY {
                    hint::spin_loop();
                    continue;
                }
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
                if <K as Borrow<Q>>::borrow(&buf.key) == key {
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
        let new_arc = Arc::new(value);
        // SAFETY: odd version excludes both readers and Path A writers.
        let old_arc: Arc<V> = unsafe { std::ptr::read(&(*entry_ptr).value) };
        unsafe { std::ptr::write(&mut (*entry_ptr).value, new_arc) };
        drop(key);
        version_ref.store(v_claimed.wrapping_add(1), Ordering::Release);
        let mask = Self::vbit_mask(pos);
        self.hot.visited.fetch_or(mask, Ordering::Relaxed);
        let guard = epoch::pin();
        // SAFETY: see `try_path_a`; same reclamation contract applies.
        unsafe { guard.defer_unchecked(move || drop(old_arc)) };
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
        // (uninit) or reclaimed by `remove` (dropped before being pushed).
        unsafe {
            let slot_ptr = self.entries_mut_ptr().add(entry_id as usize) as *mut Entry<K, V>;
            std::ptr::write(
                slot_ptr,
                Entry {
                    version: AtomicU32::new(0),
                    key,
                    value: Arc::new(value),
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

    fn writer_evict_and_install(
        &self,
        state: &mut WriterState,
        key: K,
        value: V,
        needle: u16,
    ) -> (K, V) {
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
        let evicted_key: K = unsafe { std::ptr::read(&(*evict_entry_ptr).key) };
        let evicted_arc: Arc<V> = unsafe { std::ptr::read(&(*evict_entry_ptr).value) };
        // Clone the value to hand back to the caller; the `Arc<V>` itself
        // is deferred to the epoch GC at the end of this function so any
        // in-flight reader gets to finish its own clone.
        let evicted_value: V = (*evicted_arc).clone();

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

        let new_arc = Arc::new(value);
        // SAFETY: odd version still held; the tag at `cap-1` is now EMPTY
        // so readers cannot reach this slot through SIMD scan.
        unsafe {
            std::ptr::write(&mut (*evict_entry_ptr).key, key);
            std::ptr::write(&mut (*evict_entry_ptr).value, new_arc);
        }
        evict_version_ref.store(v_claimed.wrapping_add(1), Ordering::Release);

        let mask = Self::vbit_mask(cap - 1);
        self.hot.visited.fetch_and(!mask, Ordering::Relaxed);
        let new_tag = LIVE | ((evict_id as u16) << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        fence(Ordering::Release);
        self.tags[cap - 1].store(new_tag, Ordering::Release);

        state.hand = if evict_pos < cap - 1 { evict_pos } else { 0 };

        self.hot.path_c_epoch.fetch_add(1, Ordering::Release);

        // Defer the old `Arc<V>` drop past in-flight reader pins.
        let guard = epoch::pin();
        // SAFETY: see `try_path_a`.
        unsafe { guard.defer_unchecked(move || drop(evicted_arc)) };

        (evicted_key, evicted_value)
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
    /// the entry's version, drops the K/V (returning V to the caller),
    /// shifts the tail of `tags` left by one, decrements `len`, and pushes
    /// the freed entry id onto the free list for the next warmup install.
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
        let removed_key: K = unsafe { std::ptr::read(&(*entry_ptr).key) };
        let removed_arc: Arc<V> = unsafe { std::ptr::read(&(*entry_ptr).value) };
        let removed_value: V = (*removed_arc).clone();
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
        drop(removed_key);
        // Defer the `Arc<V>` drop past in-flight reader pins.
        let guard = epoch::pin();
        // SAFETY: see `try_path_a`.
        unsafe { guard.defer_unchecked(move || drop(removed_arc)) };
        Some(removed_value)
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
        let len = self.hot.len.load(Ordering::Relaxed);
        let entries_mut = self.entries.get();
        for i in 0..len {
            let t = self.tags[i].load(Ordering::Relaxed);
            if t & LIVE != 0 {
                let id = Self::id_of(t);
                // SAFETY: LIVE ⇒ entries[id] is initialised.
                unsafe {
                    (*entries_mut)[id].assume_init_drop();
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
