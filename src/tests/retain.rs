use super::super::*;
use super::TEST_SHARDS;

#[test]
fn retain_keep_all_is_noop() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..16u64 {
        cache.insert(k, k * 10);
    }
    cache.retain(|_, _| true);
    assert_eq!(cache.len(), 16);
    for k in 0..16u64 {
        assert_eq!(cache.get(&k), Some(&(k * 10)));
    }
}

#[test]
fn retain_drop_all_empties_cache() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..16u64 {
        cache.insert(k, k * 10);
    }
    cache.retain(|_, _| false);
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
    // Cache must still be usable after a wipe-via-retain.
    assert!(cache.insert(99, 990).is_none());
    assert_eq!(cache.get(&99), Some(&990));
}

#[test]
fn retain_drops_some_keeps_others() {
    use std::collections::HashSet;
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..16u64 {
        cache.insert(k, k * 10);
    }
    cache.retain(|k, _| k % 2 == 0);
    let keys: HashSet<u64> = cache.iter().map(|(&k, _)| k).collect();
    let expected: HashSet<u64> = (0..16u64).filter(|k| k % 2 == 0).collect();
    assert_eq!(keys, expected);
    assert_eq!(cache.len(), 8);
    for k in 0..16u64 {
        if k % 2 == 0 {
            assert_eq!(cache.get(&k), Some(&(k * 10)));
        } else {
            assert_eq!(cache.get(&k), None);
        }
    }
}

#[test]
fn retain_predicate_can_mutate_value() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..8u64 {
        cache.insert(k, k);
    }
    cache.retain(|_k, v| {
        *v *= 2;
        true
    });
    for k in 0..8u64 {
        assert_eq!(cache.get(&k), Some(&(k * 2)));
    }
}

/// After retain, I8 (live ids = 0..len) must be restored so warm-up inserts
/// (`entry_id = self.len`) reuse the freed arena slots correctly.
#[test]
fn retain_restores_i8() {
    // Single shard, capacity 4, fill it, drop two non-adjacent entries.
    let mut c: Cache<u64, u64, Slot32> = Cache::with_shards(4, 1);
    c.insert(1, 100);
    c.insert(2, 200);
    c.insert(3, 300);
    c.insert(4, 400);
    assert_eq!(c.len(), 4);

    // Drop keys with mid + max ids → forces both id-remap (high → low) and
    // straight retention paths.
    c.retain(|k, _| *k != 2 && *k != 4);
    assert_eq!(c.len(), 2);
    assert_eq!(c.get(&1), Some(&100));
    assert_eq!(c.get(&3), Some(&300));

    // Warm-up branch must work: two more inserts should land without eviction.
    assert!(c.insert(5, 500).is_none());
    assert!(c.insert(6, 600).is_none());
    assert_eq!(c.len(), 4);
    for &k in &[1u64, 3, 5, 6] {
        assert!(c.contains_key(&k));
    }
    assert!(!c.contains_key(&2));
    assert!(!c.contains_key(&4));

    // Now full → next insert must evict.
    let evicted = c.insert(7, 700);
    assert!(evicted.is_some());
}

#[test]
fn retain_does_not_promote_survivors() {
    // Single shard, capacity 2. Insert 1, 2 (both unvisited). retain keeps
    // both (predicate always true). If retain had set VISITED on survivors,
    // a follow-up insert(3) would evict 2 (the unvisited tail moves up). The
    // correct behavior is identical to no-op: insert(3) evicts 1.
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    cache.retain(|_, _| true);
    let evicted = cache.insert(3, 30);
    assert_eq!(evicted, Some((1, 10)));
}

#[test]
fn retain_drops_string_values_no_leak() {
    // Exercises drop path for retained-out entries (no double-drop, no leak
    // under miri/asan).
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
    {
        let mut cache: Cache<u64, Bomb> = Cache::new(cap);
        for k in 0..(cap as u64) {
            cache.insert(k, Bomb { ctr: drops.clone() });
        }
        // Drop half via retain.
        cache.retain(|k, _| k % 2 == 0);
        assert_eq!(
            drops.load(Ordering::Relaxed),
            cap / 2,
            "retain should drop exactly the predicate-false entries"
        );
        // Remaining drop when cache goes out of scope.
    }
    assert_eq!(
        drops.load(Ordering::Relaxed),
        cap,
        "every inserted Bomb should drop exactly once across retain + Cache::drop"
    );
}

#[test]
fn retain_on_empty_is_noop() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    cache.retain(|_, _| {
        panic!("predicate must not run on empty cache");
    });
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
}

#[test]
fn retain_predicate_panic_leaves_cache_consistent() {
    // After a panic in the predicate, the cache is reset to empty. The key
    // requirement is that subsequent operations are safe (no UAF) — checked
    // here by re-inserting and reading back.
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..16u64 {
        cache.insert(k, k);
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cache.retain(|k, _| {
            if *k == 8 {
                panic!("oops");
            }
            true
        });
    }));
    assert!(result.is_err());
    // Cache is empty and usable.
    assert_eq!(cache.len(), 0);
    cache.insert(100, 1000);
    assert_eq!(cache.get(&100), Some(&1000));
}
