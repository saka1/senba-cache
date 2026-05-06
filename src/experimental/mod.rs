//! Experimental SIEVE variants.
//!
//! Library-grade implementation lives in [`crate::sieve_cache`]; the spec/oracle
//! is [`crate::sieve_orig`]. This module collects the historical / exploratory
//! variants kept around for benchmark and design comparison.

pub mod sieve_c8;
pub mod sieve_j3;
pub mod sieve_j4;
pub mod sieve_j5;
pub mod sieve_j6;
pub mod sieve_j7;
pub mod sieve_j8;
pub mod sieve_v0;
pub mod sieve_v1;
pub mod sieve_v2;
pub mod sieve_v3;

/// Common interface used by the cross-variant oracle test
/// (`tests/oracle.rs`) and the research microbench
/// (`benches/micro.rs`) to drive every SIEVE variant through identical
/// traces. Lives under `experimental` because every consumer is research
/// / dev tooling — the publishable surface
/// ([`crate::sieve_cache::Cache`]) implements it only when the
/// `experimental` feature is enabled.
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
