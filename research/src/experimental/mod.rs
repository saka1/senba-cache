//! Experimental SIEVE variants.
//!
//! Library-grade implementation lives in [`senba`]; the spec/oracle
//! is [`sieve_orig`]. This module collects the historical / exploratory
//! variants kept around for benchmark and design comparison.

pub mod concurrent_test_suite;
pub mod sieve_c10s;
pub mod sieve_c11s;
pub mod sieve_c12s;
pub mod sieve_c13s;
pub mod sieve_c14s;
pub mod sieve_c15s;
pub mod sieve_c16s;
pub mod sieve_c8;
pub mod sieve_c9;
pub mod sieve_j3;
pub mod sieve_j4;
pub mod sieve_j5;
pub mod sieve_j6;
pub mod sieve_j7;
pub mod sieve_j8;
pub mod sieve_orig;
pub mod sieve_v0;
pub mod sieve_v1;
pub mod sieve_v2;
pub mod sieve_v3;

/// Common interface used by the cross-variant oracle test
/// (`research/tests/oracle.rs`) and the research microbench
/// (`research/benches/micro.rs`) to drive every SIEVE variant through
/// identical traces. The publishable [`senba::Cache`] also implements
/// it via the adapter at the `senba_research` crate root.
pub trait CacheImpl<K, V> {
    fn new(capacity: usize) -> Self
    where
        Self: Sized;
    fn capacity(&self) -> usize;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// hit 時に visited bit を立てる必要があるので &mut self
    fn get(&mut self, key: &K) -> Option<&V>;

    /// 容量超過時に追い出された (K,V) を返す。oracle 比較の主データ。
    fn insert(&mut self, key: K, value: V) -> Option<(K, V)>;

    fn contains_key(&self, key: &K) -> bool;
}

/// Shared interface for the concurrent SIEVE variants
/// (`sieve_c14s` / `sieve_c15s` / `sieve_c16s` and successors). Mirrors
/// [`CacheImpl`] but for `&self` interior-mutability designs that return
/// `Option<V>` (clone) instead of `Option<&V>`. Used by
/// [`crate::experimental::concurrent_test_suite`] to drive variant-agnostic
/// scenarios.
///
/// `Send + Sync` is required so that future multi-threaded harness scenarios
/// (`Arc<Self>` fanned out across `std::thread::scope`) can be added without
/// changing the bound. The current 8 scenarios are single-threaded.
pub trait ConcurrentCacheImpl<K, V>: Send + Sync
where
    K: std::hash::Hash + Eq + Copy + Send + Sync + 'static,
    V: Copy + Send + Sync + 'static,
{
    fn with_capacity(capacity: usize) -> Self
    where
        Self: Sized;
    fn capacity(&self) -> usize;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn contains_key(&self, key: &K) -> bool;
    fn get(&self, key: &K) -> Option<V>;
    fn insert(&self, key: K, value: V) -> Option<(K, V)>;
}
