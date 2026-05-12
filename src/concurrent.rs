//! Concurrent wrappers around [`crate::Cache`].
//!
//! The single-threaded [`crate::Cache`] is `&mut self`-driven by design (the
//! SIEVE state machine touches `visited`, `hand`, and the tags array on every
//! op). This module provides thread-safe wrappers built on top of that surface
//! without changing the underlying `Cache` invariants — concurrency is layered
//! externally.
//!
//! ## What ships here
//!
//! - [`PartitionedCache`] — `N` independent [`Cache`] instances behind one
//!   `Mutex` each, routed by thread-id. The simplest possible parallel
//!   baseline: each thread owns one partition under the uncontended fast path,
//!   and the per-op cost reduces to "uncontended mutex acquire + lib op".
//!
//! Design rationale: see `docs/reports/2026-05-12-partitioned-design.md`.
//! The (T × N) sweep methodology — threads and partition count vary
//! independently — is part of the contract for this type.
//!
//! ## What does **not** ship here
//!
//! Lock-free / seqlock / epoch-based concurrent SIEVE variants live in
//! `senba-research::experimental` (the `c*` / `r*` series). They preserve HR
//! exactly at the cost of a more elaborate state machine; the partitioned
//! cache trades HR (same key may duplicate across partitions) for radical
//! simplicity. The two designs cover different points on the HR-vs-throughput
//! Pareto frontier.

use std::borrow::Borrow;
use std::cell::Cell;
use std::hash::{BuildHasher, Hash};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::{Cache, Slot32, SlotSize, Xxh3Build};

// ---------- thread-id allocator ----------

/// Sentinel for "thread has not yet been assigned an id". `u32::MAX` is
/// reserved so the allocator's hot path is a single TLS load plus a
/// predicted-not-taken branch.
const UNASSIGNED: u32 = u32::MAX;

static NEXT_TLS_ID: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static TLS_ID: Cell<u32> = const { Cell::new(UNASSIGNED) };
}

/// Returns the calling thread's monotonically-allocated TLS id, lazily
/// assigning one on first call. Ids are never recycled (process-global),
/// and `u32::MAX` is reserved as a sentinel — the assertion below would
/// fire long before exhaustion in any realistic process.
#[inline]
fn current_tls_id() -> u32 {
    TLS_ID.with(|cell| {
        let id = cell.get();
        if id != UNASSIGNED {
            id
        } else {
            let new_id = NEXT_TLS_ID.fetch_add(1, Ordering::Relaxed);
            assert!(
                new_id < UNASSIGNED,
                "senba::concurrent: exhausted u32 thread-id space"
            );
            cell.set(new_id);
            new_id
        }
    })
}

// ---------- PartitionedCache ----------

/// `N` independent [`Cache`] instances behind a `Mutex` each, routed by
/// thread-id. The simplest possible parallel baseline: every operation is
/// "compute the partition index from the calling thread's id, take that
/// partition's mutex, run the corresponding [`Cache`] method".
///
/// Routing is **thread-id based, not key-hash based**: the same key
/// observed on different threads lands in different partitions and may be
/// cached independently in each. This loses the global "one entry per key"
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
type Partition<K, V, S, H> = Mutex<Cache<K, V, S, H>>;

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
                Mutex::new(Cache::with_hasher(cap_i, hasher.clone()))
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
        self.partitions
            .iter()
            .map(|p| p.lock().unwrap().capacity())
            .sum()
    }

    /// Number of live entries summed across every partition. Snapshot value:
    /// concurrent inserts on other partitions can change before this
    /// returns. Locks every partition's mutex in turn.
    pub fn len(&self) -> usize {
        self.partitions
            .iter()
            .map(|p| p.lock().unwrap().len())
            .sum()
    }

    /// `true` when every partition is empty. Like [`Self::len`], a snapshot.
    pub fn is_empty(&self) -> bool {
        self.partitions.iter().all(|p| p.lock().unwrap().is_empty())
    }

    /// Picks the partition index for the calling thread. Thread-id based:
    /// every call from the same thread returns the same index for the
    /// lifetime of the process.
    #[inline]
    fn partition_of(&self) -> usize {
        (current_tls_id() as usize) & self.partition_mask
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
        self.partitions[i].lock().unwrap().get(key).cloned()
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
        self.partitions[i].lock().unwrap().peek(key).cloned()
    }

    /// Inserts `(key, value)` into the calling thread's partition. Returns
    /// the previous `(K, V)` if the key was already present in that
    /// partition (replacement), or the SIEVE-chosen victim `(K, V)` if the
    /// partition was full, or `None` if it merely filled empty space.
    ///
    /// Equivalent to [`Cache::insert`] on the chosen partition.
    pub fn insert(&self, key: K, value: V) -> Option<(K, V)> {
        let i = self.partition_of();
        self.partitions[i].lock().unwrap().insert(key, value)
    }

    /// Removes the entry for `key` from the calling thread's partition and
    /// returns its value, or `None` if absent there.
    pub fn remove<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let i = self.partition_of();
        self.partitions[i].lock().unwrap().remove(key)
    }

    /// Returns `true` when `key` is currently in the calling thread's
    /// partition. Non-promoting, like [`Cache::contains_key`].
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let i = self.partition_of();
        self.partitions[i].lock().unwrap().contains_key(key)
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
        let per: Vec<usize> = c
            .partitions
            .iter()
            .map(|p| p.lock().unwrap().capacity())
            .collect();
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
        // A single thread routes every op to exactly one partition (thread-id
        // is fixed for the lifetime of the test). With 4 partitions and cap=16,
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
    /// distinct partitions when the thread-id mask differs. With 16
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

    /// Concurrent invariants smoke: 4 threads pound a partitioned cache
    /// with Zipf-like traffic. We only check that the cache stays within
    /// its capacity contract and that values we get back are not corrupted.
    #[test]
    fn concurrent_invariants_smoke() {
        let cap = 256usize;
        let partitions = 8;
        let cache: Arc<PartitionedCache<u64, u64>> =
            Arc::new(PartitionedCache::new(cap, partitions));

        std::thread::scope(|s| {
            for tid in 0u64..4 {
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

        // len snapshot must respect the global capacity contract.
        let total_len = cache.len();
        assert!(
            total_len <= cap,
            "len snapshot {total_len} > capacity {cap}"
        );
    }
}
