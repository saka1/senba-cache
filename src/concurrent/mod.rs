//! Thread-safe SIEVE cache surface.
//!
//! Behind the `concurrent` Cargo feature, this module exposes [`Cache`] —
//! the sharded, lock-free-reader SIEVE cache built on the `sieve_r4`
//! research engine. The reader's tag scan uses AVX2+BMI1 SIMD when the
//! host CPU advertises them and a portable scalar fallback otherwise; the
//! choice is resolved once at `Cache::new` and threaded through every
//! shard call as a bool. Per-shard `parking_lot::Mutex` only for Path B/C
//! writers, `ManuallyDrop<K/V>` slots reclaimed via
//! `crossbeam-epoch::defer_unchecked` so `K, V: Send + 'static` (V also
//! `Clone`) is sufficient.
//!
//! Available on all targets except under Miri (the seqlock + epoch
//! reclamation model is its own can of worms there). The auto-shard
//! heuristic is `next_pow2(cap/8)` clamped by `MIN_PER_SHARD = 4` and
//! `MAX_PER_SHARD = 64`, motivated by the sweep in
//! `docs/reports/2026-05-13-c17s-shard-heuristic.md`. See
//! `docs/reports/2026-05-15-r4-vs-c17s.md` for the r4-vs-c17s comparison.

#[cfg(not(miri))]
mod cache;
#[cfg(not(miri))]
pub use cache::Cache;
