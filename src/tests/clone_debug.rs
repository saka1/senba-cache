use super::super::*;
use super::TEST_SHARDS;

#[test]
fn clone_preserves_entries_and_metadata() {
    let mut cache: Cache<u64, String> = Cache::with_shards(TEST_SHARDS * 4, TEST_SHARDS);
    for k in 0..20u64 {
        cache.insert(k, format!("v{k}"));
    }
    let cloned = cache.clone();
    assert_eq!(cloned.len(), cache.len());
    assert_eq!(cloned.capacity(), cache.capacity());
    assert_eq!(cloned.shards(), cache.shards());
    for k in 0..20u64 {
        assert_eq!(cloned.peek(&k), cache.peek(&k));
    }
}

#[test]
fn clone_is_independent_of_original() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..16u64 {
        cache.insert(k, k);
    }
    let mut cloned = cache.clone();
    cloned.insert(100, 999);
    cache.remove(&0);
    assert_eq!(cache.peek(&100), None);
    assert_eq!(cache.peek(&0), None);
    assert_eq!(cloned.peek(&100), Some(&999));
    assert_eq!(cloned.peek(&0), Some(&0));
}

#[test]
fn clone_after_eviction_matches_original() {
    let mut cache: Cache<u64, u64> = Cache::with_shards(8, 1);
    for k in 0..32u64 {
        cache.insert(k, k);
    }
    let cloned = cache.clone();
    assert_eq!(cloned.len(), cache.len());
    for k in 0..32u64 {
        assert_eq!(cloned.peek(&k), cache.peek(&k));
    }
}

#[test]
fn debug_format_contains_entries_and_metadata() {
    let mut cache: Cache<u64, u64> = Cache::with_shards(TEST_SHARDS * 4, TEST_SHARDS);
    cache.insert(1, 10);
    cache.insert(2, 20);
    let s = format!("{cache:?}");
    assert!(s.contains("Cache"));
    assert!(s.contains("capacity"));
    assert!(s.contains("len"));
    assert!(s.contains("entries"));
    assert!(s.contains("1"));
    assert!(s.contains("10"));
    assert!(s.contains("2"));
    assert!(s.contains("20"));
}

#[test]
fn debug_does_not_set_visited() {
    // Fill exactly to capacity, format!("{:?}"), then insert one more.
    // If Debug had set VISITED on every entry, the SIEVE hand would have
    // to clear all bits before evicting, and the eviction would land on
    // the hand position. With Debug non-promoting, the oldest tail entry
    // (key 0) is the victim.
    let mut cache: Cache<u64, u64> = Cache::with_shards(8, 1);
    for k in 0..8u64 {
        cache.insert(k, k);
    }
    let _ = format!("{cache:?}");
    let evicted = cache.insert(100, 100);
    assert_eq!(evicted, Some((0, 0)));
}
