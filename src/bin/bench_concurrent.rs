//! 並列 Zipf bench harness for `sieve_c8`.
//!
//! `bench.rs` (single-thread, multi-variant trace driver) とは独立に作る。
//! ハーネスは `std::thread::scope` + `std::sync::Barrier` で自作 (bustle 等の
//! 外部 framework は使わない)。CSV を stdout に吐く。
//!
//! 例:
//!   cargo run --release --bin bench_concurrent -- \
//!     --cap 4096 --threads 4 --skew 1.0 --keys 100000 \
//!     --ops 4000000 --warmup 200000 --trials 3 --seed 42
//!
//! ## 計測項目
//! - aggregate throughput (Mops/sec) = total_ops / max(thread elapsed)
//! - per-thread throughput (Mops/sec)
//! - hit ratio
//! - p50 / p99 chunk latency (chunk = CHUNK_OPS の per-op 平均、per-op Instant の
//!   measurement overhead を避けるため)
//! - thread throughput CV (= stddev/mean、Mutex 競合の代理指標)
//!
//! ## ワークロード
//! independent Zipf per thread, **共有キー空間**。各 thread は同じ Zipf 分布から
//! 独立に draw する (= shared keyspace + per-thread seed)。これにより hot key
//! (k=0) が全 thread で共通の hot spot となり shard contention の検証になる。

use std::sync::Arc;
use std::sync::Barrier;
use std::time::Instant;

use senba_cache::sieve_c8::ConcurrentSieveCache;
use senba_cache::workload::zipf::ZipfGen;

const SHARDS: usize = 8;
/// per-op Instant を取らずに chunk 平均を取る単位。
/// 大きすぎると tail latency が見えず、小さすぎると Instant overhead が支配する。
/// 1024 は Caffeine の bench (chunk_size=1k) を踏襲。
const CHUNK_OPS: usize = 1024;

struct Args {
    cap: usize,
    threads: usize,
    skew: f64,
    keys: u64,
    ops: usize,
    warmup: usize,
    trials: usize,
    seed: u64,
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut cap: usize = 4096;
    let mut threads: usize = 4;
    let mut skew: f64 = 1.0;
    let mut keys: u64 = 100_000;
    let mut ops: usize = 4_000_000;
    let mut warmup: usize = 200_000;
    let mut trials: usize = 1;
    let mut seed: u64 = 42;

    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let mut val = || {
            it.next()
                .unwrap_or_else(|| panic!("expected value after {flag}"))
        };
        match flag.as_str() {
            "--cap" => cap = val().parse().expect("--cap is usize"),
            "--threads" => threads = val().parse().expect("--threads is usize"),
            "--skew" => skew = val().parse().expect("--skew is f64"),
            "--keys" => keys = val().parse().expect("--keys is u64"),
            "--ops" => ops = val().parse().expect("--ops is usize"),
            "--warmup" => warmup = val().parse().expect("--warmup is usize"),
            "--trials" => trials = val().parse().expect("--trials is usize"),
            "--seed" => seed = val().parse().expect("--seed is u64"),
            "-h" | "--help" => {
                eprintln!(
                    "usage: bench_concurrent --cap N --threads N --skew F --keys N \
                     --ops N --warmup N --trials N --seed N"
                );
                std::process::exit(0);
            }
            other => panic!("unknown flag: {other}"),
        }
    }

    assert!(threads > 0 && threads.is_power_of_two(), "--threads must be power of two");
    assert!(ops % threads == 0, "--ops must be divisible by --threads");
    assert!(warmup % threads == 0, "--warmup must be divisible by --threads");

    Args {
        cap,
        threads,
        skew,
        keys,
        ops,
        warmup,
        trials,
        seed,
    }
}

struct ThreadResult {
    elapsed_ns: u128,
    hits: u64,
    chunk_means_ns: Vec<f64>,
}

fn run_trial(args: &Args) -> TrialResult {
    let cache: Arc<ConcurrentSieveCache<u64, u64, SHARDS>> =
        Arc::new(ConcurrentSieveCache::new(args.cap));
    // +1 で main thread も barrier に並ぶ (warmup 完了 → measurement 開始の
    // 全 thread 同時スタートを成立させる)。
    let barrier = Arc::new(Barrier::new(args.threads + 1));
    let warmup_per_thread = args.warmup / args.threads;
    let ops_per_thread = args.ops / args.threads;

    let results: Vec<ThreadResult> = std::thread::scope(|s| {
        let mut handles = Vec::new();
        for tid in 0..args.threads {
            let cache = Arc::clone(&cache);
            let barrier = Arc::clone(&barrier);
            let seed = args.seed ^ (tid as u64);
            let skew = args.skew;
            let keys = args.keys;
            handles.push(s.spawn(move || {
                // Zipf テーブル構築は measurement 外。
                let mut zipf = ZipfGen::new(skew, keys, seed);
                // warmup: 並列に warm 状態を作る。直列 prefill より steady state に近い。
                for _ in 0..warmup_per_thread {
                    let k = zipf.next().unwrap();
                    if cache.get(&k).is_none() {
                        cache.insert(k, k);
                    }
                }
                // 全 thread 同時開始
                barrier.wait();
                let t0 = Instant::now();
                let mut hits = 0u64;
                let mut chunk_means_ns: Vec<f64> = Vec::with_capacity(ops_per_thread / CHUNK_OPS + 1);
                let mut chunk_t0 = t0;
                let mut chunk_count = 0usize;
                for _ in 0..ops_per_thread {
                    let k = zipf.next().unwrap();
                    if cache.get(&k).is_some() {
                        hits += 1;
                    } else {
                        cache.insert(k, k);
                    }
                    chunk_count += 1;
                    if chunk_count == CHUNK_OPS {
                        let elapsed = chunk_t0.elapsed().as_nanos() as f64;
                        chunk_means_ns.push(elapsed / CHUNK_OPS as f64);
                        chunk_t0 = Instant::now();
                        chunk_count = 0;
                    }
                }
                ThreadResult {
                    elapsed_ns: t0.elapsed().as_nanos(),
                    hits,
                    chunk_means_ns,
                }
            }));
        }
        // main も barrier 待ち (warmup 完了同期)
        barrier.wait();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    aggregate(&results, args.ops as u64)
}

struct TrialResult {
    aggregate_mops: f64,
    per_thread_mops: Vec<f64>,
    hit_ratio: f64,
    p50_chunk_ns: f64,
    p99_chunk_ns: f64,
    thread_throughput_cv: f64,
    total_elapsed_ns: u128,
}

fn aggregate(results: &[ThreadResult], total_ops: u64) -> TrialResult {
    let max_elapsed_ns = results.iter().map(|r| r.elapsed_ns).max().unwrap_or(0);
    let aggregate_mops = if max_elapsed_ns > 0 {
        (total_ops as f64) / (max_elapsed_ns as f64 / 1e3)
    } else {
        0.0
    };
    let per_thread_mops: Vec<f64> = results
        .iter()
        .map(|r| {
            let n = total_ops as f64 / results.len() as f64;
            n / (r.elapsed_ns as f64 / 1e3)
        })
        .collect();
    let total_hits: u64 = results.iter().map(|r| r.hits).sum();
    let hit_ratio = total_hits as f64 / total_ops as f64;

    let mut all_chunks: Vec<f64> = results
        .iter()
        .flat_map(|r| r.chunk_means_ns.iter().copied())
        .collect();
    all_chunks.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50_chunk_ns = percentile(&all_chunks, 0.50);
    let p99_chunk_ns = percentile(&all_chunks, 0.99);

    let mean = per_thread_mops.iter().copied().sum::<f64>() / per_thread_mops.len() as f64;
    let var = per_thread_mops
        .iter()
        .map(|x| (x - mean).powi(2))
        .sum::<f64>()
        / per_thread_mops.len() as f64;
    let cv = if mean > 0.0 { var.sqrt() / mean } else { 0.0 };

    TrialResult {
        aggregate_mops,
        per_thread_mops,
        hit_ratio,
        p50_chunk_ns,
        p99_chunk_ns,
        thread_throughput_cv: cv,
        total_elapsed_ns: max_elapsed_ns,
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn main() {
    let args = parse_args();

    println!(
        "variant,trial,skew,keys,threads,cap,ops,total_elapsed_ns,aggregate_mops,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv"
    );
    for trial in 0..args.trials {
        let r = run_trial(&args);
        println!(
            "c8,{},{},{},{},{},{},{},{:.4},{:.4},{:.2},{:.2},{:.4}",
            trial,
            args.skew,
            args.keys,
            args.threads,
            args.cap,
            args.ops,
            r.total_elapsed_ns,
            r.aggregate_mops,
            r.hit_ratio,
            r.p50_chunk_ns,
            r.p99_chunk_ns,
            r.thread_throughput_cv,
        );
        // per-thread 内訳は stderr に。CSV 解析の邪魔をしない。
        eprintln!(
            "  trial {} per-thread Mops: [{}]",
            trial,
            r.per_thread_mops
                .iter()
                .map(|m| format!("{:.3}", m))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}
