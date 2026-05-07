//! Lifetime counters for a [`Cache`](super::Cache).

/// Lifetime counters for a [`Cache`](super::Cache). Returned by
/// [`Cache::stats`](super::Cache::stats).
///
/// All fields are monotonically increasing across the lifetime of the cache.
/// Counts are aggregated across all shards at call time.
///
/// Semantics:
///
/// - `hits` / `misses` count **promoting** lookups only — `get`, `get_mut`,
///   `get_key_value`, and the lookup half of `get_or_insert_with`. Probes
///   that do not affect SIEVE eviction (`peek*`, `contains_key`) and the
///   internal `find` calls inside `insert` are intentionally excluded so
///   that `hits + misses` equals the number of user-facing lookup ops.
/// - `insertions` counts every successful call to
///   [`Cache::insert`](super::Cache::insert) (both replace and new-entry
///   paths) plus the miss-path insert inside
///   [`Cache::get_or_insert_with`](super::Cache::get_or_insert_with).
/// - `evictions` counts only **capacity-driven** evictions inside `insert`.
///   Explicit removals (`remove`, `clear`, `retain`) do not increment it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub hits: u64,
    pub misses: u64,
    pub insertions: u64,
    pub evictions: u64,
}
