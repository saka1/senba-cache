//! Research-only surface for the `senba-cache` repository.
//!
//! This crate is **not published**. It collects the historical / exploratory
//! SIEVE variants ([`experimental`]) — including the NSDI'24 author-reference
//! port [`experimental::sieve_orig`] used as the correctness oracle — plus
//! the trace-replay / Zipf utilities ([`workload`]) that drive the oracle
//! tests, the micro-bench, and the perf-gate.
//!
//! The publishable [`senba::Cache`] also implements [`CacheImpl`] here, so
//! cross-variant drivers can drop it in alongside the experimental variants.

pub mod experimental;
pub mod single_shard;
pub mod workload;

pub use experimental::CacheImpl;

/// Adapter so `senba::Cache` can stand in for any [`CacheImpl`] consumer
/// (research drivers / micro-bench / oracle test). The body is the
/// pre-split inherent-method delegation; nothing here uses senba internals.
impl<K, V, S> CacheImpl<K, V> for senba::Cache<K, V, S>
where
    K: std::hash::Hash + Eq,
    S: senba::SlotSize,
{
    fn new(capacity: usize) -> Self {
        Self::new(capacity)
    }
    fn capacity(&self) -> usize {
        self.capacity()
    }
    fn len(&self) -> usize {
        self.len()
    }
    fn get(&mut self, key: &K) -> Option<&V> {
        self.get(key)
    }
    fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        self.insert(key, value)
    }
    fn contains_key(&self, key: &K) -> bool {
        self.contains_key(key)
    }
}

/// Adapter that lets the concurrent `senba::concurrent::Cache` plug into the
/// research-side cross-variant harness (`bench_concurrent`,
/// `concurrent_test_suite!`). Gated to match the publishable type's own
/// `x86_64 + non-miri` availability.
#[cfg(all(target_arch = "x86_64", not(miri)))]
impl<K, V> experimental::ConcurrentCacheImpl<K, V> for senba::concurrent::Cache<K, V>
where
    K: std::hash::Hash + Eq + Copy + Send + Sync + 'static,
    V: Copy + Send + Sync + 'static,
{
    fn with_capacity(capacity: usize) -> Self {
        Self::new(capacity)
    }
    fn capacity(&self) -> usize {
        self.capacity()
    }
    fn len(&self) -> usize {
        self.len()
    }
    fn contains_key(&self, key: &K) -> bool {
        self.contains_key(key)
    }
    fn get(&self, key: &K) -> Option<V> {
        self.get(key)
    }
    fn insert(&self, key: K, value: V) -> Option<(K, V)> {
        self.insert(key, value)
    }
}
