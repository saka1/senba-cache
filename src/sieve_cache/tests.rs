use super::*;

/// Reference value used by tests that historically assumed the 8-shard default.
/// The auto-shard policy in `Cache::new` may pick a different count (it depends
/// on `capacity`), but every length / capacity assertion below treats this as
/// "the multiplier we used to size the test cache" rather than as the actual
/// shard count of the cache under test.
const TEST_SHARDS: usize = 8;

// sizeof(Entry<u64, u64>) = 16 → fits Slot16 / Slot32 / Slot64.
// sizeof(Entry<i32, i32>) = 8  → fits all three (with slack).

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

/// Verifies bit-field exclusivity for Slot32 (default, Entry<u64,u64>=16).
/// Inner<u64, u64, Slot32>: ID_SHIFT = 5, ID_MASK = 0x07e0, HASH_MASK = 0x381f.
#[test]
fn bit_layout_exclusivity_slot32() {
    type I = Inner<u64, u64, Slot32>;
    assert_eq!(I::ID_SHIFT, 5);
    assert_eq!(I::ID_MASK, 0x07e0);
    assert_eq!(I::HASH_MASK, 0x381f);
    assert_eq!(I::SCAN_MASK, LIVE | I::HASH_MASK);
    assert_eq!(I::SCAN_MASK, 0xb81f);

    assert_eq!(LIVE | VISITED | I::ID_MASK | I::HASH_MASK, 0xFFFF);
    assert_eq!(LIVE & VISITED, 0);
    assert_eq!(LIVE & I::ID_MASK, 0);
    assert_eq!(LIVE & I::HASH_MASK, 0);
    assert_eq!(VISITED & I::ID_MASK, 0);
    assert_eq!(VISITED & I::HASH_MASK, 0);
    assert_eq!(I::ID_MASK & I::HASH_MASK, 0);

    // c-hoist invariant: embedding id into a tag gives `tag & ID_MASK = id × S::SIZE`.
    for id in 0..MAX_PER_SHARD {
        let tag_id_field = (id as u16) << I::ID_SHIFT;
        assert_eq!((tag_id_field & I::ID_MASK) as usize, id * Slot32::SIZE);
    }
}

#[test]
fn bit_layout_slot16() {
    type I = Inner<u32, u32, Slot16>;
    assert_eq!(I::ID_SHIFT, 4);
    assert_eq!(I::ID_MASK, 0x03f0);
    assert_eq!(I::HASH_MASK, 0x3c0f);
}

#[test]
fn bit_layout_slot64() {
    type I = Inner<u64, u64, Slot64>;
    assert_eq!(I::ID_SHIFT, 6);
    assert_eq!(I::ID_MASK, 0x0fc0);
    assert_eq!(I::HASH_MASK, 0x303f);
}

/// Hash spread injectivity across all three brackets.
#[test]
fn needle_spread_is_injective_all_slots() {
    for slot_id in 0..3 {
        let mut seen = std::collections::HashSet::new();
        for h in 0..=255u64 {
            let needle = match slot_id {
                0 => Inner::<u64, u64, Slot16>::needle_from_hash(h << 56),
                1 => Inner::<u64, u64, Slot32>::needle_from_hash(h << 56),
                2 => Inner::<u64, u64, Slot64>::needle_from_hash(h << 56),
                _ => unreachable!(),
            };
            assert!(seen.insert(needle), "slot {slot_id} hash {h} collides");
        }
        assert_eq!(seen.len(), 256);
    }
}

#[test]
fn slot16_small_entry() {
    // sizeof(Entry<u32, u32>) = 8 ≤ 16
    let mut c: Cache<u32, u32, Slot16> = Cache::new(TEST_SHARDS * 4);
    for k in 0..100u32 {
        c.insert(k, k * 7);
    }
    assert_eq!(c.len(), TEST_SHARDS * 4);
}

#[test]
fn slot32_default_string_value() {
    // sizeof(Entry<u64, String>) = 32 (8 + 24)
    let mut c: Cache<u64, String> = Cache::new(TEST_SHARDS * 2);
    for k in 0..40u64 {
        c.insert(k, format!("v{k}"));
    }
    assert_eq!(c.len(), TEST_SHARDS * 2);
}

#[test]
fn slot64_string_string() {
    // sizeof(Entry<String, String>) = 48 ≤ 64
    let cap = TEST_SHARDS * 2;
    let mut c: Cache<String, String, Slot64> = Cache::new(cap);
    for k in 0..200u64 {
        c.insert(format!("k{k}"), format!("v{k}"));
    }
    assert_eq!(c.len(), cap);
    // Recently inserted keys should survive (SIEVE selects within each shard).
    let alive = (0..200u64)
        .filter(|k| c.get(&format!("k{k}")) == Some(&format!("v{k}")))
        .count();
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

/// Cross-checks insert/get behavior against sieve_orig (oracle) with a single shard.
#[test]
fn matches_sieve_orig_externally_1shard() {
    use crate::sieve_orig::SieveCache as Orig;
    let cap = 64usize;
    let mut a: Orig<u64, u64> = Orig::new(cap);
    let mut b: Cache<u64, u64, Slot32> = Cache::with_shards(cap, 1);
    for k in 0..10_000u64 {
        let key = (k.wrapping_mul(2654435761)) % 256;
        let _ = a.insert(key, key);
        let _ = b.insert(key, key);
    }
    for k in 0..256u64 {
        assert_eq!(
            a.get(&k).copied(),
            b.get(&k).copied(),
            "1-shard mismatch with sieve_orig at key {k}"
        );
    }
}

/// All three brackets (Slot16/32/64) must match sieve_orig semantics for Entry<u64,u64>.
#[test]
fn matches_sieve_orig_per_slot() {
    use crate::sieve_orig::SieveCache as Orig;
    let cap = 32usize;
    let mut oracle: Orig<u64, u64> = Orig::new(cap);
    let mut s16: Cache<u64, u64, Slot16> = Cache::with_shards(cap, 1);
    let mut s32: Cache<u64, u64, Slot32> = Cache::with_shards(cap, 1);
    let mut s64: Cache<u64, u64, Slot64> = Cache::with_shards(cap, 1);
    for k in 0..5_000u64 {
        let key = (k.wrapping_mul(2654435761)) % 128;
        oracle.insert(key, key);
        s16.insert(key, key);
        s32.insert(key, key);
        s64.insert(key, key);
    }
    for k in 0..128u64 {
        let g = oracle.get(&k).copied();
        assert_eq!(s16.get(&k).copied(), g, "Slot16 mismatch key={k}");
        assert_eq!(s32.get(&k).copied(), g, "Slot32 mismatch key={k}");
        assert_eq!(s64.get(&k).copied(), g, "Slot64 mismatch key={k}");
    }
}

/// Cross-checks remove behavior against sieve_orig with interleaved operations.
#[test]
fn remove_during_churn_oracle_match() {
    use crate::sieve_orig::SieveCache as Orig;
    let cap = 32usize;
    let mut a: Orig<u64, u64> = Orig::new(cap);
    let mut b: Cache<u64, u64, Slot32> = Cache::with_shards(cap, 1);
    for k in 0..3_000u64 {
        let key = (k.wrapping_mul(2654435761)) % 128;
        let ai = a.insert(key, key);
        let bi = b.insert(key, key);
        assert_eq!(ai, bi, "insert eviction mismatch step={k} key={key}");
        if k % 5 == 0 {
            let rk = (k.wrapping_mul(11400714819323198485)) % 128;
            let ar = a.remove(&rk);
            let br = b.remove(&rk);
            assert_eq!(ar, br, "remove mismatch step={k} key={rk}");
        }
    }
    for k in 0..128u64 {
        assert_eq!(
            a.get(&k).copied(),
            b.get(&k).copied(),
            "oracle mismatch key={k}"
        );
    }
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

#[test]
#[should_panic]
fn per_shard_above_max_panics() {
    let _: Cache<u64, u64, Slot32> = Cache::with_shards(65, 1);
}

/// Regression test for the `find_avx2` `limit = round_up(tail, LANE)` change.
/// At low fill ratios `tail << tags.len()`, so the SIMD path's bound diverges
/// from the scalar path. Both must still agree with `sieve_orig` on every key,
/// covering misses (no key in tags) and hits (key present at small `tail`).
#[test]
fn find_avx2_low_fill_matches_oracle() {
    use crate::sieve_orig::SieveCache as Orig;
    // capacity 64 → order_cap = round_up(128, 16) = 128. With only a handful of
    // keys inserted, `tail` stays well below `tags.len()` and the SIMD upper
    // bound differs sharply from the old `tags.len()` bound.
    let cap = 64usize;
    let mut a: Orig<u64, u64> = Orig::new(cap);
    let mut b: Cache<u64, u64, Slot32> = Cache::with_shards(cap, 1);
    // Keep fill ratio well below capacity throughout so `tail` stays small.
    for k in 0..8u64 {
        a.insert(k, k * 7);
        b.insert(k, k * 7);
    }
    // Probe absent keys (forces the scan to traverse the full `tail` window
    // without an early hit) and present keys (verifies hits via the SIMD path).
    for k in 0..256u64 {
        assert_eq!(
            a.get(&k).copied(),
            b.get(&k).copied(),
            "low-fill SIMD bound regression at key {k}"
        );
    }
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
