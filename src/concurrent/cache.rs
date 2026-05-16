//! `senba::concurrent::Cache` — sharded, lock-free-reader concurrent SIEVE cache.
//!
//! Built on the `sieve_r4` research engine
//! (`research/src/experimental/sieve_r4.rs`). The reader's hot path is the
//! AVX2 SIMD tag scan + entry-version seqlock — no lock acquire and no
//! shared atomic write on a hit. Writers take a per-shard
//! `parking_lot::Mutex` for Path B (warmup install) and Path C (evict +
//! shift + install); Path A (in-place value update of an already-present
//! key) is lock-free via the entry version CAS.
//!
//! See `docs/reports/2026-05-13-c17s-shard-heuristic.md` for the
//! `next_pow2(cap/8)` auto-shard choice and the `MIN_PER_SHARD = 4` cliff
//! analysis that motivates the lib defaults.
//!
//! ## Soundness model
//!
//! Each entry stores `key: ManuallyDrop<K>` and `value: ManuallyDrop<V>`.
//! Readers bit-copy the live slot into a `ManuallyDrop<Entry<K,V>>` local,
//! validate the seqlock (`v1 == v2`, even), then clone V directly off the
//! deref'd `ManuallyDrop<V>`. Writers (Path A, Path C, `remove`) extract
//! the old K / V via raw `ptr::read` or `ManuallyDrop::take` and defer
//! their drop past in-flight reader pins via `crossbeam-epoch`. The pin
//! held by a reader during `V::clone` (and `K::eq`) keeps the deferred
//! allocations live, so the bit-copy local always references valid memory
//! until the clone finishes. See `shard.rs` for the per-method protocol.
//!
//! Reader hit cost is `~3–5 ns` of `epoch::pin` overhead for `V: !Copy`
//! and zero for `V: Copy` (the pin folds away via `needs_drop::<V>()`).
//! See `docs/reports/2026-05-15-r4-vs-c17s.md` for the full sweep against
//! the prior `Arc<V>`-based implementation.

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
/// reader's tag scan uses AVX2+BMI1 when the host CPU advertises them and
/// a portable scalar fallback otherwise; non-x86_64 targets always take
/// the scalar path. Setting `SENBA_FORCE_SCALAR=1` in the environment
/// forces the scalar path even on AVX2-capable x86_64 — used by CI to
/// exercise the fallback on AVX2 runners without a separate codegen build.
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
    /// SIMD reader-path availability, resolved once in
    /// `with_shards_and_hasher` so the dispatch in each shard call is a
    /// single boolean load instead of a re-entry into
    /// `is_x86_feature_detected!` on every op. On x86_64 this is set
    /// from `is_x86_feature_detected!("avx2")` — BMI1 is implied by AVX2
    /// on every x86_64 CPU shipped to date, so detecting AVX2 suffices.
    /// On other architectures it is always `false` (the dispatchers in
    /// `shard.rs` reserve explicit `#[cfg(target_arch = "aarch64")]`
    /// arms for a future NEON twin, but no SIMD twin is wired up there
    /// yet). `SENBA_FORCE_SCALAR=1` in the environment forces this to
    /// `false` regardless of arch (CI hook for scalar coverage).
    has_avx2_bmi1: bool,
}

impl<K, V> Cache<K, V, Xxh3Build>
where
    K: Hash + Eq + Send + 'static,
    V: Clone + Send + 'static,
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
    K: Hash + Eq + Send + 'static,
    V: Clone + Send + 'static,
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
        let has_avx2_bmi1 = {
            #[cfg(target_arch = "x86_64")]
            {
                std::is_x86_feature_detected!("avx2")
                    && std::env::var_os("SENBA_FORCE_SCALAR").is_none()
            }
            // Reserved aarch64 arm: NEON is mandatory on aarch64, so once
            // the NEON twins (`find_get_neon` / `find_lockfree_for_path_a_neon`)
            // land in `shard.rs`, the body here becomes
            // `std::env::var_os("SENBA_FORCE_SCALAR").is_none()`. The
            // dispatchers already carry the symmetric insertion point.
            #[cfg(target_arch = "aarch64")]
            {
                false
            }
            #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
            {
                false
            }
        };
        Self {
            shards: built.into_boxed_slice(),
            shard_mask: shards - 1,
            hasher,
            has_avx2_bmi1,
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
        self.shards[self.shard_of_hash(h)].contains::<Q>(key, h, self.has_avx2_bmi1)
    }

    /// Returns a copy of the value for `key`, or `None` if absent. Sets
    /// the SIEVE visited bit on hit.
    ///
    /// Under sustained writer contention on the same shard, `get` may
    /// return `None` after a bounded number of seqlock retries even when
    /// the key is present — concurrent in-place updates or evictions on
    /// the candidate slot keep invalidating the reader's snapshot. This
    /// is extremely unlikely in practice (it requires the same lane to
    /// race across every retry), and a follow-up call will succeed once
    /// the writer storm subsides.
    #[inline]
    pub fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        self.shards[self.shard_of_hash(h)].get_by_hash::<Q>(key, h, self.has_avx2_bmi1)
    }

    /// Like [`Self::get`] but **does not** set the SIEVE visited bit.
    /// Returns a clone of the value on hit, or `None` on miss.
    ///
    /// Use this for non-promoting lookups (metrics probes, expiry checks,
    /// debugging) where observing an entry should not affect its eviction
    /// resistance. Unlike `get`, `peek` is not counted in [`Self::stats`]
    /// hits/misses — it is a pure probe.
    #[inline]
    pub fn peek<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        self.shards[self.shard_of_hash(h)].peek_by_hash::<Q>(key, h, self.has_avx2_bmi1)
    }

    /// Inserts `key → value`. If the shard was full and SIEVE chose a
    /// victim, the evicted `(K, V)` is dropped past every in-flight reader
    /// pin via `crossbeam-epoch` — use [`Self::insert_with`] if you need
    /// to observe the evicted pair instead.
    #[inline]
    pub fn insert(&self, key: K, value: V) {
        self.insert_with(key, value, |_, _| {});
    }

    /// Like [`Self::insert`], but invokes `on_evict(evicted_key,
    /// evicted_value)` on a Path C eviction. The callback runs inside the
    /// shard's writer-mutex critical section (deferred past reader pins
    /// when K or V carries a destructor; synchronous otherwise), so it
    /// should not block — store the pair into a channel / vec and act on
    /// it elsewhere. The callback is not invoked on warmup install (Path
    /// B) or in-place key update (Path A).
    #[inline]
    pub fn insert_with<F>(&self, key: K, value: V, on_evict: F)
    where
        F: FnOnce(K, V) + Send + 'static,
    {
        let h = self.hasher.hash_one(&key);
        let i = self.shard_of_hash(h);
        self.shards[i].insert(key, value, h, self.has_avx2_bmi1, on_evict);
    }

    /// Drops every entry and resets the cache to empty. Capacity, shard
    /// count, hasher, and the AVX2 dispatch flag are preserved.
    ///
    /// Acquires each shard's writer mutex in turn. Concurrent readers
    /// that are mid-`get` either complete their cloned return (the
    /// moved-out K / V are kept alive past the reader's pin) or retry and
    /// observe an empty ring. `clear` is not counted in
    /// [`Self::stats`] evictions — it is an explicit bulk drop.
    pub fn clear(&self) {
        for s in self.shards.iter() {
            s.clear();
        }
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
        cache.insert(3, 30);
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
        cache.insert(3, 30);
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }

    #[test]
    fn all_visited_clears_bits_then_evicts() {
        let cache: Cache<i32, i32> = Cache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        cache.get(&2);
        cache.insert(3, 30);
        assert!(!cache.contains_key(&1));
        assert!(cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }

    #[test]
    fn peek_finds_existing_without_promoting() {
        let cache: Cache<i32, i32> = Cache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        // peek does not set VISITED: the oldest unvisited (1) is still the
        // SIEVE victim on the next Path C, just like if no probe happened.
        assert_eq!(cache.peek(&1), Some(10));
        cache.insert(3, 30);
        assert!(!cache.contains_key(&1));
        assert!(cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }

    #[test]
    fn peek_returns_none_for_missing() {
        let cache: Cache<i32, i32> = Cache::with_shards(2, 1);
        cache.insert(1, 10);
        assert_eq!(cache.peek(&99), None);
    }

    #[test]
    fn peek_via_borrow_q() {
        let cache: Cache<String, u64> = Cache::with_shards(4, 1);
        cache.insert("alpha".to_string(), 1);
        cache.insert("beta".to_string(), 2);
        assert_eq!(cache.peek("alpha"), Some(1));
        assert_eq!(cache.peek("beta"), Some(2));
        assert_eq!(cache.peek("missing"), None);
    }

    #[test]
    fn clear_empties_cache() {
        let cache: Cache<i32, i32> = Cache::with_shards(8, 2);
        for i in 0..6 {
            cache.insert(i, i * 10);
        }
        assert_eq!(cache.len(), 6);
        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
        for i in 0..6 {
            assert_eq!(cache.get(&i), None, "key {i} still resolves after clear");
        }
    }

    #[test]
    fn insert_after_clear_works() {
        let cache: Cache<i32, i32> = Cache::with_shards(4, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.clear();
        cache.insert(3, 30);
        cache.insert(4, 40);
        assert_eq!(cache.get(&3), Some(30));
        assert_eq!(cache.get(&4), Some(40));
        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn clear_runs_destructors() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Clone)]
        struct DropCounter {
            counter: Arc<AtomicUsize>,
        }
        impl Drop for DropCounter {
            fn drop(&mut self) {
                self.counter.fetch_add(1, Ordering::Relaxed);
            }
        }

        let drops = Arc::new(AtomicUsize::new(0));
        let cache: Cache<i32, DropCounter> = Cache::with_shards(4, 1);
        for k in 0..3 {
            cache.insert(
                k,
                DropCounter {
                    counter: Arc::clone(&drops),
                },
            );
        }
        cache.clear();
        // Force the epoch GC to flush any deferred drops.
        for _ in 0..32 {
            crossbeam_epoch::pin().flush();
        }
        drop(cache);
        assert!(
            drops.load(Ordering::Relaxed) >= 1,
            "clear() never ran any destructor"
        );
    }

    #[test]
    fn get_promotes_but_peek_does_not() {
        // Parallel to `visited_entry_survives_first_pass` but with peek
        // substituted for get — the entry should NOT survive because peek
        // never set the visited bit.
        let cache: Cache<i32, i32> = Cache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.peek(&1);
        cache.insert(3, 30);
        // 1 was the older unvisited → evicted (mirroring the no-probe case).
        assert!(!cache.contains_key(&1));
        assert!(cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }

    #[test]
    fn warm_up_to_steady_transition() {
        let cache: Cache<u64, u64> = Cache::with_shards(4, 1);
        cache.insert(1, 100);
        cache.insert(2, 200);
        cache.insert(3, 300);
        cache.insert(4, 400);
        assert_eq!(cache.len(), 4);
        cache.insert(5, 500);
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
            "Path A update changed the id mapping (unexpected Path C fallthrough)"
        );
        // Path A invariant: tag is fully immutable on an in-place key
        // update; only the entry version flips. Earlier research variants
        // (c14s/c16s) flipped a VERSION bit in the tag — this design does
        // not, and the test guards against accidental regression to that
        // shape.
        assert_eq!(
            tags_before, tags_after,
            "Path A update changed the tag (must stay tag-immutable as a core property)"
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
        cache.insert(3, 30);
        // update of key (1) must set visited; otherwise SIEVE would evict
        // (1) instead of (2) on the next install.
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
            "reader hit changed the tag (visited bit must live outside the tag array)"
        );
        let mask = Shard::<u64, u64>::vbit_mask_pub(0);
        assert!(
            sh.visited_snapshot() & mask != 0,
            "reader hit did not set the visited bit"
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
        cache.insert(99, 9900);
        assert_eq!(sh.live_count(), 4);
        let last_tag = sh.tag_at(3);
        let last_id = Shard::<u64, u64>::id_of_pub(last_tag);
        assert_eq!(last_id, 0, "Path C did not reuse the evicted id");
        let epoch_after = sh.path_c_epoch_snapshot();
        assert!(
            epoch_after > epoch_before,
            "Path C did not bump path_c_epoch"
        );
    }

    #[test]
    fn path_a_does_not_evict() {
        let cache: Cache<u64, u64> = Cache::with_shards(4, 1);
        for k in 0..4u64 {
            cache.insert(k, k);
        }
        for _ in 0..100 {
            for k in 0..4u64 {
                // `insert_with` fires the closure only on a Path C eviction.
                // Path A (in-place update of an already-present key) must
                // not invoke it — a panic here would surface a fall-through
                // bug.
                cache.insert_with(k, k * 1000, |_, _| {
                    panic!("Path A update fell through to Path C eviction");
                });
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

        // LIVE | ID | HASH cover all 16 bits with no overlap — no
        // VERSION bit in the tag (the entry seqlock subsumes its role).
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
            assert_eq!(sorted.len(), ids.len(), "shard {i} has duplicate ids");
            sum_live += live;
        }
        assert_eq!(sum_live, total_len);

        for k in 0..1024u64 {
            if let Some(v) = cache.get(&k) {
                assert_eq!(v, k, "value for key {k} is corrupted");
            }
        }
    }

    #[test]
    fn insert_with_callback_fires_on_eviction() {
        use std::sync::{Arc, Mutex};
        let evicted: Arc<Mutex<Vec<(i32, i32)>>> = Arc::new(Mutex::new(Vec::new()));
        let cache: Cache<i32, i32> = Cache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        // Capacity reached; next install evicts the unvisited oldest (key 1).
        {
            let captured = Arc::clone(&evicted);
            cache.insert_with(3, 30, move |k, v| {
                captured.lock().unwrap().push((k, v));
            });
        }
        let captured = evicted.lock().unwrap();
        assert_eq!(captured.as_slice(), &[(1, 10)]);
    }

    #[test]
    fn insert_with_callback_not_fired_on_warmup() {
        use std::sync::{Arc, Mutex};
        let evicted: Arc<Mutex<Vec<(i32, i32)>>> = Arc::new(Mutex::new(Vec::new()));
        let cache: Cache<i32, i32> = Cache::with_shards(4, 1);
        for k in 0..4i32 {
            let captured = Arc::clone(&evicted);
            cache.insert_with(k, k * 10, move |k, v| {
                captured.lock().unwrap().push((k, v));
            });
        }
        assert!(
            evicted.lock().unwrap().is_empty(),
            "Path B (warmup install) must not invoke the on_evict callback"
        );
    }

    #[test]
    fn insert_with_drops_evictee_when_dropped() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Clone)]
        struct DropCounter {
            counter: Arc<AtomicUsize>,
        }
        impl Drop for DropCounter {
            fn drop(&mut self) {
                self.counter.fetch_add(1, Ordering::Relaxed);
            }
        }

        let drops = Arc::new(AtomicUsize::new(0));
        let cache: Cache<i32, DropCounter> = Cache::with_shards(2, 1);
        cache.insert(
            1,
            DropCounter {
                counter: Arc::clone(&drops),
            },
        );
        cache.insert(
            2,
            DropCounter {
                counter: Arc::clone(&drops),
            },
        );
        // Eviction: closure does nothing — the evicted value is dropped at
        // closure scope end (deferred past reader pins).
        cache.insert_with(
            3,
            DropCounter {
                counter: Arc::clone(&drops),
            },
            |_k, _v| {},
        );
        // Force the epoch GC to run any pending defers.
        drop(cache);
        for _ in 0..32 {
            crossbeam_epoch::pin().flush();
        }
        // 3 inserts + 1 eviction-drop: at minimum 1 destructor must have fired.
        assert!(
            drops.load(Ordering::Relaxed) >= 1,
            "evicted DropCounter never ran its destructor"
        );
    }

    #[test]
    fn insert_with_callback_panic_does_not_poison_shard() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        let cache: Cache<i32, i32> = Cache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        // First eviction: callback panics. parking_lot::Mutex doesn't
        // poison on panic, so the shard must remain usable afterwards.
        let r = catch_unwind(AssertUnwindSafe(|| {
            cache.insert_with(3, 30, |_, _| panic!("boom"));
        }));
        assert!(r.is_err(), "panicking callback did not propagate");

        // Subsequent operations succeed against the same shard.
        cache.insert(4, 40);
        let mut alive: Vec<i32> = (0..5).filter(|k| cache.contains_key(k)).collect();
        alive.sort();
        assert!(
            alive.contains(&4),
            "shard is locked up after callback panic"
        );
        assert!(cache.len() <= 2);
    }

    #[test]
    fn v_string_insert_with_under_contention() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::thread;

        let cap = 64usize;
        let cache: Arc<Cache<u64, String>> = Arc::new(Cache::new(cap));
        let eviction_calls = Arc::new(AtomicUsize::new(0));

        thread::scope(|s| {
            for tid in 0..4u64 {
                let c = Arc::clone(&cache);
                let evs = Arc::clone(&eviction_calls);
                s.spawn(move || {
                    for i in 0..5_000u64 {
                        let k = (i ^ tid).wrapping_mul(0x9E37_79B9_7F4A_7C15) % 256;
                        let v = format!("t{tid}-i{i}-k{k}");
                        let evs_inner = Arc::clone(&evs);
                        c.insert_with(k, v, move |_k, evicted| {
                            // Touch the heap bytes — under a clone-mid-flight
                            // UAF this would crash / ASan would fire.
                            let _: u32 = evicted.bytes().map(u32::from).sum();
                            evs_inner.fetch_add(1, Ordering::Relaxed);
                        });
                    }
                });
            }
            for tid in 0..4u64 {
                let c = Arc::clone(&cache);
                s.spawn(move || {
                    for i in 0..5_000u64 {
                        let k = (i ^ tid).wrapping_mul(0xBF58_476D_1CE4_E5B9) % 256;
                        if let Some(v) = c.get(&k) {
                            assert!(v.starts_with('t'), "torn string: {:?}", v);
                            let _: u32 = v.bytes().map(u32::from).sum();
                        }
                    }
                });
            }
        });

        assert!(cache.len() <= cap);
        // We can't assert an exact eviction count, but with key-universe 256
        // > cap 64 and 20k inserts the callback must have fired many times.
        assert!(
            eviction_calls.load(Ordering::Relaxed) > 100,
            "expected the insert_with callback to fire frequently"
        );
    }
}
