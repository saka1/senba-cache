use super::super::*;
use super::TEST_SHARDS;

// ---------------- iter ----------------

#[test]
fn iter_empty_yields_nothing() {
    let cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    assert_eq!(cache.iter().count(), 0);
}

#[test]
fn iter_yields_all_live_entries() {
    use std::collections::HashSet;
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..16u64 {
        cache.insert(k, k * 10);
    }
    let collected: HashSet<(u64, u64)> = cache.iter().map(|(&k, &v)| (k, v)).collect();
    let expected: HashSet<(u64, u64)> = (0..16u64).map(|k| (k, k * 10)).collect();
    assert_eq!(collected, expected);
    assert_eq!(cache.iter().count(), 16);
}

#[test]
fn iter_after_removal_skips_removed_keys() {
    use std::collections::HashSet;
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..10u64 {
        cache.insert(k, k);
    }
    cache.remove(&3);
    cache.remove(&7);
    let keys: HashSet<u64> = cache.iter().map(|(&k, _)| k).collect();
    assert!(!keys.contains(&3));
    assert!(!keys.contains(&7));
    assert_eq!(keys.len(), 8);
}

#[test]
fn iter_does_not_promote() {
    // Iterate fully over a full cache; the next eviction must still target
    // the SIEVE-oldest unvisited entry, not be perturbed by iteration.
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    let _: Vec<_> = cache.iter().collect();
    let evicted = cache.insert(3, 30);
    assert_eq!(evicted, Some((1, 10)));
}

// ---------------- iter_mut / keys / values ----------------

#[test]
fn iter_mut_yields_all_live_entries_and_allows_mutation() {
    use std::collections::HashSet;
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..16u64 {
        cache.insert(k, k * 10);
    }
    for (_, v) in cache.iter_mut() {
        *v += 1;
    }
    let collected: HashSet<(u64, u64)> = cache.iter().map(|(&k, &v)| (k, v)).collect();
    let expected: HashSet<(u64, u64)> = (0..16u64).map(|k| (k, k * 10 + 1)).collect();
    assert_eq!(collected, expected);
}

#[test]
fn iter_mut_does_not_promote() {
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    for (_, v) in cache.iter_mut() {
        *v += 100;
    }
    // SIEVE-oldest unvisited entry is still 1 — mutation through iter_mut
    // must not have set VISITED.
    let evicted = cache.insert(3, 30);
    assert_eq!(evicted, Some((1, 110)));
}

#[test]
fn iter_mut_empty_yields_nothing() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    assert_eq!(cache.iter_mut().count(), 0);
}

#[test]
fn keys_and_values_match_iter() {
    use std::collections::HashSet;
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..10u64 {
        cache.insert(k, k * 7);
    }
    let ks: HashSet<u64> = cache.keys().copied().collect();
    let vs: HashSet<u64> = cache.values().copied().collect();
    let expected_k: HashSet<u64> = (0..10u64).collect();
    let expected_v: HashSet<u64> = (0..10u64).map(|k| k * 7).collect();
    assert_eq!(ks, expected_k);
    assert_eq!(vs, expected_v);
}

// ---------------- IntoIterator ----------------

#[test]
fn into_iter_ref_matches_iter() {
    use std::collections::HashSet;
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..12u64 {
        cache.insert(k, k * 3);
    }
    let via_for: HashSet<(u64, u64)> = (&cache).into_iter().map(|(&k, &v)| (k, v)).collect();
    let via_iter: HashSet<(u64, u64)> = cache.iter().map(|(&k, &v)| (k, v)).collect();
    assert_eq!(via_for, via_iter);
}

#[test]
fn into_iter_mut_allows_mutation_and_does_not_promote() {
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    for (_, v) in &mut cache {
        *v += 100;
    }
    // VISITED must not be set by IntoIterator-for-&mut: SIEVE-oldest is still 1.
    let evicted = cache.insert(3, 30);
    assert_eq!(evicted, Some((1, 110)));
}

// ---------------- drain ----------------

#[test]
fn drain_yields_every_inserted_pair_once() {
    use std::collections::HashMap;
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    let expected: HashMap<u64, u64> = (0..16u64).map(|k| (k, k * 10)).collect();
    for (&k, &v) in &expected {
        cache.insert(k, v);
    }
    let drained: HashMap<u64, u64> = cache.drain().collect();
    assert_eq!(drained, expected);
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
}

#[test]
fn drain_size_hint_is_exact_and_decreases() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..16u64 {
        cache.insert(k, k);
    }
    let mut d = cache.drain();
    assert_eq!(d.size_hint(), (16, Some(16)));
    let _ = d.next();
    assert_eq!(d.size_hint(), (15, Some(15)));
    let consumed = d.count();
    assert_eq!(consumed, 15);
}

#[test]
fn drain_preserves_capacity_and_allows_reuse() {
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(2, 1);
    cache.insert(1, 10);
    cache.insert(2, 20);
    let _: Vec<(u64, u64)> = cache.drain().collect();
    assert_eq!(cache.capacity(), 2);
    // Refilling after drain must work like a fresh cache (warm-up branch,
    // no spurious eviction until full).
    assert!(cache.insert(3, 30).is_none());
    assert!(cache.insert(4, 40).is_none());
    assert_eq!(cache.get(&3), Some(&30));
    assert_eq!(cache.get(&4), Some(&40));
    let evicted = cache.insert(5, 50);
    assert!(evicted.is_some());
}

#[test]
fn drain_drops_remaining_when_dropped_early() {
    // String values exercise drop correctness: leaking the cache via early
    // Drain::drop must not leak K/V (every entry's drop should run exactly once).
    let mut cache: Cache<u64, String> = Cache::new(TEST_SHARDS * 2);
    for k in 0..32u64 {
        cache.insert(k, format!("value-{k}"));
    }
    {
        let mut d = cache.drain();
        // Pull just two items; let the rest go through Drop's cleanup pass.
        let _ = d.next();
        let _ = d.next();
    }
    assert_eq!(cache.len(), 0);
    // Cache must remain usable.
    cache.insert(100, "after-drain".into());
    assert_eq!(cache.get(&100), Some(&"after-drain".to_string()));
}

#[test]
fn drain_on_empty_cache_yields_nothing() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    let v: Vec<(u64, u64)> = cache.drain().collect();
    assert!(v.is_empty());
    assert!(cache.is_empty());
    cache.insert(1, 10);
    assert_eq!(cache.get(&1), Some(&10));
}

#[test]
fn drain_is_visible_to_cache_immediately() {
    // While the Drain is alive, the cache reports len=0 (it borrows &mut so
    // the user can't observe this directly, but Drop of Drain must keep the
    // promise — len=0 after drop too).
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..8u64 {
        cache.insert(k, k);
    }
    let d = cache.drain();
    drop(d);
    assert_eq!(cache.len(), 0);
}

#[test]
fn drain_does_not_count_as_eviction() {
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
    for k in 0..16u64 {
        cache.insert(k, k);
    }
    let before = cache.stats();
    let _: Vec<(u64, u64)> = cache.drain().collect();
    let after = cache.stats();
    // Insertions / hits / misses are unchanged by drain itself; evictions
    // must not have grown (drain is explicit removal, not capacity pressure).
    assert_eq!(after.evictions, before.evictions);
    assert_eq!(after.insertions, before.insertions);
    assert_eq!(after.hits, before.hits);
    assert_eq!(after.misses, before.misses);
}

#[test]
fn drain_then_insert_does_not_resurrect_old_keys() {
    // Specifically guards against stale LIVE tags surviving into the SIMD
    // scan window after drain. If `Drain::new` failed to zero tags[0..len],
    // a fresh insert's `find` could match a stale tag and read an already-
    // moved entries arena slot.
    let mut cache: Cache<u64, u64, Slot32> = Cache::with_shards(4, 1);
    for k in 0..4u64 {
        cache.insert(k, k * 10);
    }
    let _: Vec<(u64, u64)> = cache.drain().collect();
    // Keys from the previous epoch must not be visible.
    for k in 0..4u64 {
        assert!(cache.get(&k).is_none());
    }
    // Re-insert and confirm correct values land.
    cache.insert(0, 999);
    assert_eq!(cache.get(&0), Some(&999));
}

#[test]
fn drain_preserves_only_partially_filled_shards() {
    // Mix shard fill levels to ensure per-shard old_lens snapshot is honored
    // (no out-of-bounds id reads on under-filled shards).
    let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 8);
    cache.insert(1, 10);
    cache.insert(2, 20);
    cache.insert(3, 30);
    let mut drained: Vec<(u64, u64)> = cache.drain().collect();
    drained.sort();
    assert_eq!(drained, vec![(1, 10), (2, 20), (3, 30)]);
    assert!(cache.is_empty());
}

#[test]
fn drain_via_mem_forget_leaves_cache_consistent() {
    // mem::forget(drain) must leave the cache safe to use, even though it
    // leaks any still-pending entries. Use Box<u64> values so a leak is
    // observable to miri/asan but does not affect correctness assertions.
    let mut cache: Cache<u64, Box<u64>> = Cache::new(TEST_SHARDS * 4);
    for k in 0..16u64 {
        cache.insert(k, Box::new(k));
    }
    let d = cache.drain();
    std::mem::forget(d);
    // Cache must report empty.
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
    // No previously-inserted key should be reachable.
    for k in 0..16u64 {
        assert!(cache.get(&k).is_none());
    }
    // Cache must be reusable: fresh inserts succeed and read back correctly.
    for k in 100..108u64 {
        cache.insert(k, Box::new(k));
    }
    for k in 100..108u64 {
        assert_eq!(cache.get(&k).map(|b| **b), Some(k));
    }
    // Note: the original 16 boxes are leaked (acceptable: mem::forget
    // already promised this).
}

// ---------------- end drain ----------------
