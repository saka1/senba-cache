//! Experimental SIEVE variants.
//!
//! Library-grade implementation lives in [`senba`]; the spec/oracle
//! is [`sieve_orig`]. This module collects the historical / exploratory
//! variants kept around for benchmark and design comparison.

pub mod sieve_c10s;
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
