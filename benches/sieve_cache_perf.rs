//! Performance regression bench for `senba_cache::Cache` (the library-grade SIEVE
//! in `src/sieve_cache.rs`). Quality-gate companion to `cargo test` /
//! `cargo clippy`: run with criterion's `--save-baseline` / `--baseline` so
//! any non-trivial edit to `sieve_cache.rs` (or the modules it depends on)
//! can be checked for regression before commit.
//!
//! Design constraints (intentionally narrow — see `benches/micro.rs` for the
//! experimental playground that gets rewritten freely):
//!
//! - **Small scenario set**, so the whole run fits in ~25–30s.
//! - **Fixed seeds and trace lengths**, so two runs on the same machine are
//!   directly comparable via `--save-baseline` / `--baseline`.
//! - **Public API only** (`Cache<K, V, S>`). No probing into `Inner`,
//!   no module-private tricks. If a refactor breaks the public path, this
//!   bench notices.
//! - **Three code paths covered**:
//!     1. insert-only on `u64,u64,Slot32` — the hot warm-up + eviction loop
//!        on the smallest entry size (where any per-op overhead shows up).
//!     2. mixed get+insert on the same config — exercises the SIMD `find`
//!        path and the visited-bit set.
//!     3. insert-only on `String,String,Slot64` — covers the heavier-entry
//!        path (drop on eviction, larger Storage stride).
//!
//! Usage (also documented in CLAUDE.md):
//!
//! ```bash
//! # before your change
//! cargo bench --bench sieve_cache_perf -- --save-baseline before
//! # after your change
//! cargo bench --bench sieve_cache_perf -- --baseline before
//! ```
//!
//! Criterion will report `Performance has regressed.` / `... has improved.`
//! per scenario. Treat regressions >5% on any scenario as a signal to
//! investigate before merging.

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use senba_cache::workload::zipf::ZipfGen;
use senba_cache::{Cache, Slot32, Slot64};
use std::hint::black_box;
use std::time::Duration;

// Fixed parameters. Do not tune these casually — comparable baselines
// require identical inputs across runs.
//
// Capacity is bounded by `MAX_PER_SHARD = 64` (6-bit id) × SHARDS. With
// SHARDS=8 the hard ceiling is 512; we sit a bit below to leave the
// eviction path well-exercised without crowding any shard at its limit.
const SEED: u64 = 0xC0FFEE;
const N_KEYS: u64 = 5_000;
const TRACE_LEN: usize = 200_000;
const SKEW: f64 = 1.0;
const CAP_U64: usize = 384; // 48 / shard
const CAP_STR: usize = 256; // 32 / shard

fn perf_group<'a>(
    c: &'a mut Criterion,
    name: &str,
) -> criterion::BenchmarkGroup<'a, criterion::measurement::WallTime> {
    let mut g = c.benchmark_group(name);
    // Tuned for low-variance regression checking, not exploration. The numbers
    // below were picked so that running the bench twice back-to-back with no
    // code change reports "within noise threshold" on all three scenarios on a
    // typical WSL2 / x86_64 dev machine. Total runtime ≈ 25–30s.
    //
    // - `sample_size(60)`: 2× the criterion default at this measurement_time,
    //   roughly halving the CI width on the median (sqrt(N) scaling).
    // - `measurement_time(4s)`: each sample averages more iterations, smoothing
    //   per-sample jitter from CPU freq scaling / other processes.
    // - `warm_up_time(1s)`: gives the JIT-free Rust binary time to settle into
    //   a steady I/D-cache + branch-predictor state before timing starts.
    // - `noise_threshold(0.02)`: changes within ±2% are formally classified as
    //   noise. Criterion still prints the delta but won't say "regressed" /
    //   "improved" until the median moves past this floor. The CLAUDE.md gate
    //   (>5% on any scenario = investigate) sits comfortably above it.
    g.sample_size(60)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(4))
        .noise_threshold(0.02);
    g
}

fn zipf_trace() -> Vec<u64> {
    ZipfGen::new(SKEW, N_KEYS, SEED).take(TRACE_LEN).collect()
}

/// Scenario 1: insert-only on `Cache<u64, u64, Slot32, 8>`.
fn bench_insert_u64(c: &mut Criterion) {
    let mut g = perf_group(c, "sieve_cache_perf/insert_u64");
    let trace = zipf_trace();
    g.throughput(Throughput::Elements(trace.len() as u64));
    g.bench_with_input(BenchmarkId::from_parameter(CAP_U64), &trace, |b, trace| {
        b.iter_batched(
            || Cache::<u64, u64, Slot32>::with_shards(CAP_U64, 8),
            |mut c| {
                for &k in trace {
                    c.insert(black_box(k), k);
                }
            },
            BatchSize::LargeInput,
        )
    });
    g.finish();
}

/// Scenario 2: mixed 50% get / 50% insert on `Cache<u64, u64, Slot32, 8>`.
/// The cache is pre-warmed in the setup closure so `get` actually hits the
/// SIMD `find` path (and the visited-bit set) in steady state.
fn bench_mixed_u64(c: &mut Criterion) {
    let mut g = perf_group(c, "sieve_cache_perf/mixed_u64");
    let trace = zipf_trace();
    g.throughput(Throughput::Elements(trace.len() as u64));
    g.bench_with_input(BenchmarkId::from_parameter(CAP_U64), &trace, |b, trace| {
        b.iter_batched(
            || {
                let mut c = Cache::<u64, u64, Slot32>::with_shards(CAP_U64, 8);
                // Warm-up so get() has a realistic hit ratio for Zipf 1.0.
                for &k in trace.iter().take(CAP_U64 * 2) {
                    c.insert(k, k);
                }
                c
            },
            |mut c| {
                for (i, &k) in trace.iter().enumerate() {
                    if i & 1 == 0 {
                        black_box(c.get(&k));
                    } else {
                        c.insert(black_box(k), k);
                    }
                }
            },
            BatchSize::LargeInput,
        )
    });
    g.finish();
}

/// Scenario 3: insert-only on `Cache<String, String, Slot64, 8>`.
/// Exercises the heavier-entry path: drop-on-eviction and the wider stride.
fn bench_insert_string(c: &mut Criterion) {
    let mut g = perf_group(c, "sieve_cache_perf/insert_string");
    // Reuse the integer trace, formatted into short strings deterministically.
    // Done once outside iter so we measure cache work, not formatting.
    let int_trace = zipf_trace();
    let trace: Vec<(String, String)> = int_trace
        .iter()
        .map(|k| (format!("k{k:08}"), format!("v{k:08}")))
        .collect();
    g.throughput(Throughput::Elements(trace.len() as u64));
    g.bench_with_input(BenchmarkId::from_parameter(CAP_STR), &trace, |b, trace| {
        b.iter_batched(
            || Cache::<String, String, Slot64>::with_shards(CAP_STR, 8),
            |mut c| {
                for (k, v) in trace {
                    c.insert(black_box(k.clone()), v.clone());
                }
            },
            BatchSize::LargeInput,
        )
    });
    g.finish();
}

criterion_group!(perf, bench_insert_u64, bench_mixed_u64, bench_insert_string);
criterion_main!(perf);
