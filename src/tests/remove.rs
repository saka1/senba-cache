use super::super::*;
use super::TEST_SHARDS;

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
