use super::super::*;
use super::TEST_SHARDS;

#[test]
fn extend_inserts_owned_pairs() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    cache.extend((0..10).map(|i| (i, i * 10)));
    assert_eq!(cache.len(), 10);
    for i in 0..10 {
        assert_eq!(cache.get(&i), Some(&(i * 10)));
    }
}

#[test]
fn extend_borrowed_pairs_via_copy() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    let src: Vec<(u64, u64)> = (0..5).map(|i| (i, i + 100)).collect();
    cache.extend(src.iter().map(|(k, v)| (k, v)));
    assert_eq!(cache.len(), 5);
    for (k, v) in &src {
        assert_eq!(cache.get(k), Some(v));
    }
}

#[test]
fn extend_overwrites_existing_keys() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    cache.insert(1, 100);
    cache.extend([(1, 200), (2, 300)]);
    assert_eq!(cache.get(&1), Some(&200));
    assert_eq!(cache.get(&2), Some(&300));
    assert_eq!(cache.len(), 2);
}

#[test]
fn extend_past_capacity_evicts_silently() {
    let cap = TEST_SHARDS * 2;
    let mut cache: Cache<u64, u64> = Cache::new(cap);
    cache.extend((0..(cap as u64) * 4).map(|i| (i, i)));
    assert_eq!(cache.len(), cap);
}
