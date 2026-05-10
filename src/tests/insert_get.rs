use super::super::*;
use super::TEST_SHARDS;

#[test]
fn cache_initially_empty() {
    let cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    assert_eq!(cache.len(), 0);
    assert_eq!(cache.capacity(), TEST_SHARDS * 4);
    assert!(cache.is_empty());
}

#[test]
fn insert_then_get() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    assert!(cache.insert(1, 10).is_none());
    assert_eq!(cache.get(&1), Some(&10));
    assert_eq!(cache.len(), 1);
}

#[test]
fn get_missing_returns_none() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    cache.insert(1, 10);
    assert_eq!(cache.get(&2), None);
}

#[test]
fn contains_key_reflects_insertions() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    assert!(!cache.contains_key(&1));
    cache.insert(1, 10);
    assert!(cache.contains_key(&1));
    assert!(!cache.contains_key(&2));
}

#[test]
fn insert_existing_key_updates_value() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    cache.insert(1, 10);
    assert!(cache.insert(1, 20).is_none());
    assert_eq!(cache.get(&1), Some(&20));
    assert_eq!(cache.len(), 1);
}

#[test]
fn get_mut_updates_in_place_and_sets_visited() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    cache.insert(1, 10);
    *cache.get_mut(&1).unwrap() += 5;
    assert_eq!(cache.get(&1), Some(&15));
    assert!(cache.get_mut(&999).is_none());
}

#[test]
fn get_key_value_returns_stored_key_and_value() {
    let mut cache: Cache<String, u64> = Cache::new(TEST_SHARDS * 4);
    cache.insert("alpha".to_string(), 1);

    let (k, v) = cache.get_key_value("alpha").unwrap();
    assert_eq!(k, "alpha");
    assert_eq!(*v, 1);
    assert!(cache.get_key_value("missing").is_none());
}

#[test]
fn get_key_value_promotes_on_hit() {
    // Same setup as peek_versus_get_eviction_difference: get_key_value
    // promotes 1, so the SIEVE victim is 2.
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    let (_, v) = cache.get_key_value(&1).unwrap();
    assert_eq!(*v, 10);
    let evicted = cache.insert(3, 30);
    assert_eq!(evicted, Some((2, 20)));
    assert!(cache.contains_key(&1));
}
