use super::super::*;
use super::TEST_SHARDS;

#[test]
fn borrow_lookup_string_via_str() {
    let mut cache: Cache<String, u64> = Cache::new(TEST_SHARDS * 4);
    cache.insert("alpha".to_string(), 1);
    cache.insert("beta".to_string(), 2);

    // get / contains_key / peek / get_mut / remove all reachable via &str
    assert_eq!(cache.get("alpha"), Some(&1));
    assert!(cache.contains_key("beta"));
    assert_eq!(cache.peek("alpha"), Some(&1));
    *cache.get_mut("beta").unwrap() = 20;
    assert_eq!(cache.get("beta"), Some(&20));
    assert_eq!(cache.remove("alpha"), Some(1));
    assert!(!cache.contains_key("alpha"));
}

#[test]
fn with_hasher_uses_custom_buildhasher() {
    use std::collections::hash_map::RandomState;

    let mut cache: Cache<u64, u64, Slot32, RandomState> =
        Cache::with_hasher(TEST_SHARDS * 4, RandomState::new());
    for k in 0..16u64 {
        assert!(cache.insert(k, k * 10).is_none());
    }
    for k in 0..16u64 {
        assert_eq!(cache.get(&k), Some(&(k * 10)));
    }
    assert_eq!(cache.len(), 16);
    assert_eq!(cache.capacity(), TEST_SHARDS * 4);
}

#[test]
fn with_shards_and_hasher_routes_through_custom_hasher() {
    use std::collections::hash_map::RandomState;

    let mut cache: Cache<String, u64, Slot32, RandomState> =
        Cache::with_shards_and_hasher(32, 4, RandomState::new());
    cache.insert("alpha".to_string(), 1);
    cache.insert("beta".to_string(), 2);
    assert_eq!(cache.shards(), 4);
    assert_eq!(cache.get("alpha"), Some(&1));
    assert_eq!(cache.get("beta"), Some(&2));
}

#[test]
#[should_panic]
fn capacity_below_shards_panics() {
    // Auto-`new` would happily build a 1-shard cache here, so only the
    // explicit `with_shards` path enforces this invariant now.
    let _: Cache<u64, u64> = Cache::with_shards(TEST_SHARDS - 1, TEST_SHARDS);
}

#[test]
#[should_panic]
fn zero_capacity_panics() {
    let _: Cache<u64, u64> = Cache::new(0);
}

#[test]
#[should_panic]
fn per_shard_above_max_panics() {
    let _: Cache<u64, u64, Slot32> = Cache::with_shards(65, 1);
}

/// `Cache::new` must pick a shard count consistent with `MAX_PER_SHARD = 64`.
#[test]
fn auto_shards_match_capacity_brackets() {
    let cases: &[(usize, usize)] = &[
        (1, 1),
        (64, 1),
        (65, 2),
        (128, 2),
        (129, 4),
        (512, 8),
        (513, 16),
    ];
    for &(cap, expected_shards) in cases {
        let c: Cache<u64, u64> = Cache::new(cap);
        assert_eq!(
            c.shards(),
            expected_shards,
            "auto-shards mismatch at capacity={cap}"
        );
        assert_eq!(c.capacity(), cap);
    }
}
