//! Variant-agnostic test suite for concurrent SIEVE variants.
//!
//! Each scenario is a generic function over `C: ConcurrentCacheImpl<u64, u64>`;
//! the [`concurrent_suite!`] macro expands a fixed set of `#[test]` wrappers
//! per variant. Variant-side `mod tests` only needs one macro invocation to
//! pick up the whole suite.
//!
//! ## Onboarding a new variant
//!
//! Inside the variant's `#[cfg(test)] mod tests` block, write the adapter
//! `impl ConcurrentCacheImpl<u64, u64> for ConcurrentSieveCache<u64, u64>`
//! (see `sieve_c16s.rs` for a 17-line example) and then append:
//!
//! ```ignore
//! crate::concurrent_suite!(ConcurrentSieveCache<u64, u64>);
//! ```
//!
//! ## Out of scope (intentionally kept variant-side)
//! - 1-shard deterministic eviction order tests (e.g. `evicts_oldest_when_full_and_unvisited`)
//! - Internal-structure tests that touch `shard()` / `live_ids()` / tag bit layout
//! - 1-shard oracle equivalence (`matches_sieve_orig_externally_1shard`)

use crate::experimental::ConcurrentCacheImpl;

const TEST_CAP_SMALL: usize = 32;
const TEST_CAP_CHURN: usize = 128;
const TEST_CAP_VISIBILITY: usize = 256;

pub fn cache_initially_empty<C: ConcurrentCacheImpl<u64, u64>>() {
    let cache = C::with_capacity(TEST_CAP_SMALL);
    assert_eq!(cache.len(), 0);
    assert_eq!(cache.capacity(), TEST_CAP_SMALL);
    assert!(cache.is_empty());
}

pub fn insert_then_get<C: ConcurrentCacheImpl<u64, u64>>() {
    let cache = C::with_capacity(TEST_CAP_SMALL);
    assert!(cache.insert(1, 10).is_none());
    assert_eq!(cache.get(&1), Some(10));
    assert_eq!(cache.len(), 1);
}

pub fn get_missing_returns_none<C: ConcurrentCacheImpl<u64, u64>>() {
    let cache = C::with_capacity(TEST_CAP_SMALL);
    cache.insert(1, 10);
    assert_eq!(cache.get(&2), None);
}

pub fn contains_key_reflects_insertions<C: ConcurrentCacheImpl<u64, u64>>() {
    let cache = C::with_capacity(TEST_CAP_SMALL);
    assert!(!cache.contains_key(&1));
    cache.insert(1, 10);
    assert!(cache.contains_key(&1));
    assert!(!cache.contains_key(&2));
}

pub fn insert_existing_key_updates_value<C: ConcurrentCacheImpl<u64, u64>>() {
    let cache = C::with_capacity(TEST_CAP_SMALL);
    cache.insert(1, 10);
    assert!(cache.insert(1, 20).is_none());
    assert_eq!(cache.get(&1), Some(20));
    assert_eq!(cache.len(), 1);
}

pub fn total_capacity_is_respected_under_churn<C: ConcurrentCacheImpl<u64, u64>>() {
    let cache = C::with_capacity(TEST_CAP_CHURN);
    for k in 0..10_000u64 {
        cache.insert(k, k);
        assert!(cache.len() <= TEST_CAP_CHURN);
    }
    assert_eq!(cache.len(), TEST_CAP_CHURN);
}

pub fn churn_keeps_a_full_capacity_set<C: ConcurrentCacheImpl<u64, u64>>() {
    let cache = C::with_capacity(TEST_CAP_CHURN);
    for k in 0..50_000u64 {
        cache.insert(k, k * 3);
    }
    assert_eq!(cache.len(), TEST_CAP_CHURN);
    let mut alive = 0;
    for k in 0..50_000u64 {
        if cache.get(&k) == Some(k * 3) {
            alive += 1;
        }
    }
    assert_eq!(alive, TEST_CAP_CHURN);
}

pub fn self_insert_self_get_visibility<C: ConcurrentCacheImpl<u64, u64>>() {
    let cache = C::with_capacity(TEST_CAP_VISIBILITY);
    for k in 0..200u64 {
        cache.insert(k, k * 17);
        assert_eq!(
            cache.get(&k),
            Some(k * 17),
            "直後の self-get で miss: k={k}"
        );
    }
}

/// Expands to a fixed set of `#[test]` wrappers around the generic suite
/// functions, parameterized on the variant's concrete cache type. Place at the
/// end of each variant's `#[cfg(test)] mod tests` block:
///
/// ```ignore
/// // From inside the senba_research crate (variant authors):
/// crate::concurrent_suite!(ConcurrentSieveCache<u64, u64>);
/// // From outside the crate (e.g. integration tests):
/// senba_research::concurrent_suite!(ConcurrentSieveCache<u64, u64>);
/// ```
#[macro_export]
macro_rules! concurrent_suite {
    ($cache_ty:ty) => {
        #[test]
        fn cache_initially_empty() {
            $crate::experimental::concurrent_test_suite::cache_initially_empty::<$cache_ty>();
        }
        #[test]
        fn insert_then_get() {
            $crate::experimental::concurrent_test_suite::insert_then_get::<$cache_ty>();
        }
        #[test]
        fn get_missing_returns_none() {
            $crate::experimental::concurrent_test_suite::get_missing_returns_none::<$cache_ty>();
        }
        #[test]
        fn contains_key_reflects_insertions() {
            $crate::experimental::concurrent_test_suite::contains_key_reflects_insertions::<
                $cache_ty,
            >();
        }
        #[test]
        fn insert_existing_key_updates_value() {
            $crate::experimental::concurrent_test_suite::insert_existing_key_updates_value::<
                $cache_ty,
            >();
        }
        #[test]
        fn total_capacity_is_respected_under_churn() {
            $crate::experimental::concurrent_test_suite::total_capacity_is_respected_under_churn::<
                $cache_ty,
            >();
        }
        #[test]
        fn churn_keeps_a_full_capacity_set() {
            $crate::experimental::concurrent_test_suite::churn_keeps_a_full_capacity_set::<
                $cache_ty,
            >();
        }
        #[test]
        fn self_insert_self_get_visibility() {
            $crate::experimental::concurrent_test_suite::self_insert_self_get_visibility::<
                $cache_ty,
            >();
        }
    };
}
