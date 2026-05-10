use super::super::*;
use super::TEST_SHARDS;

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
