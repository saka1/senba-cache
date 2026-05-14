//! Performance regression bench for `senba::concurrent::Cache` — companion
//! to `sieve_cache_perf.rs` (which gates the single-thread `senba::Cache`).
//! Run with `--save-baseline` / `--baseline` so any non-trivial edit to
//! `src/concurrent/` can be checked for regression before commit.
//!
//! Design constraints (kept narrow on purpose):
//!
//! - **Four scenarios**, fixed seeds and trace lengths, total runtime ~35s.
//! - **Public `senba::concurrent::Cache` API only** — no shard probing.
//! - **Threads dimension chosen to span low / high contention**:
//!     - T=4 — moderate fan-out, mostly Path A hits.
//!     - T=16 — heavy contention on the same hot shards, Path C dominates.
//! - **V dimension chosen to span the dispatch**:
//!     - `V=u64` — Copy path, `epoch::pin` folds away, no clone-on-hit.
//!     - `V=String` — !Copy path, full epoch defer + V::clone on every hit.
//!
//! ## Gate (see CLAUDE.md)
//!
//! Treat **>5% regression on any of the four cells as a commit-blocker**
//! for any edit under `src/concurrent/`. The 5% threshold is the same one
//! `sieve_cache_perf` uses for the single-thread surface.

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use senba::concurrent::Cache;
use senba_research::workload::zipf::ZipfGen;
use std::hint::black_box;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

// Fixed inputs. Changing these invalidates prior baselines.
const SEED: u64 = 0x00C0_FFEE_C0DE;
const N_KEYS: u64 = 5_000;
const TRACE_LEN_PER_THREAD: usize = 100_000;
const CAP: usize = 4096;
const SHARDS: usize = 512;

fn perf_group<'a>(
    c: &'a mut Criterion,
    name: &str,
) -> criterion::BenchmarkGroup<'a, criterion::measurement::WallTime> {
    let mut g = c.benchmark_group(name);
    g.sample_size(30)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(4))
        .noise_threshold(0.02);
    g
}

fn zipf_trace(skew: f64) -> Vec<u64> {
    ZipfGen::new(skew, N_KEYS, SEED)
        .take(TRACE_LEN_PER_THREAD)
        .collect()
}

/// Run `trace` once per thread across `threads` worker threads against the
/// shared `cache`. 50/50 get-or-insert: a miss falls through to insert,
/// mirroring the `bench_concurrent` "gim" op-mix used in the production
/// sweep. Returns wall time once every worker has joined.
fn run_gim<V>(cache: Arc<Cache<u64, V>>, trace: &[u64], threads: usize, mk_v: fn(u64) -> V)
where
    V: Clone + Send + 'static,
{
    let trace = Arc::new(trace.to_vec());
    thread::scope(|s| {
        for tid in 0..threads {
            let cache = Arc::clone(&cache);
            let trace = Arc::clone(&trace);
            s.spawn(move || {
                // Each thread starts at a different offset so the SIMD
                // contention pattern isn't an artificial lockstep.
                let off = (tid * 7919) % trace.len().max(1);
                for i in 0..trace.len() {
                    let k = trace[(off + i) % trace.len()];
                    if cache.get(&k).is_none() {
                        cache.insert(black_box(k), mk_v(k));
                    }
                }
            });
        }
    });
}

/// Scenario 1: Zipf skew=1.0, V=u64, T=4. Low-contention Copy path.
fn bench_zipf1_u64_t4(c: &mut Criterion) {
    let mut g = perf_group(c, "sieve_concurrent_perf/zipf1_u64_t4");
    let trace = zipf_trace(1.0);
    g.throughput(Throughput::Elements((TRACE_LEN_PER_THREAD * 4) as u64));
    g.bench_with_input(BenchmarkId::from_parameter(CAP), &trace, |b, trace| {
        b.iter_batched(
            || Arc::new(Cache::<u64, u64>::with_shards(CAP, SHARDS)),
            |cache| run_gim::<u64>(cache, trace, 4, |k| k),
            BatchSize::LargeInput,
        )
    });
    g.finish();
}

/// Scenario 2: Zipf skew=1.4, V=u64, T=16. High-contention Copy path.
fn bench_zipf14_u64_t16(c: &mut Criterion) {
    let mut g = perf_group(c, "sieve_concurrent_perf/zipf14_u64_t16");
    let trace = zipf_trace(1.4);
    g.throughput(Throughput::Elements((TRACE_LEN_PER_THREAD * 16) as u64));
    g.bench_with_input(BenchmarkId::from_parameter(CAP), &trace, |b, trace| {
        b.iter_batched(
            || Arc::new(Cache::<u64, u64>::with_shards(CAP, SHARDS)),
            |cache| run_gim::<u64>(cache, trace, 16, |k| k),
            BatchSize::LargeInput,
        )
    });
    g.finish();
}

/// Scenario 3: Zipf skew=1.0, V=String(~16B), T=4. Low-contention !Copy path.
fn bench_zipf1_string_t4(c: &mut Criterion) {
    let mut g = perf_group(c, "sieve_concurrent_perf/zipf1_string_t4");
    let trace = zipf_trace(1.0);
    g.throughput(Throughput::Elements((TRACE_LEN_PER_THREAD * 4) as u64));
    g.bench_with_input(BenchmarkId::from_parameter(CAP), &trace, |b, trace| {
        b.iter_batched(
            || Arc::new(Cache::<u64, String>::with_shards(CAP, SHARDS)),
            |cache| run_gim::<String>(cache, trace, 4, |k| format!("v{k:08}")),
            BatchSize::LargeInput,
        )
    });
    g.finish();
}

/// Scenario 4: Zipf skew=1.4, V=String(~16B), T=16. High-contention !Copy
/// path — the cell r4 was designed to beat the prior c17s-lift on.
fn bench_zipf14_string_t16(c: &mut Criterion) {
    let mut g = perf_group(c, "sieve_concurrent_perf/zipf14_string_t16");
    let trace = zipf_trace(1.4);
    g.throughput(Throughput::Elements((TRACE_LEN_PER_THREAD * 16) as u64));
    g.bench_with_input(BenchmarkId::from_parameter(CAP), &trace, |b, trace| {
        b.iter_batched(
            || Arc::new(Cache::<u64, String>::with_shards(CAP, SHARDS)),
            |cache| run_gim::<String>(cache, trace, 16, |k| format!("v{k:08}")),
            BatchSize::LargeInput,
        )
    });
    g.finish();
}

criterion_group!(
    perf,
    bench_zipf1_u64_t4,
    bench_zipf14_u64_t16,
    bench_zipf1_string_t4,
    bench_zipf14_string_t16,
);
criterion_main!(perf);
