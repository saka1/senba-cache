use super::super::*;
use super::TEST_SHARDS;

#[test]
fn peek_returns_value_without_promoting() {
    // Capacity 2, single shard so SIEVE behavior is fully deterministic.
    // Insert 1, 2 (both unvisited, oldest = 1). peek(&1) must NOT mark 1 visited;
    // inserting 3 then evicts 1 (the unvisited tail). If peek had promoted, 2
    // would have evicted instead.
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    assert_eq!(cache.peek(&1), Some(&10));
    let evicted = cache.insert(3, 30);
    assert_eq!(evicted, Some((1, 10)));
    assert!(cache.contains_key(&2));
    assert!(cache.contains_key(&3));
}

#[test]
fn peek_missing_returns_none() {
    let cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    assert_eq!(cache.peek(&42), None);
}

#[test]
fn peek_versus_get_eviction_difference() {
    // Same setup, but use `get` instead of `peek` — get promotes 1, so the
    // SIEVE victim becomes 2. Confirms the symmetric eviction outcome and
    // pins down that peek's "no promotion" is observable.
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    assert_eq!(cache.get(&1), Some(&10));
    let evicted = cache.insert(3, 30);
    assert_eq!(evicted, Some((2, 20)));
    assert!(cache.contains_key(&1));
    assert!(cache.contains_key(&3));
}

#[test]
fn peek_mut_updates_in_place() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    cache.insert(1, 10);
    *cache.peek_mut(&1).unwrap() += 5;
    assert_eq!(cache.peek(&1), Some(&15));
    assert!(cache.peek_mut(&999).is_none());
}

#[test]
fn peek_mut_does_not_promote() {
    // Mirrors peek_versus_get_eviction_difference: capacity 2, 1 shard.
    // peek_mut on key 1 must NOT set VISITED, so 1 is the SIEVE victim
    // when 3 is inserted. (`get_mut` would promote and evict 2 instead.)
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    *cache.peek_mut(&1).unwrap() += 100;
    let evicted = cache.insert(3, 30);
    assert_eq!(evicted, Some((1, 110)));
    assert!(cache.contains_key(&2));
    assert!(cache.contains_key(&3));
}

#[test]
fn peek_mut_via_borrow_string_to_str() {
    let mut cache: Cache<String, u64> = Cache::new(TEST_SHARDS * 4);
    cache.insert("alpha".to_string(), 1);
    *cache.peek_mut("alpha").unwrap() = 42;
    assert_eq!(cache.peek("alpha"), Some(&42));
}

#[test]
fn peek_key_value_returns_stored_key_and_value() {
    let mut cache: Cache<String, u64> = Cache::new(TEST_SHARDS * 4);
    cache.insert("beta".to_string(), 7);

    let (k, v) = cache.peek_key_value("beta").unwrap();
    assert_eq!(k, "beta");
    assert_eq!(*v, 7);
    assert!(cache.peek_key_value("missing").is_none());
}

#[test]
fn peek_key_value_does_not_promote() {
    // Symmetric to peek_returns_value_without_promoting: peek_key_value
    // on key 1 leaves 1 as the SIEVE victim.
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    let (_, v) = cache.peek_key_value(&1).unwrap();
    assert_eq!(*v, 10);
    let evicted = cache.insert(3, 30);
    assert_eq!(evicted, Some((1, 10)));
    assert!(cache.contains_key(&2));
}
