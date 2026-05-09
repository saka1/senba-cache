use super::super::*;
use super::TEST_SHARDS;

/// Verifies bit-field exclusivity for Slot32 (default, Entry<u64,u64>=16).
/// Shard<u64, u64, Slot32>: ID_SHIFT = 5, ID_MASK = 0x07e0, HASH_MASK = 0x781f.
/// (HASH_MASK gained one bit vs the original layout when VISITED moved out
/// of the tag into a per-shard u64 bitmap.)
#[test]
fn bit_layout_exclusivity_slot32() {
    type I = Shard<u64, u64, Slot32>;
    assert_eq!(I::ID_SHIFT, 5);
    assert_eq!(I::ID_MASK, 0x07e0);
    assert_eq!(I::HASH_MASK, 0x781f);
    assert_eq!(I::SCAN_MASK, LIVE | I::HASH_MASK);
    assert_eq!(I::SCAN_MASK, 0xf81f);

    // After dropping VISITED, the only remaining status bit is LIVE; the
    // 15-bit non-LIVE region is partitioned exactly by ID_MASK + HASH_MASK.
    assert_eq!(LIVE | I::ID_MASK | I::HASH_MASK, 0xFFFF);
    assert_eq!(LIVE & I::ID_MASK, 0);
    assert_eq!(LIVE & I::HASH_MASK, 0);
    assert_eq!(I::ID_MASK & I::HASH_MASK, 0);
    assert_eq!(I::HASH_MASK.count_ones(), 9);

    // c-hoist invariant: embedding id into a tag gives `tag & ID_MASK = id × S::SIZE`.
    for id in 0..MAX_PER_SHARD {
        let tag_id_field = (id as u16) << I::ID_SHIFT;
        assert_eq!((tag_id_field & I::ID_MASK) as usize, id * Slot32::SIZE);
    }
}

#[test]
fn bit_layout_slot16() {
    type I = Shard<u32, u32, Slot16>;
    assert_eq!(I::ID_SHIFT, 4);
    assert_eq!(I::ID_MASK, 0x03f0);
    assert_eq!(I::HASH_MASK, 0x7c0f);
    assert_eq!(I::HASH_MASK.count_ones(), 9);
}

#[test]
fn bit_layout_slot64() {
    type I = Shard<u64, u64, Slot64>;
    assert_eq!(I::ID_SHIFT, 6);
    assert_eq!(I::ID_MASK, 0x0fc0);
    assert_eq!(I::HASH_MASK, 0x703f);
    assert_eq!(I::HASH_MASK.count_ones(), 9);
}

/// Hash spread injectivity across all three brackets, now over the full 9-bit
/// hash field (was 8 bits before the VISITED-bitmap refactor).
#[test]
fn needle_spread_is_injective_all_slots() {
    for slot_id in 0..3 {
        let mut seen = std::collections::HashSet::new();
        for h in 0..=511u64 {
            // `needle_from_hash` reads bits [55, 64) of the input hash.
            let needle = match slot_id {
                0 => Shard::<u64, u64, Slot16>::needle_from_hash(h << 55),
                1 => Shard::<u64, u64, Slot32>::needle_from_hash(h << 55),
                2 => Shard::<u64, u64, Slot64>::needle_from_hash(h << 55),
                _ => unreachable!(),
            };
            assert!(seen.insert(needle), "slot {slot_id} hash {h} collides");
        }
        assert_eq!(seen.len(), 512);
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
