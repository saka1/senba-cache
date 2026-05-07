//! 並列 Zipf bench harness。`sieve_c8` と modern W-TinyLFU 系 (moka 0.12,
//! mini-moka 0.10) を同じ harness で叩き、並列特性 (aggregate Mops / thread CV /
//! tail latency) を比較する。
//!
//! `bench.rs` (single-thread, HR oracle 用 trace driver) とは独立。ハーネスは
//! `std::thread::scope` + `std::sync::Barrier` で自作 (bustle 等の外部 framework
//! は使わない)。CSV を stdout に吐く。
//!
//! 例:
//!   cargo run --release --bin bench_concurrent -- \
//!     --variant c8,moka,mini_moka \
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
//!
//! ## moka / mini-moka adapter の方針
//! `bench.rs` の adapter は HR を oracle として測るため毎 op 後に `sync()` /
//! `run_pending_tasks()` を呼んでいるが、**この concurrent harness では呼ばない**。
//! 理由:
//! - 毎 op flush は read/write log の amortize 設計を潰し、内部 Mutex を踏ませる
//!   ため、moka/mini-moka の「並列特性」を測ったことにならない。real-world で
//!   per-op 同期する利用法は存在しない。
//! - 結果として HR は本来より少し薄まる (admission 判定の遅延ぶん) が、これは
//!   並列利用そのままの挙動。HR oracle が欲しければ bench.rs 側を見る。
//!
//! 計測終了直前に一度だけ `run_pending_tasks` / `sync` を呼ぶこともしない。
//! (= 計測 window 外で flush しても意味がないし、window 中の挙動こそが評価対象)

use std::sync::Arc;
use std::sync::Barrier;
use std::time::Instant;

use senba_research::experimental::sieve_c8::ConcurrentSieveCache;
use senba_research::experimental::sieve_c9::ConcurrentSieveCache as ConcurrentSieveC9;
use senba_research::workload::zipf::ZipfGen;

/// per-op Instant を取らずに chunk 平均を取る単位。
/// 大きすぎると tail latency が見えず、小さすぎると Instant overhead が支配する。
/// 1024 は Caffeine の bench (chunk_size=1k) を踏襲。
const CHUNK_OPS: usize = 1024;

/// 3 variant を同じ driver で叩くための最小 trait。
/// - `&self` のみ (どの実装も interior mutability を持つ)。
/// - `get` は hit/miss だけ返す (値は bench で使わない、clone コスト分が乗るのは
///   どの variant も同条件)。
trait ConcCache: Send + Sync + 'static {
    fn build(capacity: usize, shards: usize) -> Arc<Self>;
    fn get_hit(&self, key: &u64) -> bool;
    fn insert(&self, key: u64, value: u64);
}

impl<const S: usize> ConcCache for ConcurrentSieveCache<u64, u64, S> {
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveCache::new(capacity))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveCache::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: u64) {
        let _ = ConcurrentSieveCache::insert(self, key, value);
    }
}

impl ConcCache for ConcurrentSieveC9<u64, u64> {
    fn build(capacity: usize, shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveC9::with_shards(capacity, shards))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveC9::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: u64) {
        let _ = ConcurrentSieveC9::insert(self, key, value);
    }
}

impl ConcCache for moka::sync::Cache<u64, u64> {
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        // moka の Cache 自体が内部で Arc を持っているため Arc<Cache> は二重 Arc
        // になるが、harness を generic に保つために統一する。clone はどちらにせよ
        // cheap (bench loop の hot path には居ない)。
        Arc::new(moka::sync::Cache::new(capacity as u64))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        self.get(key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: u64) {
        moka::sync::Cache::insert(self, key, value);
    }
}

impl ConcCache for mini_moka::sync::Cache<u64, u64> {
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(mini_moka::sync::Cache::new(capacity as u64))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        self.get(key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: u64) {
        mini_moka::sync::Cache::insert(self, key, value);
    }
}

/// 操作ミックス。
/// - `Gim`: 既存の get-if-miss-insert (= miss なら insert)。元の bench_concurrent と同じ。
/// - `ReadHeavy`: 95% get / 5% insert。get と insert は別 Zipf draw を使う
///   (insert 側は seed をずらして cache の hot key 集合を直接押し込まない)。
#[derive(Clone, Copy, PartialEq, Eq)]
enum OpMix {
    Gim,
    ReadHeavy,
}

impl OpMix {
    fn as_str(self) -> &'static str {
        match self {
            OpMix::Gim => "gim",
            OpMix::ReadHeavy => "read-heavy",
        }
    }
}

struct Args {
    cap: usize,
    threads: usize,
    skew: f64,
    keys: u64,
    ops: usize,
    warmup: usize,
    trials: usize,
    seed: u64,
    variants: Vec<String>,
    /// c8 の SHARDS (const generic)。runtime 値を const に dispatch する match で使う。
    /// c9 では `with_shards` 引数として直接渡す。
    /// moka / mini-moka には影響しない (内部 shard を独自管理)。
    shards: usize,
    op_mix: OpMix,
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
    let mut variants: Vec<String> = vec!["c8".into()];
    let mut shards: usize = 8;
    let mut op_mix = OpMix::Gim;

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
            "--variant" | "--variants" => {
                variants = val().split(',').map(|s| s.trim().to_string()).collect();
            }
            "--shards" => shards = val().parse().expect("--shards is usize"),
            "--op-mix" => {
                let v = val();
                op_mix = match v.as_str() {
                    "gim" => OpMix::Gim,
                    "read-heavy" => OpMix::ReadHeavy,
                    other => panic!("--op-mix must be gim|read-heavy, got: {other}"),
                };
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: bench_concurrent [--variant c8,c9,moka,mini_moka] \
                     [--shards N] [--op-mix gim|read-heavy] --cap N --threads N \
                     --skew F --keys N --ops N --warmup N --trials N --seed N"
                );
                std::process::exit(0);
            }
            other => panic!("unknown flag: {other}"),
        }
    }

    assert!(
        threads > 0 && threads.is_power_of_two(),
        "--threads must be power of two"
    );
    assert!(
        ops.is_multiple_of(threads),
        "--ops must be divisible by --threads"
    );
    assert!(
        warmup.is_multiple_of(threads),
        "--warmup must be divisible by --threads"
    );
    assert!(
        shards.is_power_of_two() && (8..=512).contains(&shards),
        "--shards must be a power of two in [8, 512]"
    );
    for v in &variants {
        assert!(
            matches!(v.as_str(), "c8" | "c9" | "moka" | "mini_moka"),
            "unknown variant: {v} (expected c8|c9|moka|mini_moka)"
        );
    }

    Args {
        cap,
        threads,
        skew,
        keys,
        ops,
        warmup,
        trials,
        seed,
        variants,
        shards,
        op_mix,
    }
}

struct ThreadResult {
    elapsed_ns: u128,
    hits: u64,
    chunk_means_ns: Vec<f64>,
}

/// `read-heavy` mode で「op が insert か」を判定する。Zipf draw を 1 回追加で
/// 流すよりも、index 単位の単純な mod 判定 (= 5% insert) が overhead が小さい。
const READ_HEAVY_INSERT_EVERY: usize = 20;

fn run_trial<C: ConcCache>(args: &Args) -> TrialResult {
    let cache = C::build(args.cap, args.shards);
    // +1 で main thread も barrier に並ぶ (warmup 完了 → measurement 開始の
    // 全 thread 同時スタートを成立させる)。
    let barrier = Arc::new(Barrier::new(args.threads + 1));
    let warmup_per_thread = args.warmup / args.threads;
    let ops_per_thread = args.ops / args.threads;
    let op_mix = args.op_mix;

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
                // read-heavy では insert 側を別 seed の Zipf で draw する
                // (cache を「自分が今 read している hot key 集合そのもの」で汚染しないため)。
                // 0xC0FFEE_DEAD_BEEF は単に違う seed を選ぶための定数。
                let mut zipf_ins = ZipfGen::new(skew, keys, seed ^ 0x00C0_FFEE_DEAD_BEEF_u64);
                // warmup: 並列に warm 状態を作る。直列 prefill より steady state に近い。
                // 計測 mode (gim / read-heavy) に依らず GIM で warm する: cache を
                // hot key で埋める段階は read-heavy でも必要。
                for _ in 0..warmup_per_thread {
                    let k = zipf.next().unwrap();
                    if !cache.get_hit(&k) {
                        cache.insert(k, k);
                    }
                }
                // 全 thread 同時開始
                barrier.wait();
                let t0 = Instant::now();
                let mut hits = 0u64;
                let mut chunk_means_ns: Vec<f64> =
                    Vec::with_capacity(ops_per_thread / CHUNK_OPS + 1);
                let mut chunk_t0 = t0;
                let mut chunk_count = 0usize;
                for i in 0..ops_per_thread {
                    match op_mix {
                        OpMix::Gim => {
                            let k = zipf.next().unwrap();
                            if cache.get_hit(&k) {
                                hits += 1;
                            } else {
                                cache.insert(k, k);
                            }
                        }
                        OpMix::ReadHeavy => {
                            if i.is_multiple_of(READ_HEAVY_INSERT_EVERY) {
                                let k = zipf_ins.next().unwrap();
                                cache.insert(k, k);
                            } else {
                                let k = zipf.next().unwrap();
                                if cache.get_hit(&k) {
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

fn emit(variant: &str, trial: usize, args: &Args, r: &TrialResult) {
    // shards 列は moka/mini-moka では「N/A」相当 (内部 shard を独自管理)。
    // CSV を tidy に保つため、c8/c9 以外は 0 を入れる。集計時は variant でフィルタする想定。
    let shards_col = if matches!(variant, "c8" | "c9") {
        args.shards
    } else {
        0
    };
    println!(
        "{},{},{},{},{},{},{},{},{},{},{:.4},{:.4},{:.2},{:.2},{:.4}",
        variant,
        trial,
        args.op_mix.as_str(),
        args.skew,
        args.keys,
        args.threads,
        args.cap,
        shards_col,
        args.ops,
        r.total_elapsed_ns,
        r.aggregate_mops,
        r.hit_ratio,
        r.p50_chunk_ns,
        r.p99_chunk_ns,
        r.thread_throughput_cv,
    );
    eprintln!(
        "  [{}] trial {} per-thread Mops: [{}]",
        variant,
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
        "variant,trial,op_mix,skew,keys,threads,cap,shards,ops,total_elapsed_ns,aggregate_mops,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv"
    );
    for variant in &args.variants {
        for trial in 0..args.trials {
            let r = match variant.as_str() {
                "c8" => run_c8(&args),
                "c9" => run_trial::<ConcurrentSieveC9<u64, u64>>(&args),
                "moka" => run_trial::<moka::sync::Cache<u64, u64>>(&args),
                "mini_moka" => run_trial::<mini_moka::sync::Cache<u64, u64>>(&args),
                other => panic!("unknown variant: {other}"),
            };
            emit(variant, trial, &args, &r);
        }
    }
}

/// 実行時 `--shards` 値を c8 の const generic に dispatch する。
/// const generic は実行時値を直接渡せないため、サポートする値ごとに明示分岐する。
/// 範囲は parse_args の assert に合わせて 8..=512 (power of two)。
fn run_c8(args: &Args) -> TrialResult {
    match args.shards {
        8 => run_trial::<ConcurrentSieveCache<u64, u64, 8>>(args),
        16 => run_trial::<ConcurrentSieveCache<u64, u64, 16>>(args),
        32 => run_trial::<ConcurrentSieveCache<u64, u64, 32>>(args),
        64 => run_trial::<ConcurrentSieveCache<u64, u64, 64>>(args),
        128 => run_trial::<ConcurrentSieveCache<u64, u64, 128>>(args),
        256 => run_trial::<ConcurrentSieveCache<u64, u64, 256>>(args),
        512 => run_trial::<ConcurrentSieveCache<u64, u64, 512>>(args),
        n => panic!("c8 shards={n} not in supported set (assert above should have caught this)"),
    }
}
