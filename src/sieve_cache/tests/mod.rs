//! Unit tests for `sieve_cache`. Split by topic to mirror the module layout:
//! `slot.rs` covers the `SlotSize` machinery, `stats.rs` the `Stats` counters,
//! `iter.rs` the iterator types (`Iter` / `IterMut` / `Keys` / `Values` /
//! `Drain`), and `cache.rs` everything else (insert / get / remove / peek /
//! retain / clear / oracle parity / clone / Debug / Extend / borrow lookup /
//! custom hasher / drop counts / SIMD-vs-oracle parity).

mod cache;
mod iter;
mod slot;
mod stats;

/// Reference value used by tests that historically assumed the 8-shard default.
/// The auto-shard policy in `Cache::new` may pick a different count (it depends
/// on `capacity`), but every length / capacity assertion below treats this as
/// "the multiplier we used to size the test cache" rather than as the actual
/// shard count of the cache under test.
const TEST_SHARDS: usize = 8;
