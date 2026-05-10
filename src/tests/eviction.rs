use super::super::*;
use super::TEST_SHARDS;

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
