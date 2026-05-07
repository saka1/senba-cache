use super::super::*;

#[test]
fn stats_initial_zero() {
    let cache: Cache<u64, u64, Slot32> = Cache::new(4);
    let s = cache.stats();
    assert_eq!(s.hits, 0);
    assert_eq!(s.misses, 0);
    assert_eq!(s.insertions, 0);
    assert_eq!(s.evictions, 0);
}

#[test]
fn stats_count_get_hit_and_miss() {
    let mut cache: Cache<u64, u64, Slot32> = Cache::new(4);
    cache.insert(1, 10);
    assert_eq!(cache.stats().insertions, 1);

    assert_eq!(cache.get(&1), Some(&10));
    assert_eq!(cache.get(&2), None);
    assert_eq!(cache.get(&1), Some(&10));
    let s = cache.stats();
    assert_eq!(s.hits, 2);
    assert_eq!(s.misses, 1);
    assert_eq!(s.insertions, 1);
    assert_eq!(s.evictions, 0);
}

#[test]
fn stats_get_mut_and_get_key_value_count() {
    let mut cache: Cache<u64, u64, Slot32> = Cache::new(4);
    cache.insert(1, 10);
    assert!(cache.get_mut(&1).is_some());
    assert!(cache.get_mut(&2).is_none());
    assert!(cache.get_key_value(&1).is_some());
    assert!(cache.get_key_value(&2).is_none());
    let s = cache.stats();
    assert_eq!(s.hits, 2);
    assert_eq!(s.misses, 2);
}

#[test]
fn stats_peek_and_contains_do_not_count() {
    let mut cache: Cache<u64, u64, Slot32> = Cache::new(4);
    cache.insert(1, 10);
    let _ = cache.peek(&1);
    let _ = cache.peek(&2);
    let _ = cache.peek_mut(&1);
    let _ = cache.peek_key_value(&1);
    let _ = cache.contains_key(&1);
    let _ = cache.contains_key(&2);
    let s = cache.stats();
    assert_eq!(s.hits, 0);
    assert_eq!(s.misses, 0);
}

#[test]
fn stats_insert_replace_counts_insertion_not_eviction() {
    let mut cache: Cache<u64, u64, Slot32> = Cache::new(4);
    cache.insert(1, 10);
    cache.insert(1, 20); // replace
    let s = cache.stats();
    assert_eq!(s.insertions, 2);
    assert_eq!(s.evictions, 0);
}

#[test]
fn stats_capacity_eviction_counts() {
    // 1 shard with capacity 2 so a 3rd insert is guaranteed to evict.
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    let evicted = cache.insert(3, 30);
    assert!(evicted.is_some());
    let s = cache.stats();
    assert_eq!(s.insertions, 3);
    assert_eq!(s.evictions, 1);
}

#[test]
fn stats_remove_does_not_count_eviction() {
    let mut cache: Cache<u64, u64, Slot32> = Cache::new(4);
    cache.insert(1, 10);
    assert_eq!(cache.remove(&1), Some(10));
    let s = cache.stats();
    assert_eq!(s.insertions, 1);
    assert_eq!(s.evictions, 0);
}

#[test]
fn stats_clear_and_retain_do_not_count_eviction() {
    let mut cache: Cache<u64, u64, Slot32> = Cache::new(4);
    cache.insert(1, 10);
    cache.insert(2, 20);
    cache.retain(|_, v| *v == 10);
    cache.clear();
    let s = cache.stats();
    assert_eq!(s.evictions, 0);
}

#[test]
fn stats_get_or_insert_with_hit_miss_split() {
    let mut cache: Cache<u64, u64, Slot32> = Cache::new(4);
    let v = *cache.get_or_insert_with(1, || 100);
    assert_eq!(v, 100);
    let v = *cache.get_or_insert_with(1, || 999);
    assert_eq!(v, 100); // hit, closure not evaluated
    let s = cache.stats();
    assert_eq!(s.misses, 1);
    assert_eq!(s.hits, 1);
    assert_eq!(s.insertions, 1);
    assert_eq!(s.evictions, 0);
}

#[test]
fn stats_get_or_insert_with_miss_can_evict() {
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    let _ = cache.get_or_insert_with(3, || 30);
    let s = cache.stats();
    assert_eq!(s.misses, 1);
    assert_eq!(s.insertions, 3);
    assert_eq!(s.evictions, 1);
}

#[test]
fn stats_aggregated_across_shards() {
    // Force >1 shard (capacity / MAX_PER_SHARD > 1 ⟹ 2+ shards).
    let mut cache: Cache<u64, u64, Slot32> = Cache::new(MAX_PER_SHARD * 2);
    assert!(cache.shards() >= 2);
    for k in 0u64..32 {
        cache.insert(k, k);
    }
    for k in 0u64..32 {
        assert!(cache.get(&k).is_some());
    }
    for k in 1000u64..1010 {
        assert!(cache.get(&k).is_none());
    }
    let s = cache.stats();
    assert_eq!(s.hits, 32);
    assert_eq!(s.misses, 10);
    assert_eq!(s.insertions, 32);
}

#[test]
fn stats_clone_copies_counters() {
    let mut cache: Cache<u64, u64, Slot32> = Cache::new(4);
    cache.insert(1, 10);
    let _ = cache.get(&1);
    let _ = cache.get(&2);
    let cloned = cache.clone();
    assert_eq!(cache.stats(), cloned.stats());
}
