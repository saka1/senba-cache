use super::super::*;
use super::TEST_SHARDS;

#[test]
fn get_or_insert_with_inserts_on_miss() {
    use std::cell::Cell;
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    let calls = Cell::new(0u32);
    let v = cache.get_or_insert_with(7, || {
        calls.set(calls.get() + 1);
        42
    });
    assert_eq!(v, &42);
    assert_eq!(calls.get(), 1);
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.peek(&7), Some(&42));
}

#[test]
fn get_or_insert_with_skips_closure_on_hit() {
    use std::cell::Cell;
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    cache.insert(7, 42);
    let calls = Cell::new(0u32);
    let v = cache.get_or_insert_with(7, || {
        calls.set(calls.get() + 1);
        999
    });
    assert_eq!(v, &42);
    assert_eq!(calls.get(), 0);
    assert_eq!(cache.len(), 1);
}

#[test]
fn get_or_insert_with_promotes_on_hit() {
    // Hit path must set VISITED (same semantics as `get`), so the entry
    // survives the next SIEVE sweep.
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    let _ = cache.get_or_insert_with(1, || unreachable!("should not run on hit"));
    let evicted = cache.insert(3, 30);
    assert_eq!(evicted, Some((2, 20)));
    assert!(cache.contains_key(&1));
}

#[test]
fn get_or_insert_with_evicts_when_full() {
    // Capacity 2, single shard. Inserting a 3rd key via get_or_insert_with
    // must trigger eviction of the SIEVE victim.
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    let v = cache.get_or_insert_with(3, || 30);
    assert_eq!(v, &30);
    assert_eq!(cache.len(), 2);
    assert!(cache.contains_key(&3));
    // Oldest unvisited (1) was evicted.
    assert!(!cache.contains_key(&1));
    assert!(cache.contains_key(&2));
}
