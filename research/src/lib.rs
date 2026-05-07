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
