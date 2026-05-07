//! 単一 shard concurrent SIEVE testbed harness。
//!
//! `bench_concurrent` の構造を踏襲しつつ、c8/c9 の **shard 内側 1 個** だけを
//! N thread で叩く。multi-shard の embarassingly parallel 効果を排除し、
//! 「shard 内の並行スケーリング限界」を直接測る観測装置。
//!
//! 例:
//!   cargo run --release -p senba-research --bin bench_single_shard -- \
//!     --variant c8 --workload zipf --skew 1.0 --threads 4 --cap 4096 \
//!     --keys 100000 --op-mix gim --ops 4000000 --warmup 200000 \
//!     --trials 3 --seed 42
//!
//! ## ワークロード (3 軸)
//! - `zipf`: 既存 Zipf を per-thread 別 seed で draw (`--keys` で universe size)。
//! - `adversarial-hot`: 全 thread が key=0。visited bit ping-pong の理論上限。
//! - `uniform`: thread 別の disjoint range を round-robin。shard 内競合 floor。
//!
//! ## op mix
//! - `read-only`: 100% get
//! - `read-heavy`: 95% get / 5% insert (insert 側は別 Zipf seed)
//! - `gim`: get-if-miss-insert (50/50 想定の miss 率に応じた mix)
//!
//! ## CSV 出力
//! header:
//!   variant,trial,workload,op_mix,skew,keys,threads,cap,ops,
//!   total_elapsed_ns,aggregate_mops,mops_min_per_thread,hit_ratio,
//!   p50_chunk_ns,p99_chunk_ns,thread_throughput_cv

use std::sync::Arc;
use std::sync::Barrier;
use std::time::Instant;

use senba_research::single_shard::SingleShard;
use senba_research::single_shard::adapters::{C8SingleShard, C9SingleShard};
use senba_research::single_shard::workload::{AdversarialHot, UniformDisjoint};
use senba_research::workload::zipf::ZipfGen;

const CHUNK_OPS: usize = 1024;
const READ_HEAVY_INSERT_EVERY: usize = 20;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Workload {
    Zipf,
    AdversarialHot,
    Uniform,
}

impl Workload {
    fn as_str(self) -> &'static str {
        match self {
            Workload::Zipf => "zipf",
            Workload::AdversarialHot => "adversarial-hot",
            Workload::Uniform => "uniform",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpMix {
    ReadOnly,
    ReadHeavy,
    Gim,
}

impl OpMix {
    fn as_str(self) -> &'static str {
        match self {
            OpMix::ReadOnly => "read-only",
            OpMix::ReadHeavy => "read-heavy",
            OpMix::Gim => "gim",
        }
    }
}

struct Args {
    variant: String,
    cap: usize,
    threads: usize,
    workload: Workload,
    skew: f64,
    keys: u64,
    ops: usize,
    warmup: usize,
    trials: usize,
    seed: u64,
    op_mix: OpMix,
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut variant = String::from("c8");
    let mut cap: usize = 64;
    let mut threads: usize = 4;
    let mut workload = Workload::Zipf;
    let mut skew: f64 = 1.0;
    let mut keys: u64 = 100_000;
    let mut ops: usize = 4_000_000;
    let mut warmup: usize = 200_000;
    let mut trials: usize = 1;
    let mut seed: u64 = 42;
    let mut op_mix = OpMix::Gim;

    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let mut val = || {
            it.next()
                .unwrap_or_else(|| panic!("expected value after {flag}"))
        };
        match flag.as_str() {
            "--variant" => variant = val().clone(),
            "--cap" => cap = val().parse().expect("--cap is usize"),
            "--threads" => threads = val().parse().expect("--threads is usize"),
            "--workload" => {
                let v = val();
                workload = match v.as_str() {
                    "zipf" => Workload::Zipf,
                    "adversarial-hot" => Workload::AdversarialHot,
                    "uniform" => Workload::Uniform,
                    other => {
                        panic!("--workload must be zipf|adversarial-hot|uniform, got: {other}")
                    }
                };
            }
            "--skew" => skew = val().parse().expect("--skew is f64"),
            "--keys" => keys = val().parse().expect("--keys is u64"),
            "--ops" => ops = val().parse().expect("--ops is usize"),
            "--warmup" => warmup = val().parse().expect("--warmup is usize"),
            "--trials" => trials = val().parse().expect("--trials is usize"),
            "--seed" => seed = val().parse().expect("--seed is u64"),
            "--op-mix" => {
                let v = val();
                op_mix = match v.as_str() {
                    "read-only" => OpMix::ReadOnly,
                    "read-heavy" => OpMix::ReadHeavy,
                    "gim" => OpMix::Gim,
                    other => panic!("--op-mix must be read-only|read-heavy|gim, got: {other}"),
                };
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: bench_single_shard --variant {{c8,c9}} \
                     --workload {{zipf,adversarial-hot,uniform}} \
                     --op-mix {{read-only,read-heavy,gim}} \
                     --cap N --threads N --skew F --keys N \
                     --ops N --warmup N --trials N --seed N"
                );
                std::process::exit(0);
            }
            other => panic!("unknown flag: {other}"),
        }
    }

    assert!(threads > 0, "--threads must be > 0");
    assert!(
        ops.is_multiple_of(threads),
        "--ops must be divisible by --threads"
    );
    assert!(
        warmup.is_multiple_of(threads),
        "--warmup must be divisible by --threads"
    );
    assert!(
        matches!(variant.as_str(), "c8" | "c9"),
        "--variant must be c8 or c9, got: {variant}"
    );
    assert!(
        cap > 0 && cap <= 64,
        "--cap must be in [1, 64] (c8 6-bit ID limit)"
    );

    Args {
        variant,
        cap,
        threads,
        workload,
        skew,
        keys,
        ops,
        warmup,
        trials,
        seed,
        op_mix,
    }
}

struct ThreadResult {
    elapsed_ns: u128,
    hits: u64,
    chunk_means_ns: Vec<f64>,
}

/// op stream を抽象化。各 thread がローカルに 1 つ持つ。
trait OpStream: Send {
    fn next_get(&mut self) -> u64;
    fn next_insert(&mut self) -> u64;
}

struct ZipfStream {
    get: ZipfGen,
    ins: ZipfGen,
}

impl OpStream for ZipfStream {
    #[inline]
    fn next_get(&mut self) -> u64 {
        self.get.next().unwrap()
    }
    #[inline]
    fn next_insert(&mut self) -> u64 {
        self.ins.next().unwrap()
    }
}

struct AdversarialStream {
    inner: AdversarialHot,
}

impl OpStream for AdversarialStream {
    #[inline]
    fn next_get(&mut self) -> u64 {
        self.inner.next().unwrap()
    }
    #[inline]
    fn next_insert(&mut self) -> u64 {
        // adversarial-hot では insert も同 key=0 に揃える。
        self.inner.next().unwrap()
    }
}

struct UniformStream {
    inner: UniformDisjoint,
}

impl OpStream for UniformStream {
    #[inline]
    fn next_get(&mut self) -> u64 {
        self.inner.next().unwrap()
    }
    #[inline]
    fn next_insert(&mut self) -> u64 {
        self.inner.next().unwrap()
    }
}

fn make_stream(args: &Args, tid: usize) -> Box<dyn OpStream> {
    let seed = args.seed ^ (tid as u64);
    match args.workload {
        Workload::Zipf => Box::new(ZipfStream {
            get: ZipfGen::new(args.skew, args.keys, seed),
            ins: ZipfGen::new(args.skew, args.keys, seed ^ 0x00C0_FFEE_DEAD_BEEF_u64),
        }),
        Workload::AdversarialHot => Box::new(AdversarialStream {
            inner: AdversarialHot,
        }),
        Workload::Uniform => Box::new(UniformStream {
            inner: UniformDisjoint::new(tid as u64, args.threads as u64, args.keys),
        }),
    }
}

fn run_trial<C: SingleShard<u64, u64> + 'static>(args: &Args) -> TrialResult {
    let cache: Arc<C> = Arc::new(C::new(args.cap));
    let barrier = Arc::new(Barrier::new(args.threads + 1));
    let warmup_per_thread = args.warmup / args.threads;
    let ops_per_thread = args.ops / args.threads;
    let op_mix = args.op_mix;

    let results: Vec<ThreadResult> = std::thread::scope(|s| {
        let mut handles = Vec::new();
        for tid in 0..args.threads {
            let cache = Arc::clone(&cache);
            let barrier = Arc::clone(&barrier);
            let mut stream = make_stream(args, tid);
            handles.push(s.spawn(move || {
                // warmup: GIM で cache を steady state に近づける。
                // adversarial-hot や uniform でも warmup は GIM 形で回す
                // (= 1 回は insert を踏ませる) ことで、measurement window は
                // 純 read 系の評価に近くなる。
                for _ in 0..warmup_per_thread {
                    let k = stream.next_get();
                    if cache.read(&k, |_| ()).is_none() {
                        let _ = cache.insert(k, k);
                    }
                }
                barrier.wait();
                let t0 = Instant::now();
                let mut hits = 0u64;
                let mut chunk_means_ns: Vec<f64> =
                    Vec::with_capacity(ops_per_thread / CHUNK_OPS + 1);
                let mut chunk_t0 = t0;
                let mut chunk_count = 0usize;
                for i in 0..ops_per_thread {
                    match op_mix {
                        OpMix::ReadOnly => {
                            let k = stream.next_get();
                            if cache.read(&k, |_| ()).is_some() {
                                hits += 1;
                            }
                        }
                        OpMix::Gim => {
                            let k = stream.next_get();
                            if cache.read(&k, |_| ()).is_some() {
                                hits += 1;
                            } else {
                                let _ = cache.insert(k, k);
                            }
                        }
                        OpMix::ReadHeavy => {
                            if i.is_multiple_of(READ_HEAVY_INSERT_EVERY) {
                                let k = stream.next_insert();
                                let _ = cache.insert(k, k);
                            } else {
                                let k = stream.next_get();
                                if cache.read(&k, |_| ()).is_some() {
                                    hits += 1;
                                }
                            }
                        }
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
        barrier.wait();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    aggregate(&results, args.ops as u64)
}

struct TrialResult {
    aggregate_mops: f64,
    mops_min_per_thread: f64,
    hit_ratio: f64,
    p50_chunk_ns: f64,
    p99_chunk_ns: f64,
    thread_throughput_cv: f64,
    total_elapsed_ns: u128,
    per_thread_mops: Vec<f64>,
}

fn aggregate(results: &[ThreadResult], total_ops: u64) -> TrialResult {
    let max_elapsed_ns = results.iter().map(|r| r.elapsed_ns).max().unwrap_or(0);
    let aggregate_mops = if max_elapsed_ns > 0 {
        (total_ops as f64) / (max_elapsed_ns as f64 / 1e3)
    } else {
        0.0
    };
    let n_threads = results.len() as f64;
    let per_thread_mops: Vec<f64> = results
        .iter()
        .map(|r| {
            let n = total_ops as f64 / n_threads;
            n / (r.elapsed_ns as f64 / 1e3)
        })
        .collect();
    let mops_min_per_thread = per_thread_mops
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min);
    let mops_min_per_thread = if mops_min_per_thread.is_finite() {
        mops_min_per_thread
    } else {
        0.0
    };
    let total_hits: u64 = results.iter().map(|r| r.hits).sum();
    let hit_ratio = total_hits as f64 / total_ops as f64;

    let mut all_chunks: Vec<f64> = results
        .iter()
        .flat_map(|r| r.chunk_means_ns.iter().copied())
        .collect();
    all_chunks.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50_chunk_ns = percentile(&all_chunks, 0.50);
    let p99_chunk_ns = percentile(&all_chunks, 0.99);

    let mean = per_thread_mops.iter().copied().sum::<f64>() / n_threads;
    let var = per_thread_mops
        .iter()
        .map(|x| (x - mean).powi(2))
        .sum::<f64>()
        / n_threads;
    let cv = if mean > 0.0 { var.sqrt() / mean } else { 0.0 };

    TrialResult {
        aggregate_mops,
        mops_min_per_thread,
        hit_ratio,
        p50_chunk_ns,
        p99_chunk_ns,
        thread_throughput_cv: cv,
        total_elapsed_ns: max_elapsed_ns,
        per_thread_mops,
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn emit(args: &Args, trial: usize, r: &TrialResult) {
    println!(
        "{},{},{},{},{},{},{},{},{},{},{:.4},{:.4},{:.4},{:.2},{:.2},{:.4}",
        args.variant,
        trial,
        args.workload.as_str(),
        args.op_mix.as_str(),
        args.skew,
        args.keys,
        args.threads,
        args.cap,
        args.ops,
        r.total_elapsed_ns,
        r.aggregate_mops,
        r.mops_min_per_thread,
        r.hit_ratio,
        r.p50_chunk_ns,
        r.p99_chunk_ns,
        r.thread_throughput_cv,
    );
    eprintln!(
        "  [{}] trial {} per-thread Mops: [{}]",
        args.variant,
        trial,
        r.per_thread_mops
            .iter()
            .map(|m| format!("{:.3}", m))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

fn main() {
    let args = parse_args();

    println!(
        "variant,trial,workload,op_mix,skew,keys,threads,cap,ops,total_elapsed_ns,aggregate_mops,mops_min_per_thread,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv"
    );
    for trial in 0..args.trials {
        let r = match args.variant.as_str() {
            "c8" => run_trial::<C8SingleShard<u64, u64>>(&args),
            "c9" => run_trial::<C9SingleShard<u64, u64>>(&args),
            other => panic!("unknown variant: {other}"),
        };
        emit(&args, trial, &r);
    }
}
