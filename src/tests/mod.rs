//! Unit tests for `senba::Cache`. Split by **public-API topic** rather than by
//! internal module — most tests exercise `Cache` end-to-end, and the public
//! surface is the natural axis for a reader looking up "how does retain
//! behave?" or "what does peek_mut do?". Genuinely white-box tests
//! (bit-layout constants, `needle_from_hash` injectivity) live co-located
//! inside `src/shard/` instead.

mod clear;
mod clone_debug;
mod construction;
mod eviction;
mod extend;
mod get_or_insert;
mod insert_get;
mod iter;
mod layout;
mod peek;
mod remove;
mod retain;
mod slot_sizes;
mod stats;

/// Reference value used by tests that historically assumed the 8-shard default.
/// The auto-shard policy in `Cache::new` may pick a different count (it depends
/// on `capacity`), but every length / capacity assertion below treats this as
/// "the multiplier we used to size the test cache" rather than as the actual
/// shard count of the cache under test.
const TEST_SHARDS: usize = 8;
