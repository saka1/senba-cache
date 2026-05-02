use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rand::SeedableRng;
use rand::rngs::StdRng;
use std::hint::black_box;
use senba_cache::Cache;
use senba_cache::sieve_orig::SieveCache as Orig;
use senba_cache::sieve_v0::SieveCache as V0;
use senba_cache::workload::zipf::ZipfGen;

const SKEWS: &[f64] = &[1.05, 1.1, 1.2];
const CAPS: &[usize] = &[1024, 8192, 65536];
const ZIPF_KEYS: u64 = 100_000;
const TRACE_LEN: usize = 200_000;
const SEED: u64 = 42;

fn insert_only_for<C: Cache<u64, u64>>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    label: &str,
    skew: f64,
    cap: usize,
    trace: &[u64],
) {
    group.bench_with_input(
        BenchmarkId::new(format!("{label}/skew{skew}"), cap),
        &(cap, trace),
        |b, (cap, trace)| {
            b.iter_batched(
                || C::new(*cap),
                |mut c| {
                    for &k in *trace {
                        c.insert(black_box(k), k);
                    }
                },
                BatchSize::LargeInput,
            )
        },
    );
}

fn bench_insert_only(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_only");
    for &skew in SKEWS {
        for &cap in CAPS {
            let trace: Vec<u64> = ZipfGen::new(skew, ZIPF_KEYS, SEED).take(TRACE_LEN).collect();
            group.throughput(Throughput::Elements(trace.len() as u64));
            insert_only_for::<Orig<u64, u64>>(&mut group, "orig", skew, cap, &trace);
            insert_only_for::<V0<u64, u64>>(&mut group, "v0", skew, cap, &trace);
        }
    }
    group.finish();
}

#[derive(Clone, Copy)]
enum Op {
    Get(u64),
    Insert(u64),
}

/// 80% get / 20% insert を seed 固定で事前展開。RNG コストを計測対象から除外。
fn make_mixed_ops(skew: f64, n: usize, get_ratio: f64) -> Vec<Op> {
    use rand::RngExt;
    let mut rng = StdRng::seed_from_u64(SEED ^ 0xA5A5);
    ZipfGen::new(skew, ZIPF_KEYS, SEED)
        .take(n)
        .map(|k| {
            if rng.random::<f64>() < get_ratio {
                Op::Get(k)
            } else {
                Op::Insert(k)
            }
        })
        .collect()
}

fn mixed_for<C: Cache<u64, u64>>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    label: &str,
    skew: f64,
    cap: usize,
    ops: &[Op],
) {
    group.bench_with_input(
        BenchmarkId::new(format!("{label}/skew{skew}"), cap),
        &(cap, ops),
        |b, (cap, ops)| {
            b.iter_batched(
                || C::new(*cap),
                |mut c| {
                    for op in *ops {
                        match *op {
                            Op::Get(k) => {
                                let _ = c.get(black_box(&k));
                            }
                            Op::Insert(k) => {
                                let _ = c.insert(black_box(k), k);
                            }
                        }
                    }
                },
                BatchSize::LargeInput,
            )
        },
    );
}

fn bench_mixed(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_80r_20w");
    for &skew in SKEWS {
        for &cap in CAPS {
            let ops = make_mixed_ops(skew, TRACE_LEN, 0.8);
            group.throughput(Throughput::Elements(ops.len() as u64));
            mixed_for::<Orig<u64, u64>>(&mut group, "orig", skew, cap, &ops);
            mixed_for::<V0<u64, u64>>(&mut group, "v0", skew, cap, &ops);
        }
    }
    group.finish();
}

criterion_group!(benches, bench_insert_only, bench_mixed);
criterion_main!(benches);
