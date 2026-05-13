//! Thread-safe SIEVE cache surface.
//!
//! Behind the `concurrent` Cargo feature, this module exposes [`Cache`] —
//! the sharded, lock-free-reader SIEVE cache promoted from the `c17s`
//! research variant. AVX2 SIMD tag scan + entry-version seqlock on the
//! reader side, per-shard `parking_lot::Mutex` only for Path B/C
//! writers, `Arc<V>` + `crossbeam-epoch` deferred drops so `V: Clone +
//! Send + Sync + 'static` is sound (matching `moka::sync::Cache<K, V>`).
//!
//! Available on `x86_64 + AVX2` only; the module compiles out on other
//! targets. The auto-shard heuristic is `next_pow2(cap/8)` clamped by
//! `MIN_PER_SHARD = 4` and `MAX_PER_SHARD = 64`, motivated by the sweep
//! in `docs/reports/2026-05-13-c17s-shard-heuristic.md`.

#[cfg(all(target_arch = "x86_64", not(miri)))]
mod cache;
#[cfg(all(target_arch = "x86_64", not(miri)))]
pub use cache::Cache;
