//! Thread-safe SIEVE caches.
//!
//! Two designs ship behind the `concurrent` Cargo feature, occupying
//! different points on the HR-vs-throughput Pareto frontier:
//!
//! - [`Cache`] — sharded, lock-free-reader SIEVE (promoted from the `c17s`
//!   research variant). AVX2 SIMD tag scan + entry-version seqlock on the
//!   reader side; per-shard `parking_lot::Mutex` only for Path B/C
//!   writers. **`x86_64 + AVX2` only**; the type compiles out on other
//!   targets. Pareto-dominates `PartitionedCache` on HR-preserving
//!   workloads (Twitter session traces, ARC OLTP/DS1) by 2-7×
//!   ([`docs/reports/2026-05-13-c17s-shard-heuristic.md`]).
//! - [`PartitionedCache`] — `N` independent [`crate::Cache`] instances
//!   behind one `Mutex` each, routed by a per-thread routing hint. Simpler
//!   model but loses HR on shared hot keys.
//!   ([`docs/reports/2026-05-12-partitioned-design.md`]).
//!
//! Both rely on [`parking_lot::Mutex`] for the uncontended fast path
//! (~5 ns vs ~10-15 ns for `std::sync::Mutex` on Linux glibc).

#[cfg(all(target_arch = "x86_64", not(miri)))]
mod cache;
#[cfg(all(target_arch = "x86_64", not(miri)))]
pub use cache::Cache;

use std::borrow::Borrow;
use std::cell::Cell;
use std::hash::{BuildHasher, Hash};
use std::sync::atomic::{AtomicU32, Ordering};

use parking_lot::Mutex;

use crate::{Cache as LibCache, Slot32, SlotSize, Xxh3Build};

// ---------- routing hint allocator ----------

/// Sentinel for "thread has not yet been assigned a hint". `u32::MAX` is
/// reserved so the allocator's hot path is a single TLS load plus a
/// predicted-not-taken branch.
const UNASSIGNED: u32 = u32::MAX;

static NEXT_ROUTING_HINT: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static TLS_ROUTING_HINT: Cell<u32> = const { Cell::new(UNASSIGNED) };
}

/// Returns the calling thread's routing hint — a `u32` used to bias partition
/// selection. Lazily allocated on first call and stable for the thread's
/// lifetime thereafter.
///
/// **Not a unique id.** Uniqueness is best-effort, not a contract: routing
/// only needs each thread to land on *some* partition consistently, not on a
/// distinct one. The allocator hands out monotonically increasing values, so
/// in any realistic process every thread does get a distinct hint, but the
/// type itself does not depend on that — if two threads ever collided they
/// would simply share a partition (extra contention, no correctness impact).
///
/// The `assert!` below catches `u32::MAX` because that value is reserved as
/// the "not-yet-assigned" sentinel; writing it to TLS would re-trigger the
/// allocator on every call. Reaching `2^32` threads in a single process is
/// not a real concern.
#[inline]
fn routing_hint() -> u32 {
    TLS_ROUTING_HINT.with(|cell| {
        let hint = cell.get();
        if hint != UNASSIGNED {
            hint
        } else {
            let new_hint = NEXT_ROUTING_HINT.fetch_add(1, Ordering::Relaxed);
            assert!(
                new_hint < UNASSIGNED,
                "senba::concurrent: routing-hint allocator returned the reserved sentinel u32::MAX"
            );
            cell.set(new_hint);
            new_hint
        }
    })
}

// ---------- PartitionedCache ----------

/// `N` independent [`Cache`] instances behind a `Mutex` each, routed by a
/// per-thread routing hint. The simplest possible parallel baseline: every
/// operation is "compute the partition index from the calling thread's
/// routing hint, take that partition's mutex, run the corresponding
/// [`Cache`] method".
///
/// Routing is **per-thread, not key-hash based**: the same key observed on
/// different threads lands in different partitions and may be cached
/// independently in each. This loses the global "one entry per key"
/// invariant in exchange for zero cross-partition coordination. Workloads
/// with thread-local working sets pay no HR penalty; workloads with global
/// hot keys may see up to `partitions()`-way duplication.
///
/// ## When this is the right choice
///
/// - You can scale by spinning up more partitions until each thread is
///   uncontended (`partitions() >= num_threads`).
/// - Your workload's hot key set either fits per-thread, or you can absorb
///   the duplication.
/// - You want a stable surface that's not coupled to specific concurrency
///   tricks (seqlocks, epoch reclamation, etc.).
///
/// ## When to prefer something else
///
/// - **HR-critical workloads with shared hot keys** (database OLTP traces,
///   shared session caches): the experimental `c*` / `r*` variants in
///   `senba-research` preserve HR exactly.
/// - **Single-threaded use**: just use [`Cache`] directly — wrapping in
///   `Mutex` adds an unnecessary lock acquire per op.
///
/// ```
/// use senba::concurrent::PartitionedCache;
/// use std::sync::Arc;
///
/// // 4 partitions, total capacity 16 (4 per partition).
/// let cache: Arc<PartitionedCache<u64, String>> = Arc::new(PartitionedCache::new(16, 4));
/// cache.insert(1, "hello".into());
/// assert_eq!(cache.get(&1), Some("hello".to_string()));
/// assert_eq!(cache.partitions(), 4);
/// ```
pub struct PartitionedCache<K, V, S: SlotSize = Slot32, H: BuildHasher = Xxh3Build> {
    partitions: Box<[Partition<K, V, S, H>]>,
    /// `partitions.len() - 1`. Cached so `partition_of` is a single AND.
    /// Power-of-two `partitions` count is asserted at construction time.
    partition_mask: usize,
}

/// A single underlying [`Cache`] guarded by its own `Mutex`. Kept as a
/// type alias mostly to give the field above a clippy-friendly signature.
type Partition<K, V, S, H> = Mutex<LibCache<K, V, S, H>>;

impl<K, V, S> PartitionedCache<K, V, S, Xxh3Build>
where
    K: Hash + Eq,
    S: SlotSize,
{
    /// Creates a partitioned cache with `capacity` total entries split
    /// across `partitions` independent [`Cache`] instances, using the
    /// default [`Xxh3Build`] hasher.
    ///
    /// `partitions` must be a power of two (so the routing reduces to a
    /// single AND) and `>= 1`. `capacity` must be `>= partitions` so every
    /// partition holds at least one entry. The remainder when `capacity`
    /// does not divide evenly is distributed across the first
    /// `capacity % partitions` partitions (one extra slot each), matching
    /// [`Cache::with_shards`].
    pub fn new(capacity: usize, partitions: usize) -> Self {
        Self::with_hasher(capacity, partitions, Xxh3Build)
    }
}

impl<K, V, S, H> PartitionedCache<K, V, S, H>
where
    K: Hash + Eq,
    S: SlotSize,
    H: BuildHasher + Clone,
{
    /// Creates a partitioned cache with the supplied [`BuildHasher`]. The
    /// hasher is cloned once per partition (so the same seed/state lands
    /// in every underlying [`Cache`]).
    pub fn with_hasher(capacity: usize, partitions: usize, hasher: H) -> Self {
        assert!(partitions > 0, "partitions must be > 0");
        assert!(
            partitions.is_power_of_two(),
            "partitions ({partitions}) must be a power of two so routing can be a bit mask"
        );
        assert!(
            capacity >= partitions,
            "capacity ({capacity}) must be >= partitions ({partitions}) so each partition has cap >= 1"
        );
        let base = capacity / partitions;
        let extra = capacity % partitions;
        let built: Vec<Partition<K, V, S, H>> = (0..partitions)
            .map(|i| {
                let cap_i = base + if i < extra { 1 } else { 0 };
                Mutex::new(LibCache::with_hasher(cap_i, hasher.clone()))
            })
            .collect();
        Self {
            partitions: built.into_boxed_slice(),
            partition_mask: partitions - 1,
        }
    }

    /// Number of partitions. Always a power of two and fixed at construction.
    #[inline]
    pub fn partitions(&self) -> usize {
        self.partition_mask + 1
    }

    /// Total capacity summed across every partition (fixed at construction).
    pub fn capacity(&self) -> usize {
        self.partitions.iter().map(|p| p.lock().capacity()).sum()
    }

    /// Number of live entries summed across every partition. Snapshot value:
    /// concurrent inserts on other partitions can change before this
    /// returns. Locks every partition's mutex in turn.
    pub fn len(&self) -> usize {
        self.partitions.iter().map(|p| p.lock().len()).sum()
    }

    /// `true` when every partition is empty. Like [`Self::len`], a snapshot.
    pub fn is_empty(&self) -> bool {
        self.partitions.iter().all(|p| p.lock().is_empty())
    }

    /// Picks the partition index for the calling thread. Routing-hint based:
    /// every call from the same thread returns the same index for the
    /// lifetime of the process.
    #[inline]
    fn partition_of(&self) -> usize {
        (routing_hint() as usize) & self.partition_mask
    }

    /// Returns a clone of the value for `key` from the calling thread's
    /// partition, or `None` if absent there. Sets the SIEVE VISITED bit on
    /// hit (same semantics as [`Cache::get`]) within that partition.
    ///
    /// Note: a key may be present in some partitions and absent in others
    /// because partitions do not coordinate. A miss here only means
    /// "absent in this thread's partition", not "absent globally".
    pub fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        V: Clone,
    {
        let i = self.partition_of();
        self.partitions[i].lock().get(key).cloned()
    }

    /// Non-promoting lookup — returns a clone of the value without setting
    /// VISITED. Same partition selection as [`Self::get`].
    pub fn peek<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        V: Clone,
    {
        let i = self.partition_of();
        self.partitions[i].lock().peek(key).cloned()
    }

    /// Inserts `(key, value)` into the calling thread's partition. Returns
    /// the previous `(K, V)` if the key was already present in that
    /// partition (replacement), or the SIEVE-chosen victim `(K, V)` if the
    /// partition was full, or `None` if it merely filled empty space.
    ///
    /// Equivalent to [`Cache::insert`] on the chosen partition.
    pub fn insert(&self, key: K, value: V) -> Option<(K, V)> {
        let i = self.partition_of();
        self.partitions[i].lock().insert(key, value)
    }

    /// Removes the entry for `key` from the calling thread's partition and
    /// returns its value, or `None` if absent there.
    pub fn remove<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let i = self.partition_of();
        self.partitions[i].lock().remove(key)
    }

    /// Returns `true` when `key` is currently in the calling thread's
    /// partition. Non-promoting, like [`Cache::contains_key`].
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let i = self.partition_of();
        self.partitions[i].lock().contains_key(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn new_distributes_capacity_evenly() {
        let c: PartitionedCache<u64, u64> = PartitionedCache::new(100, 4);
        assert_eq!(c.capacity(), 100);
        assert_eq!(c.partitions(), 4);
    }

    #[test]
    fn new_handles_capacity_remainder() {
        let c: PartitionedCache<u64, u64> = PartitionedCache::new(103, 4);
        // Sum of per-partition capacities must equal the requested total
        // (matches Cache::with_shards's "first `extra` partitions get +1" rule).
        assert_eq!(c.capacity(), 103);
        let per: Vec<usize> = c.partitions.iter().map(|p| p.lock().capacity()).collect();
        assert_eq!(per, vec![26, 26, 26, 25]);
    }

    #[test]
    #[should_panic(expected = "partitions")]
    fn non_power_of_two_partitions_panics() {
        let _: PartitionedCache<u64, u64> = PartitionedCache::new(12, 3);
    }

    #[test]
    #[should_panic(expected = "capacity")]
    fn capacity_smaller_than_partitions_panics() {
        let _: PartitionedCache<u64, u64> = PartitionedCache::new(2, 4);
    }

    #[test]
    fn single_thread_basic_round_trip() {
        // A single thread routes every op to exactly one partition (routing
        // hint is fixed for the lifetime of the test). With 4 partitions and cap=16,
        // that partition holds 4 entries — so insert only up to that cap to
        // avoid SIEVE eviction interfering with the round-trip check.
        let c: PartitionedCache<u64, u64> = PartitionedCache::new(16, 4);
        let per_partition_cap = 16 / 4;
        for k in 0u64..(per_partition_cap as u64) {
            c.insert(k, k * 10);
        }
        for k in 0u64..(per_partition_cap as u64) {
            assert_eq!(c.get(&k), Some(k * 10));
        }
        assert_eq!(c.peek(&0), Some(0));
        assert_eq!(c.remove(&0), Some(0));
        assert_eq!(c.get(&0), None);
        assert!(c.contains_key(&1));
    }

    /// Same key inserted from two distinct threads should end up in two
    /// distinct partitions when their routing hints mask differently. With 16
    /// partitions and only 2 spawned threads, a single attempt has a 1/16
    /// chance of routing both to the same partition by coincidence, so we
    /// retry until we observe the duplication.
    #[test]
    fn thread_routing_can_duplicate_same_key_across_partitions() {
        let mut saw_duplicate_partitions = false;
        for _ in 0..8 {
            let cache: Arc<PartitionedCache<u64, u64>> = Arc::new(PartitionedCache::new(64, 16));
            let c1 = Arc::clone(&cache);
            let c2 = Arc::clone(&cache);
            let h1 = std::thread::spawn(move || {
                c1.insert(42, 100);
                c1.partition_of()
            });
            let h2 = std::thread::spawn(move || {
                c2.insert(42, 200);
                c2.partition_of()
            });
            let p1 = h1.join().unwrap();
            let p2 = h2.join().unwrap();
            if p1 != p2 {
                saw_duplicate_partitions = true;
                break;
            }
        }
        assert!(
            saw_duplicate_partitions,
            "expected at least one trial with two threads routed to different partitions"
        );
    }

    /// Concurrent invariants smoke: 16 threads pound a partitioned cache
    /// with Zipf-like traffic. `threads >= 2 * partitions()` forces partition
    /// collisions by pigeonhole, so each partition gets at least two writers
    /// and the per-partition `Mutex` actually has to serialize cross-thread
    /// access — without that, every partition would be touched by a single
    /// writer and the test would never exercise the contended path.
    #[test]
    fn concurrent_invariants_smoke() {
        let cap = 256usize;
        let partitions = 8;
        let threads = 16u64; // >= 2 * partitions to force collisions
        let cache: Arc<PartitionedCache<u64, u64>> =
            Arc::new(PartitionedCache::new(cap, partitions));

        std::thread::scope(|s| {
            for tid in 0..threads {
                let c = Arc::clone(&cache);
                s.spawn(move || {
                    // Tiny LCG to avoid pulling in rand for a lib unit test;
                    // good enough for a smoke check.
                    let mut state: u64 = 0x9E3779B97F4A7C15u64 ^ tid;
                    for _ in 0..20_000 {
                        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                        // Zipf-ish skew: square the upper bits → top ~1024
                        // keys hit roughly half the time.
                        let k = (state >> 32) % 1024;
                        if let Some(v) = c.get(&k) {
                            assert_eq!(v, k, "value corruption at key {k}");
                        } else {
                            c.insert(k, k);
                        }
                    }
                });
            }
        });

        // Per-partition capacity contract is strictly stronger than the
        // aggregate `total_len <= cap` check — verify it directly.
        for (i, p) in cache.partitions.iter().enumerate() {
            let p = p.lock();
            assert!(
                p.len() <= p.capacity(),
                "partition {i}: len {} > capacity {}",
                p.len(),
                p.capacity()
            );
        }
    }

    /// Mixed insert + remove under contention. Some threads only insert,
    /// some only remove the same key range. The cache must (a) stay within
    /// per-partition capacity at all times the snapshot is taken, and (b)
    /// never hand back a corrupted value — `get(&k)` must return `Some(k)`
    /// or `None`.
    #[test]
    fn concurrent_insert_remove_preserves_invariants() {
        let cap = 64usize;
        let partitions = 4;
        let cache: Arc<PartitionedCache<u64, u64>> =
            Arc::new(PartitionedCache::new(cap, partitions));

        const KEYS: u64 = 256;
        const ITERS: usize = 10_000;

        std::thread::scope(|s| {
            for tid in 0u64..8 {
                let c = Arc::clone(&cache);
                let insert_thread = tid % 2 == 0;
                s.spawn(move || {
                    let mut state: u64 = 0xDEADBEEFCAFEBABEu64 ^ tid;
                    for _ in 0..ITERS {
                        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                        let k = (state >> 32) % KEYS;
                        if insert_thread {
                            c.insert(k, k);
                        } else {
                            c.remove(&k);
                        }
                        if let Some(v) = c.get(&k) {
                            assert_eq!(v, k, "value corruption at key {k}");
                        }
                    }
                });
            }
        });

        for (i, p) in cache.partitions.iter().enumerate() {
            let p = p.lock();
            assert!(
                p.len() <= p.capacity(),
                "partition {i}: len {} > capacity {}",
                p.len(),
                p.capacity()
            );
        }
    }
}
