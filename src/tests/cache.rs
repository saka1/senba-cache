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

#[test]
fn remove_basic() {
    let mut c: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    c.insert(1, 100);
    c.insert(2, 200);
    c.insert(3, 300);
    assert_eq!(c.remove(&2), Some(200));
    assert_eq!(c.get(&2), None);
    assert_eq!(c.get(&1), Some(&100));
    assert_eq!(c.get(&3), Some(&300));
    assert_eq!(c.len(), 2);
}

#[test]
fn remove_missing_returns_none() {
    let mut c: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    c.insert(1, 100);
    assert_eq!(c.remove(&999), None);
    assert_eq!(c.len(), 1);
}

/// After remove, I8 (live ids = 0..len) must be restored so that
/// the warm-up branch (`entry_id = self.len`) works correctly on the next insert.
#[test]
fn remove_then_insert_reuses_id() {
    let mut c: Cache<u64, u64, Slot32> = Cache::with_shards(4, 1);
    c.insert(1, 100);
    c.insert(2, 200);
    c.insert(3, 300);
    c.insert(4, 400);
    assert_eq!(c.len(), 4);

    // remove reduces len to 3; swap-to-fill-gap restores I8.
    assert_eq!(c.remove(&2), Some(200));
    assert_eq!(c.len(), 3);

    // Insert a 5th entry via the warm-up branch (no eviction expected).
    assert_eq!(c.insert(5, 500), None);
    assert_eq!(c.len(), 4);

    // 1, 3, 4, 5 are live; 2 is gone.
    assert_eq!(c.get(&1), Some(&100));
    assert_eq!(c.get(&2), None);
    assert_eq!(c.get(&3), Some(&300));
    assert_eq!(c.get(&4), Some(&400));
    assert_eq!(c.get(&5), Some(&500));
}

/// Removing the entry with the maximum id (no swap needed).
#[test]
fn remove_max_id_no_swap() {
    let mut c: Cache<u64, u64, Slot32> = Cache::with_shards(4, 1);
    c.insert(1, 100);
    c.insert(2, 200);
    c.insert(3, 300);
    // With warm-up ordering, key 3 gets id=2 (the max).
    assert_eq!(c.remove(&3), Some(300));
    assert_eq!(c.len(), 2);
    assert_eq!(c.get(&1), Some(&100));
    assert_eq!(c.get(&2), Some(&200));
    assert_eq!(c.get(&3), None);
}

/// Repeated remove → insert cycles must not corrupt state.
#[test]
fn remove_insert_churn() {
    let mut c: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..100u64 {
        c.insert(k, k * 11);
    }
    // Remove all even keys.
    for k in (0..100u64).step_by(2) {
        let _ = c.remove(&k);
    }
    // Only odd keys may remain (up to capacity).
    let alive: usize = (1..100u64)
        .step_by(2)
        .filter(|k| c.get(k) == Some(&(k * 11)))
        .count();
    assert!(alive > 0);
    // New inserts must succeed.
    for k in 200..220u64 {
        c.insert(k, k);
    }
    assert!(c.len() <= TEST_SHARDS * 4);
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

// ---------------- peek ----------------

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

// ---------------- get_or_insert_with ----------------

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

// ---------------- clear ----------------

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

// ---------------- retain ----------------

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

#[test]
fn get_mut_updates_in_place_and_sets_visited() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    cache.insert(1, 10);
    *cache.get_mut(&1).unwrap() += 5;
    assert_eq!(cache.get(&1), Some(&15));
    assert!(cache.get_mut(&999).is_none());
}

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

// ---------------- peek_mut ----------------

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

// ---------------- get_key_value / peek_key_value ----------------

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

#[test]
fn tags_storage_is_32_byte_aligned_for_each_slot_size() {
    // `find_avx2` issues `_mm256_load_si256` against `tags.as_ptr()`. The
    // soundness of that load relies on `Vec<TagsChunk>` inheriting
    // `repr(C, align(32))` from its element type. Verify the contract holds
    // for every public `SlotSize` and at non-trivial cap (so multiple chunks
    // are allocated). Pairs with the const-eval `_TAGSCHUNK_ALIGN_OK` guard
    // by giving `cargo test --release` (where `debug_assert!` does not fire)
    // the same coverage.
    fn check<S: SlotSize>(cap: usize, shards: usize, label: &str) {
        let cache: Cache<u64, u64, S> = Cache::with_shards(cap, shards);
        for (i, sh) in cache.shards.iter().enumerate() {
            let addr = sh.tags.as_ptr() as usize;
            assert_eq!(
                addr & 31,
                0,
                "{label}: shard {i} tags base 0x{addr:x} not 32-byte aligned"
            );
            // Flat-view byte length must be a 32B multiple — the layout
            // claim that `AlignedTags::Deref`'s SAFETY argument relies on
            // (chunks × LANE × sizeof(u16) = chunks × 32).
            let bytes = sh.tags.len() * std::mem::size_of::<u16>();
            assert!(
                bytes.is_multiple_of(32),
                "{label}: tags byte len {bytes} not a 32B multiple"
            );
        }
    }
    check::<Slot16>(384, 8, "Slot16");
    check::<Slot32>(384, 8, "Slot32");
    check::<Slot64>(256, 8, "Slot64");
    // Edge case: tiny per-shard rounds up to a single LANE.
    // cap=8 / shards=1 ⇒ per_shard=8, order_cap = max(LANE, round_up(8, LANE)) = 16.
    check::<Slot32>(8, 1, "Slot32_min");
}
