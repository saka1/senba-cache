use super::super::*;
use super::TEST_SHARDS;

#[test]
fn clear_empties_cache() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..16u64 {
        cache.insert(k, k * 10);
    }
    assert!(!cache.is_empty());
    cache.clear();
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
    for k in 0..16u64 {
        assert_eq!(cache.get(&k), None);
    }
}

#[test]
fn clear_preserves_capacity_and_allows_reuse() {
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    cache.clear();
    assert_eq!(cache.capacity(), 2);
    // Refilling after clear must work like a fresh cache: warm-up path,
    // no spurious eviction until full.
    assert!(cache.insert(3, 30).is_none());
    assert!(cache.insert(4, 40).is_none());
    assert_eq!(cache.get(&3), Some(&30));
    assert_eq!(cache.get(&4), Some(&40));
    // Now full — next insert evicts.
    let evicted = cache.insert(5, 50);
    assert!(evicted.is_some());
}

#[test]
fn clear_drops_string_values() {
    // Exercises the drop path through clear (no double-drop, no leak under miri/asan).
    let mut cache: Cache<u64, String> = Cache::new(TEST_SHARDS * 2);
    for k in 0..32u64 {
        cache.insert(k, format!("value-{k}"));
    }
    cache.clear();
    assert_eq!(cache.len(), 0);
}

#[test]
fn clear_on_empty_cache_is_noop() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    cache.clear();
    assert!(cache.is_empty());
    cache.insert(1, 10);
    assert_eq!(cache.get(&1), Some(&10));
}

#[test]
fn drop_runs_for_live_entries_only() {
    // String values exercise drop correctness (no double-drop, no leak).
    let mut cache: Cache<u64, String> = Cache::new(TEST_SHARDS * 2);
    for k in 0..64u64 {
        cache.insert(k, format!("value-{k}"));
    }
    assert_eq!(cache.len(), TEST_SHARDS * 2);
    // remove also exercises the drop path.
    for k in 0..16u64 {
        let _ = cache.remove(&k);
    }
    // Remaining entries are dropped when Cache goes out of scope.
}
