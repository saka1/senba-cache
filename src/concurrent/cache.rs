//! `senba::concurrent::Cache` — sharded, lock-free-reader concurrent SIEVE cache.
//!
//! Promoted from the `c17s` research variant
//! (`research/src/experimental/sieve_c17s.rs`). The reader's hot path is the
//! AVX2 SIMD tag scan + entry-version seqlock — no lock acquire on a hit.
//! Writers take a per-shard `parking_lot::Mutex` for Path B (warmup install)
//! and Path C (evict + shift + install); Path A (in-place value update of
//! an already-present key) is lock-free via the entry version CAS.
//!
//! See `docs/reports/2026-05-13-c17s-shard-heuristic.md` for the
//! `next_pow2(cap/8)` auto-shard choice and the `MIN_PER_SHARD = 4` cliff
//! analysis that motivates the lib defaults.
//!
//! ## Soundness model
//!
//! The entry arena stores `Arc<V>` rather than raw `V`; readers bump the
//! refcount on the bit-copy they pulled out of `entries[id]` and writers
//! defer their old `Arc<V>` drops through `crossbeam-epoch`. The reader's
//! local epoch pin keeps the deferred drops alive until every in-flight
//! `V::clone` finishes (see `shard.rs` module doc for the protocol). This
//! lets the public API require only `V: Clone + Send + Sync + 'static`,
//! matching `moka::sync::Cache<K, V>`.

mod shard;

use crate::Xxh3Build;
use shard::Shard;
use std::borrow::Borrow;
use std::hash::{BuildHasher, Hash};

const MIN_PER_SHARD: usize = 4;

#[inline]
fn auto_shard(capacity: usize) -> usize {
    assert!(capacity > 0, "capacity must be > 0");
    let target = capacity.div_ceil(8).next_power_of_two().max(1);
    let max_shards = capacity.div_ceil(MIN_PER_SHARD).next_power_of_two().max(1);
    let min_shards = capacity
        .div_ceil(shard::MAX_PER_SHARD)
        .next_power_of_two()
        .max(1);
    target.min(max_shards).max(min_shards)
}

/// Lock-free-reader concurrent SIEVE cache.
///
/// `Cache::new(capacity)` picks the number of shards from `capacity`
/// (`next_pow2(cap/8)`, clamped to `MIN_PER_SHARD = 4` and `MAX_PER_SHARD =
/// 64`). Override with [`Cache::with_shards`] when you need a specific
/// shard count (e.g. to match a benchmark sweep).
///
/// All operations take `&self` — the cache is `Sync`. Readers take no lock
/// on a hit; the writer path acquires a per-shard `parking_lot::Mutex`
/// only when the lock-free Path A misses.
///
/// **Experimental.** Available behind the `concurrent` Cargo feature. The
/// implementation is `x86_64 + AVX2` only and is not exposed on other
/// targets (the entire module compiles out).
///
/// ```ignore
/// use senba::concurrent::Cache;
///
/// let cache: Cache<u64, u64> = Cache::new(4096);
/// cache.insert(1, 10);
/// assert_eq!(cache.get(&1), Some(10));
/// ```
pub struct Cache<K, V, H: BuildHasher = Xxh3Build> {
    shards: Box<[Shard<K, V>]>,
    /// `shards.len() - 1`, cached so `shard_of_hash` is a single AND.
    shard_mask: usize,
    hasher: H,
}

impl<K, V> Cache<K, V, Xxh3Build>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Creates a cache with `capacity` total entries split across an
    /// automatically chosen power-of-two number of shards (see module doc).
    #[inline]
    pub fn new(capacity: usize) -> Self {
        Self::with_shards_and_hasher(capacity, auto_shard(capacity), Xxh3Build)
    }

    /// Like [`Self::new`] but with an explicit `shards` count. `shards`
    /// must be a power of two and `>= 1`; `capacity` must be `>= shards`.
    #[inline]
    pub fn with_shards(capacity: usize, shards: usize) -> Self {
        Self::with_shards_and_hasher(capacity, shards, Xxh3Build)
    }
}

impl<K, V, H> Cache<K, V, H>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
    H: BuildHasher,
{
    /// Cache with a user-supplied [`BuildHasher`] and auto-chosen shards.
    #[inline]
    pub fn with_hasher(capacity: usize, hasher: H) -> Self {
        Self::with_shards_and_hasher(capacity, auto_shard(capacity), hasher)
    }

    /// Cache with explicit shard count and a user-supplied [`BuildHasher`].
    pub fn with_shards_and_hasher(capacity: usize, shards: usize, hasher: H) -> Self {
        assert!(shards > 0, "shards must be > 0");
        assert!(
            shards.is_power_of_two(),
            "shards ({shards}) must be a power of two so routing reduces to a single AND"
        );
        assert!(
            capacity >= shards,
            "capacity ({capacity}) must be >= shards ({shards}) so each shard has cap >= 1"
        );
        let base = capacity / shards;
        let extra = capacity % shards;
        let mut built: Vec<Shard<K, V>> = Vec::with_capacity(shards);
        for i in 0..shards {
            let cap_i = base + if i < extra { 1 } else { 0 };
            built.push(Shard::new(cap_i));
        }
        Self {
            shards: built.into_boxed_slice(),
            shard_mask: shards - 1,
            hasher,
        }
    }

    /// Total capacity summed across every shard (fixed at construction).
    #[inline]
    pub fn capacity(&self) -> usize {
        self.shards.iter().map(|s| s.capacity()).sum()
    }

    /// Number of live entries summed across every shard. Snapshot value:
    /// concurrent writes on other shards can change before this returns.
    #[inline]
    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.len()).sum()
    }

    /// `true` when every shard is empty. Snapshot value, like [`Self::len`].
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.len() == 0)
    }

    /// Number of shards. Informational; the value is internal-detail and
    /// the auto-shard heuristic may change in a future minor.
    #[doc(hidden)]
    #[inline]
    pub fn shards(&self) -> usize {
        self.shard_mask + 1
    }

    /// `true` if `key` is present in the cache. Snapshot value; concurrent
    /// writers may add or evict between the call and the return.
    #[inline]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        self.shards[self.shard_of_hash(h)].contains::<Q>(key, h)
    }

    /// Returns a copy of the value for `key`, or `None` if absent. Sets
    /// the SIEVE visited bit on hit.
    #[inline]
    pub fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        self.shards[self.shard_of_hash(h)].get_by_hash::<Q>(key, h)
    }

    /// Inserts `key → value`. Returns the previously-evicted `(key,
    /// value)` if the shard was full and SIEVE chose a victim, or `None`
    /// otherwise (warmup install / Path A update of an existing key).
    #[inline]
    pub fn insert(&self, key: K, value: V) -> Option<(K, V)> {
        let h = self.hasher.hash_one(&key);
        let i = self.shard_of_hash(h);
        self.shards[i].insert(key, value, h)
    }

    /// Removes the entry for `key` if present and returns its value.
    /// Cold path — takes the per-shard writer mutex.
    #[inline]
    pub fn remove<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        self.shards[self.shard_of_hash(h)].remove::<Q>(key, h)
    }

    #[inline]
    fn shard_of_hash(&self, hash: u64) -> usize {
        (hash as usize) & self.shard_mask
    }

    #[cfg(test)]
    pub(crate) fn shard(&self, idx: usize) -> &Shard<K, V> {
        &self.shards[idx]
    }
}

#[cfg(test)]
mod tests {
    use super::Cache;
    use super::shard::Shard;

    #[test]
    fn evicts_oldest_when_full_and_unvisited() {
        let cache: Cache<i32, i32> = Cache::with_shards(2, 1);
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
        let cache: Cache<i32, i32> = Cache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((2, 20)));
    }

    #[test]
    fn all_visited_clears_bits_then_evicts() {
        let cache: Cache<i32, i32> = Cache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        cache.get(&2);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((1, 10)));
    }

    #[test]
    fn warm_up_to_steady_transition() {
        let cache: Cache<u64, u64> = Cache::with_shards(4, 1);
        assert_eq!(cache.insert(1, 100), None);
        assert_eq!(cache.insert(2, 200), None);
        assert_eq!(cache.insert(3, 300), None);
        assert_eq!(cache.insert(4, 400), None);
        assert_eq!(cache.len(), 4);
        let evicted = cache.insert(5, 500);
        assert!(evicted.is_some());
        assert_eq!(cache.len(), 4);
        assert_eq!(cache.get(&5), Some(500));
    }

    #[test]
    fn distinct_keys_full_per_shard_all_hit() {
        let n: u64 = 64;
        let cache: Cache<u64, u64> = Cache::with_shards(n as usize, 1);
        for k in 0..n {
            cache.insert(k, k * 7);
        }
        for k in 0..n {
            assert_eq!(cache.get(&k), Some(k * 7), "miss for key {k}");
        }
    }

    #[test]
    fn update_via_path_a_preserves_id_and_tag() {
        let cache: Cache<u64, u64> = Cache::with_shards(4, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.insert(3, 30);
        cache.insert(4, 40);
        let sh = cache.shard(0);
        let ids_before: Vec<usize> = sh.live_ids();
        let tags_before: Vec<u16> = (0..4).map(|i| sh.tag_at(i)).collect();
        cache.insert(2, 222);
        let ids_after: Vec<usize> = sh.live_ids();
        let tags_after: Vec<u16> = (0..4).map(|i| sh.tag_at(i)).collect();
        assert_eq!(
            ids_before, ids_after,
            "Path A update が id mapping を変えている (= 想定外の Path C 経路)"
        );
        // c17s 固有: tag は完全に不変 (c14s/c16s は VERSION bit が flip していた)
        assert_eq!(
            tags_before, tags_after,
            "Path A update が tag を変更している (c17s は tag 不変が core property)"
        );
        assert_eq!(cache.get(&2), Some(222));
    }

    #[test]
    fn path_a_increments_entry_version_by_two() {
        let cache: Cache<u64, u64> = Cache::with_shards(4, 1);
        cache.insert(1, 10);
        let sh = cache.shard(0);
        let v0 = sh.entry_version(0);
        cache.insert(1, 100);
        let v1 = sh.entry_version(0);
        cache.insert(1, 1000);
        let v2 = sh.entry_version(0);
        assert_eq!(
            v1,
            v0.wrapping_add(2),
            "1st update should bump version by 2"
        );
        assert_eq!(
            v2,
            v0.wrapping_add(4),
            "2nd update should bump version by 4"
        );
        assert_eq!(v1 & 1, 0);
        assert_eq!(v2 & 1, 0);
        assert_eq!(cache.get(&1), Some(1000));
    }

    #[test]
    fn update_existing_key_sets_visited_like_oracle() {
        let cache: Cache<i32, i32> = Cache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.insert(1, 11);
        let evicted = cache.insert(3, 30);
        assert_eq!(
            evicted,
            Some((2, 20)),
            "update が visited を SET しないと (1) が evict されてしまう"
        );
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }

    #[test]
    fn reader_hit_does_not_modify_tag() {
        let cache: Cache<u64, u64> = Cache::with_shards(4, 1);
        cache.insert(1, 100);
        let sh = cache.shard(0);
        let tag_before = sh.tag_at(0);
        assert_eq!(cache.get(&1), Some(100));
        let tag_after = sh.tag_at(0);
        assert_eq!(
            tag_before, tag_after,
            "reader hit が tag を変更している (visited 分離が崩れている)"
        );
        let mask = Shard::<u64, u64>::vbit_mask_pub(0);
        assert!(
            sh.visited_snapshot() & mask != 0,
            "visited bit が立っていない"
        );
    }

    #[test]
    fn evict_reuses_id_at_tail_position_and_bumps_epoch() {
        let cache: Cache<u64, u64> = Cache::with_shards(4, 1);
        for k in 0..4u64 {
            cache.insert(k, k * 10);
        }
        let sh = cache.shard(0);
        let epoch_before = sh.path_c_epoch_snapshot();
        let ids_before: Vec<usize> = sh.live_ids();
        assert_eq!(sh.live_count(), 4);
        assert_eq!(ids_before, vec![0, 1, 2, 3]);
        let evicted = cache.insert(99, 9900);
        assert!(evicted.is_some());
        assert_eq!(sh.live_count(), 4);
        let last_tag = sh.tag_at(3);
        let last_id = Shard::<u64, u64>::id_of_pub(last_tag);
        assert_eq!(last_id, 0, "Path C で id 再利用していない");
        let epoch_after = sh.path_c_epoch_snapshot();
        assert!(
            epoch_after > epoch_before,
            "Path C で path_c_epoch が bump されていない"
        );
    }

    #[test]
    fn path_a_does_not_evict() {
        let cache: Cache<u64, u64> = Cache::with_shards(4, 1);
        for k in 0..4u64 {
            assert_eq!(cache.insert(k, k), None);
        }
        for _ in 0..100 {
            for k in 0..4u64 {
                assert_eq!(
                    cache.insert(k, k * 1000),
                    None,
                    "Path A update が evicted を返した (= Path C に落ちた)"
                );
            }
        }
        for k in 0..4u64 {
            assert_eq!(cache.get(&k), Some(k * 1000));
        }
    }

    #[test]
    fn remove_existing_key_returns_value() {
        let cache: Cache<u64, u64> = Cache::with_shards(4, 1);
        cache.insert(1, 100);
        cache.insert(2, 200);
        cache.insert(3, 300);
        assert_eq!(cache.remove(&2), Some(200));
        assert_eq!(cache.len(), 2);
        assert!(!cache.contains_key(&2));
        assert!(cache.contains_key(&1));
        assert!(cache.contains_key(&3));
    }

    #[test]
    fn remove_absent_key_returns_none() {
        let cache: Cache<u64, u64> = Cache::with_shards(4, 1);
        cache.insert(1, 100);
        assert_eq!(cache.remove(&999), None);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn remove_then_insert_reuses_id() {
        let cache: Cache<u64, u64> = Cache::with_shards(4, 1);
        cache.insert(1, 100);
        cache.insert(2, 200);
        cache.insert(3, 300);
        let sh = cache.shard(0);
        let ids_before = sh.live_ids();
        assert_eq!(ids_before, vec![0, 1, 2]);
        // remove pos=1 (id=1)
        assert_eq!(cache.remove(&2), Some(200));
        // free_ids = [1], next warmup uses id=1
        cache.insert(4, 400);
        let ids_after = sh.live_ids();
        // tags now: [0, 2, 1] (id 0 still at pos 0, id 2 shifted to pos 1, new id 1 at pos 2)
        assert_eq!(ids_after.len(), 3);
        assert!(ids_after.contains(&1), "removed id (1) not reused");
        assert_eq!(cache.get(&4), Some(400));
        assert_eq!(cache.get(&1), Some(100));
        assert_eq!(cache.get(&3), Some(300));
    }

    #[test]
    fn bit_layout_exclusivity_u64_u64() {
        type S = Shard<u64, u64>;
        // Entry<u64,u64> = AtomicU32 + u64 + u64 = 4+8+8 = 20, align(32) → sizeof = 32
        // ⇒ ID_SHIFT = 5
        assert_eq!(S::entry_size_pub(), 32);
        assert_eq!(S::id_shift(), 5);
        assert_eq!(S::id_mask(), 0x07E0);
        assert_eq!(S::hash_mask(), 0x781F);
        assert_eq!(S::hash_mask().count_ones(), 9);
        assert_eq!(S::scan_mask(), S::live_bit_pub() | S::hash_mask());

        // LIVE | ID | HASH の 3 区画で 0xFFFF を埋め切る (c17s は VERSION 不在)。
        assert_eq!(S::live_bit_pub() | S::id_mask() | S::hash_mask(), 0xFFFF);
        assert_eq!(S::live_bit_pub() & S::id_mask(), 0);
        assert_eq!(S::live_bit_pub() & S::hash_mask(), 0);
        assert_eq!(S::id_mask() & S::hash_mask(), 0);
    }

    #[test]
    fn shard_hot_layout_contract() {
        assert_eq!(Shard::<u64, u64>::shard_hot_size_pub(), 64);
        assert_eq!(Shard::<u64, u64>::shard_hot_align_pub(), 64);
    }

    #[test]
    fn auto_shard_examples() {
        use super::auto_shard;
        // cap=64 → target = 64/8 = 8 shards, min = 64/64 = 1, max = 64/4 = 16. → 8
        assert_eq!(auto_shard(64), 8);
        // cap=4096 → target = 4096/8 = 512 shards, min = 4096/64 = 64, max = 4096/4 = 1024. → 512
        assert_eq!(auto_shard(4096), 512);
        // cap=4 → target = 1, min = 1, max = 1 → 1
        assert_eq!(auto_shard(4), 1);
        // cap=8 → target = 1, min = 1, max = 2 → 1
        assert_eq!(auto_shard(8), 1);
        // cap=16 → target = 2, min = 1, max = 4 → 2
        assert_eq!(auto_shard(16), 2);
    }

    #[test]
    fn v_string_chaos_under_contention() {
        // Stresses the Arc<V> + epoch reclamation path with a heap-owning V.
        // Under the previous (V: Copy) implementation this test would race
        // a writer's drop against a reader's clone and segfault on heap V;
        // with `Arc<V>` + `crossbeam-epoch` it must run clean.
        use std::sync::Arc;
        use std::thread;

        let cap = 64usize;
        let cache: Arc<Cache<u64, String>> = Arc::new(Cache::new(cap));

        thread::scope(|s| {
            // Writers: hammer a small key universe with churning string values.
            for tid in 0..4u64 {
                let c = Arc::clone(&cache);
                s.spawn(move || {
                    for i in 0..10_000u64 {
                        let k = (i ^ tid).wrapping_mul(0x9E37_79B9_7F4A_7C15) % 256;
                        let v = format!("t{tid}-i{i}-k{k}");
                        c.insert(k, v);
                    }
                });
            }
            // Readers: keep cloning values out and assert they're well-formed.
            for tid in 0..4u64 {
                let c = Arc::clone(&cache);
                s.spawn(move || {
                    for i in 0..10_000u64 {
                        let k = (i ^ tid).wrapping_mul(0xBF58_476D_1CE4_E5B9) % 256;
                        if let Some(v) = c.get(&k) {
                            // The whole point: we received an owned `String`
                            // whose heap allocation didn't get freed mid-clone.
                            assert!(v.starts_with('t'), "torn string: {:?}", v);
                            // Force inspection of the heap bytes; if the
                            // allocation were freed under us, ASan / heap
                            // checker would fire here.
                            let _ = v.len();
                            let _: u32 = v.bytes().map(u32::from).sum();
                        }
                    }
                });
            }
        });

        // Final structural invariants.
        let total_len = cache.len();
        assert!(total_len <= cap);
        let nshards = cache.shards();
        let mut sum_live = 0;
        for i in 0..nshards {
            let sh = cache.shard(i);
            let live = sh.live_count();
            assert_eq!(live, sh.len());
            sum_live += live;
        }
        assert_eq!(sum_live, total_len);

        // Spot-check we can still get/remove without UAF.
        let _ = cache.get(&7);
        let _ = cache.remove(&7);
    }

    #[test]
    fn multi_thread_zipf_like_chaos() {
        use std::sync::Arc;
        use std::thread;

        let cap = 256usize;
        let cache: Arc<Cache<u64, u64>> = Arc::new(Cache::new(cap));

        thread::scope(|s| {
            for tid in 0..4u64 {
                let c = Arc::clone(&cache);
                s.spawn(move || {
                    // LCG-based pseudo-Zipf: skewed by squaring a uniform draw.
                    let mut state = 0x9E37_79B9_7F4A_7C15u64.wrapping_add(tid);
                    for _ in 0..50_000 {
                        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                        let u = state >> 32;
                        let k = ((u * u) >> 32) % 1024;
                        if c.get(&k).is_none() {
                            c.insert(k, k);
                        }
                    }
                });
            }
        });

        let total_len = cache.len();
        assert!(total_len <= cap, "len {total_len} > cap {cap}");

        // Per-shard structural invariants.
        let nshards = cache.shards();
        let mut sum_live = 0;
        for i in 0..nshards {
            let sh = cache.shard(i);
            let live = sh.live_count();
            let ids = sh.live_ids();
            assert_eq!(live, ids.len());
            assert_eq!(live, sh.len());
            let mut sorted = ids.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), ids.len(), "shard {i} で id 重複");
            sum_live += live;
        }
        assert_eq!(sum_live, total_len);

        for k in 0..1024u64 {
            if let Some(v) = cache.get(&k) {
                assert_eq!(v, k, "key {k} の value が破壊されている");
            }
        }
    }
}
