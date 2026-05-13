//! Thread-safe SIEVE cache surface.
//!
//! Behind the `concurrent` Cargo feature, this module exposes [`Cache`] —
//! the sharded, lock-free-reader SIEVE cache built on the `sieve_r4`
//! research engine. AVX2 SIMD tag scan + entry-version seqlock on the
//! reader side (no shared atomic write on a hit), per-shard
//! `parking_lot::Mutex` only for Path B/C writers, `ManuallyDrop<K/V>`
//! slots reclaimed via `crossbeam-epoch::defer_unchecked` so `K, V: Send
//! + 'static` (V also `Clone`) is sufficient.
//!
//! Available on `x86_64 + AVX2` only; the module compiles out on other
//! targets. The auto-shard heuristic is `next_pow2(cap/8)` clamped by
//! `MIN_PER_SHARD = 4` and `MAX_PER_SHARD = 64`, motivated by the sweep
//! in `docs/reports/2026-05-13-c17s-shard-heuristic.md`. See
//! `docs/reports/2026-05-15-r4-vs-c17s.md` for the r4-vs-c17s comparison.

#[cfg(all(target_arch = "x86_64", not(miri)))]
mod cache;
#[cfg(all(target_arch = "x86_64", not(miri)))]
pub use cache::Cache;
