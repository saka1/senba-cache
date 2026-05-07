use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use senba_research::CacheImpl;
use senba_research::experimental::sieve_orig::SieveCache as Orig;
// use senba_research::experimental::sieve_v0::SieveCache as V0;
// use senba_research::experimental::sieve_v1::SieveCache as V1;
// use senba_research::experimental::sieve_v2::SieveCache as V2;
use senba_research::experimental::sieve_j3::SieveCache as J3;
use senba_research::experimental::sieve_j4::SieveCache as J4;
use senba_research::experimental::sieve_j5::SieveCache as J5;
use senba_research::experimental::sieve_v3::SieveCache as V3;
use senba_research::workload::zipf::ZipfGen;
use std::hint::black_box;
use std::time::Duration;

// NSDI'24 SIEVE 論文 §5.3 / §6.1 の synthetic Zipf 実験に寄せた設定。
// 詳細は docs/sieve-paper-workload.md を参照。
//   - α は実 web workload で観測されている範囲 (0.55 - 1.5) を中心にカバー
//   - キャッシュ容量は footprint (= ユニーク object 数) の {0.1%, 1%, 10%}
//   - trace 長は footprint の 10x (paper の 100x には届かないがオーダ的に近づける)
const SKEWS: &[f64] = &[0.6, 0.8, 1.0, 1.2];
const N_KEYS: u64 = 100_000;
const TRACE_LEN: usize = 1_000_000;
const CAP_RATIOS: &[f64] = &[0.001, 0.01, 0.1];
const SEED: u64 = 42;

fn quick_group<'a>(
    c: &'a mut Criterion,
    name: &str,
) -> criterion::BenchmarkGroup<'a, criterion::measurement::WallTime> {
    let mut g = c.benchmark_group(name);
    g.sample_size(20)
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(3));
    g
}

fn insert_only_for<C: CacheImpl<u64, u64>>(
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
    let mut group = quick_group(c, "insert_only");
    let caps: Vec<usize> = CAP_RATIOS
        .iter()
        .map(|r| ((N_KEYS as f64) * r).round() as usize)
        .collect();
    for &skew in SKEWS {
        let trace: Vec<u64> = ZipfGen::new(skew, N_KEYS, SEED).take(TRACE_LEN).collect();
        group.throughput(Throughput::Elements(trace.len() as u64));
        for &cap in &caps {
            insert_only_for::<Orig<u64, u64>>(&mut group, "orig", skew, cap, &trace);
            // insert_only_for::<V0<u64, u64>>(&mut group, "v0", skew, cap, &trace);
            // insert_only_for::<V1<u64, u64>>(&mut group, "v1", skew, cap, &trace);
            // insert_only_for::<V2<u64, u64>>(&mut group, "v2", skew, cap, &trace);
            insert_only_for::<V3<u64, u64>>(&mut group, "v3", skew, cap, &trace);
            insert_only_for::<J3<u64, u64>>(&mut group, "j3", skew, cap, &trace);
            insert_only_for::<J4<u64, u64>>(&mut group, "j4", skew, cap, &trace);
        }
    }
    group.finish();
}

/// Worst-case memory-fairness bench: orig vs j5 at u64,u64 (smallest entries,
/// where j3's 2x order_cap slack is most punishing). Compares:
///
/// - `orig` @ nominal cap (baseline, link 8B/entry, no slack)
/// - `orig_2x` @ 2 * cap (handicap: more live entries AND more bytes than j5)
/// - `j5` @ nominal cap (target, 2x slack via j3 internals)
///
/// If j5 still beats `orig_2x` on ns/op, j5 is faster *despite* being given
/// less memory. If `orig_2x` wins, the 2x slack is paying for the speed.
fn bench_mem_fair(c: &mut Criterion) {
    let mut group = quick_group(c, "mem_fair_u64");
    let caps: Vec<usize> = CAP_RATIOS
        .iter()
        .map(|r| ((N_KEYS as f64) * r).round() as usize)
        .collect();
    for &skew in SKEWS {
        let trace: Vec<u64> = ZipfGen::new(skew, N_KEYS, SEED).take(TRACE_LEN).collect();
        group.throughput(Throughput::Elements(trace.len() as u64));
        for &cap in &caps {
            insert_only_for::<Orig<u64, u64>>(&mut group, "orig", skew, cap, &trace);
            insert_only_for::<Orig<u64, u64>>(&mut group, "orig_2x", skew, cap * 2, &trace);
            insert_only_for::<J5<u64, u64>>(&mut group, "j5", skew, cap, &trace);
        }
    }
    group.finish();
}

criterion_group!(benches, bench_insert_only, bench_mem_fair);
criterion_main!(benches);
