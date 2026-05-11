//! VTune (Windows) 向けの並行 SIEVE プロファイリングドライバ。
//!
//! 目的: `bench_vtune.rs` の concurrent 版。senba 系列の並行 variant
//! (c8 / c9 / c14s) を shared-keyspace Zipf で叩き、per-shard state のどの
//! cache line が cross-core HITM (modified line を別コアが snoop hit) を
//! 起こしているかを VTune Memory Access analysis で特定する想定。
//! perf c2c の "Top Hot Cachelines" + "Cacheline Distribution" 相当を
//! Windows 側で取りに行く。
//!
//! # ハーネス構成
//!
//! - `std::thread::scope` + `std::sync::Barrier` (`bench_concurrent.rs` と同型)。
//! - **moka / mini-moka などの第三者 cache は持ち込まない**。プロファイル対象は
//!   senba 系の per-shard state のみで、外部 crate のシンボルが grouping を
//!   汚すと見たい hot line がノイズで埋まる。
//! - Zipf 列は **thread ごとに事前展開して `Vec<u64>` に格納**。measurement
//!   window 中は indexed iteration のみで、`ZipfGen` の CDF 二分探索 / RNG
//!   を VTune の hot-spot に混入させない (= bench_vtune.rs と同じ方針)。
//! - shared keyspace + per-thread seed: 全 thread が同じ Zipf 分布から独立に
//!   draw するので、k=0 周辺の hot key が共通の hot spot となり shard contention
//!   を再現する (= bench_concurrent.rs と同じ。HR oracle ではないので thread
//!   間で trace が独立でも問題ない)。
//!
//! # ITT API による collection bracket
//!
//! `ittapi::pause` / `ittapi::resume` で VTune の collection 範囲を measurement
//! loop に張り付ける。bracket は **main thread から張る** ことで全 thread の
//! warmup を確実に collection 範囲外に落とす:
//!
//! 1. process 起動 → 即 `ittapi::pause()` (== VTune は collection 停止状態)
//! 2. 各 thread を spawn、thread 内で warmup → barrier.wait
//! 3. main thread が barrier.wait に到達する直前で `ittapi::resume()`
//! 4. barrier 開放 → 全 thread が同時に measurement 開始 (collection ON)
//! 5. 全 thread join 後 `ittapi::pause()`
//!
//! ITT は process-wide なので thread-local fence は不要。
//! VTune が attach されていなければ ITT 呼び出しは no-op (panic / spurious 出力なし)。
//!
//! ## `-start-paused` は必須
//!
//! `bench_vtune.rs` と同様、VTune には `-start-paused` を必ず付ける。付けないと
//! 起動 ~ 最初の `ittapi::pause()` が届くまでの数百 ms 〜 数秒、および warmup の
//! 早期サンプル (cache empty で insert 主体) が collection に混じる。
//! 詳細は `bench_vtune.rs` の module docstring 参照。
//!
//! # クロスビルド (Linux → Windows MSVC ABI)
//!
//! ```bash
//! cargo install cargo-xwin
//! rustup target add x86_64-pc-windows-msvc
//! ln -sf /usr/bin/clang ~/.cargo/bin/clang-cl  # clang-cl driver shim
//! cargo xwin build --release -p senba-research \
//!     --bin bench_vtune_concurrent --target x86_64-pc-windows-msvc
//! # 成果物: target/x86_64-pc-windows-msvc/release/bench_vtune_concurrent.{exe,pdb}
//! ```
//!
//! # 使い方
//!
//! ```text
//! bench_vtune_concurrent --variant c14s --threads 4 \
//!     --cap 4096 --keys 100000 --skew 1.0 \
//!     --warmup 400000 --ops 8000000 --seed 42
//! ```
//!
//! # VTune 起動例 (perf c2c 相当: Memory Access + memory objects)
//!
//! ```text
//! vtune -collect memory-access -knob analyze-mem-objects=true \
//!     -knob mem-object-size-min-thres=64 -start-paused -- \
//!     bench_vtune_concurrent.exe --variant c14s --threads 4 \
//!     --cap 4096 --keys 100000 --skew 1.0 \
//!     --warmup 400000 --ops 8000000 --seed 42
//! ```
//!
//! Bottom-up を `Memory Object / Function / Call Stack` で grouping し、
//! `MEM_LOAD_L3_HIT_RETIRED.XSNP_HITM` (cross-core HITM) で sort すると
//! perf c2c の "Top Hot Cachelines" 相当の view が得られる。

use std::sync::Arc;
use std::sync::Barrier;
use std::time::Instant;

use senba_research::experimental::sieve_c8::ConcurrentSieveCache as ConcurrentSieveC8;
use senba_research::experimental::sieve_c9::ConcurrentSieveCache as ConcurrentSieveC9;
use senba_research::experimental::sieve_c14s::ConcurrentSieveCache as ConcurrentSieveC14S;
use senba_research::experimental::sieve_c16s::ConcurrentSieveCache as ConcurrentSieveC16S;
use senba_research::experimental::sieve_c17s::ConcurrentSieveCache as ConcurrentSieveC17S;
use senba_research::experimental::sieve_c18s::ConcurrentSieveCache as ConcurrentSieveC18S;
use senba_research::experimental::sieve_r1::ConcurrentSieveR1;
use senba_research::workload::zipf::ZipfGen;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    C8,
    C9,
    C14s,
    C16s,
    C17s,
    C18s,
    R1 { ways: usize },
}

impl Variant {
    /// `r1` variant は `r1@ways=N` (N = power of two, 1..=SHARDS) で受け取る。
    /// 単に `r1` と書いた場合は `ways=1` (= c17s 等価ルーティング) と解釈する。
    fn parse(s: &str) -> Self {
        match s {
            "c8" => Variant::C8,
            "c9" => Variant::C9,
            "c14s" => Variant::C14s,
            "c16s" => Variant::C16s,
            "c17s" => Variant::C17s,
            "c18s" => Variant::C18s,
            "r1" => Variant::R1 { ways: 1 },
            other if other.starts_with("r1@ways=") => {
                let ways: usize = other["r1@ways=".len()..]
                    .parse()
                    .expect("r1@ways=<N>: N must be a usize");
                assert!(
                    ways.is_power_of_two(),
                    "r1@ways=<N>: N must be a power of two, got {ways}"
                );
                Variant::R1 { ways }
            }
            other => {
                panic!("--variant must be c8|c9|c14s|c16s|c17s|c18s|r1[@ways=N], got: {other}")
            }
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Variant::C8 => "c8",
            Variant::C9 => "c9",
            Variant::C14s => "c14s",
            Variant::C16s => "c16s",
            Variant::C17s => "c17s",
            Variant::C18s => "c18s",
            Variant::R1 { .. } => "r1",
        }
    }
}

struct Args {
    variant: Variant,
    threads: usize,
    cap: usize,
    keys: u64,
    skew: f64,
    warmup: usize,
    ops: usize,
    seed: u64,
    /// c8/c14s の const generic SHARDS、c9 の `with_shards` 引数。
    /// c14s は SHARDS=64 固定 (Phase 1 設計)、c8 は power-of-two ∈ [8, 512]、
    /// c9 は任意 power-of-two。
    shards: usize,
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // 既定値は `bench_concurrent.rs` の運用域に揃える。c8 / c9 / c14s は
    // per-shard ≤ 64 (6-bit ID 上限) なので、SHARDS=64 のとき total cap は
    // 4096 が上限。VTune の sample 数を稼ぐため ops は長めの 8M。
    let mut variant = Variant::C14s;
    let mut threads: usize = 4;
    let mut cap: usize = 4096;
    let mut keys: u64 = 100_000;
    let mut skew: f64 = 1.0;
    let mut warmup: usize = 400_000;
    let mut ops: usize = 8_000_000;
    let mut seed: u64 = 42;
    let mut shards: usize = 64;

    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let mut val = || {
            it.next()
                .unwrap_or_else(|| panic!("expected value after {flag}"))
        };
        match flag.as_str() {
            "--variant" => variant = Variant::parse(&val().clone()),
            "--threads" => threads = val().parse().expect("--threads usize"),
            "--cap" => cap = val().parse().expect("--cap usize"),
            "--keys" => keys = val().parse().expect("--keys u64"),
            "--skew" => skew = val().parse().expect("--skew f64"),
            "--warmup" => warmup = val().parse().expect("--warmup usize"),
            "--ops" => ops = val().parse().expect("--ops usize"),
            "--seed" => seed = val().parse().expect("--seed u64"),
            "--shards" => shards = val().parse().expect("--shards usize"),
            "-h" | "--help" => {
                eprintln!(
                    "usage: bench_vtune_concurrent --variant {{c8,c9,c14s,c16s,c17s,c18s}} \
                     --threads N --cap N --keys N --skew F --warmup N --ops N \
                     --seed N [--shards N]\n\
                     defaults: variant=c14s threads=4 cap=4096 keys=100000 \
                     skew=1.0 warmup=400000 ops=8000000 seed=42 shards=64\n\
                     note: cap is bounded by 64 * shards (6-bit entry ID).\n\
                     ITT API drives VTune collection: warmup runs paused, \
                     measurement loop runs resumed."
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
    assert!(cap > 0, "--cap > 0");
    assert!(keys > 0, "--keys > 0");
    assert!(skew > 0.0 && skew.is_finite(), "--skew finite > 0");
    assert!(ops > 0, "--ops > 0");
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
    if matches!(variant, Variant::C14s) {
        assert_eq!(
            shards, 64,
            "c14s requires --shards 64 (Phase 1 fixed design)"
        );
    }
    if matches!(variant, Variant::C16s) {
        assert_eq!(
            shards, 64,
            "c16s requires --shards 64 (Phase 1 fixed design)"
        );
    }
    if matches!(variant, Variant::C17s) {
        assert_eq!(
            shards, 64,
            "c17s requires --shards 64 (Phase 1 fixed design)"
        );
    }
    if matches!(variant, Variant::C18s) {
        assert_eq!(
            shards, 64,
            "c18s requires --shards 64 (Phase 1 fixed design)"
        );
    }
    if let Variant::R1 { ways } = variant {
        assert_eq!(shards, 64, "r1 requires --shards 64");
        assert!(
            ways <= shards,
            "r1@ways=<N>: ways ({ways}) must be <= shards ({shards})"
        );
    }
    // c8 / c9 / c14s は全部 6-bit entry ID で per-shard ≤ 64。
    // ここで蹴っておかないと cache の constructor まで panic を持ち越す。
    assert!(
        cap <= 64 * shards,
        "--cap ({cap}) must be <= 64 * shards ({shards}) — per-shard cap is bounded by 6-bit ID"
    );

    Args {
        variant,
        threads,
        cap,
        keys,
        skew,
        warmup,
        ops,
        seed,
        shards,
    }
}

/// 並行 variant を同じ driver で叩くための最小 trait (`bench_concurrent.rs` と同型)。
/// `build_with_ways` は r1 等の routing variant 用、default で `build` 互換。
trait ConcCache: Send + Sync + 'static {
    fn build(capacity: usize, shards: usize) -> Arc<Self>;
    fn build_with_ways(capacity: usize, shards: usize, _ways: usize) -> Arc<Self> {
        Self::build(capacity, shards)
    }
    fn get_hit(&self, key: &u64) -> bool;
    fn insert(&self, key: u64, value: u64);
}

impl<const S: usize> ConcCache for ConcurrentSieveC8<u64, u64, S> {
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveC8::new(capacity))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveC8::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: u64) {
        let _ = ConcurrentSieveC8::insert(self, key, value);
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

impl<const S: usize> ConcCache for ConcurrentSieveC14S<u64, u64, S> {
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveC14S::new(capacity))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveC14S::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: u64) {
        let _ = ConcurrentSieveC14S::insert(self, key, value);
    }
}

impl<const S: usize> ConcCache for ConcurrentSieveC16S<u64, u64, S> {
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveC16S::new(capacity))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveC16S::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: u64) {
        let _ = ConcurrentSieveC16S::insert(self, key, value);
    }
}

impl<const S: usize> ConcCache for ConcurrentSieveC17S<u64, u64, S> {
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveC17S::new(capacity))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveC17S::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: u64) {
        let _ = ConcurrentSieveC17S::insert(self, key, value);
    }
}

impl<const S: usize> ConcCache for ConcurrentSieveC18S<u64, u64, S> {
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveC18S::new(capacity))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveC18S::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: u64) {
        let _ = ConcurrentSieveC18S::insert(self, key, value);
    }
}

impl<const S: usize> ConcCache for ConcurrentSieveR1<u64, u64, S> {
    /// fallback (= `ways=1`、c17s 等価 routing)。`Variant::R1` dispatch は
    /// `build_with_ways` 経路を使う。
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveR1::with_ways(capacity, 1))
    }
    fn build_with_ways(capacity: usize, _shards: usize, ways: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveR1::with_ways(capacity, ways))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveR1::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: u64) {
        let _ = ConcurrentSieveR1::insert(self, key, value);
    }
}

struct Stats {
    elapsed_ns: u128,
    hits: u64,
    misses: u64,
}

fn build_trace(skew: f64, n_keys: u64, len: usize, seed: u64) -> Vec<u64> {
    let mut g = ZipfGen::new(skew, n_keys, seed);
    let mut v = Vec::with_capacity(len);
    for _ in 0..len {
        v.push(g.next().unwrap());
    }
    v
}

fn run<C: ConcCache>(args: &Args) -> Stats {
    // Zipf 事前展開: thread ごとに warmup / measurement 列を別 seed で生成。
    // 全 thread 同じ skew / keyspace から draw → hot key 共有 (shard contention 再現)。
    eprintln!(
        "[bench_vtune_concurrent] generating per-thread traces (warmup={}, ops={}, threads={})...",
        args.warmup, args.ops, args.threads,
    );
    let warmup_per_thread = args.warmup / args.threads;
    let ops_per_thread = args.ops / args.threads;
    let mut warmups: Vec<Vec<u64>> = Vec::with_capacity(args.threads);
    let mut traces: Vec<Vec<u64>> = Vec::with_capacity(args.threads);
    for tid in 0..args.threads {
        let seed_w = args.seed ^ (tid as u64);
        let seed_m = (args.seed ^ 0xDEAD_BEEF_DEAD_BEEF_u64) ^ (tid as u64);
        warmups.push(build_trace(args.skew, args.keys, warmup_per_thread, seed_w));
        traces.push(build_trace(args.skew, args.keys, ops_per_thread, seed_m));
    }

    // 起動直後から collection は止めておく (-start-paused と二重防御)。
    ittapi::pause();

    // ways は r1 のときだけ意味を持つ。他 variant は default impl で無視される。
    let ways = match args.variant {
        Variant::R1 { ways } => ways,
        _ => 1,
    };
    let cache = C::build_with_ways(args.cap, args.shards, ways);
    let barrier = Arc::new(Barrier::new(args.threads + 1));

    let results: Vec<(u128, u64, u64)> = std::thread::scope(|s| {
        let mut handles = Vec::new();
        for tid in 0..args.threads {
            let cache = Arc::clone(&cache);
            let barrier = Arc::clone(&barrier);
            // moves: per-thread Vec を hand off (再 alloc / 共有なし)。
            let warmup = std::mem::take(&mut warmups[tid]);
            let trace = std::mem::take(&mut traces[tid]);
            handles.push(s.spawn(move || {
                // warmup: 計測対象外。collection は止まったまま。
                for &k in &warmup {
                    if !cache.get_hit(&k) {
                        cache.insert(k, k);
                    }
                }
                // 全 thread + main 同時開始の barrier。main 側で resume 済み。
                barrier.wait();
                let t0 = Instant::now();
                let mut hits = 0u64;
                let mut misses = 0u64;
                for &k in &trace {
                    if cache.get_hit(&k) {
                        hits += 1;
                    } else {
                        cache.insert(k, k);
                        misses += 1;
                    }
                }
                (t0.elapsed().as_nanos(), hits, misses)
            }));
        }
        // 全 thread が warmup を終えて barrier に積まれるまで待つ間に
        // main も barrier に向かう。barrier 解放の "瞬間" は collection ON
        // にしておきたいので、main は barrier.wait() の直前で resume する。
        eprintln!("[bench_vtune_concurrent] resuming VTune collection (entering measurement)");
        ittapi::resume();
        barrier.wait();
        let r: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        ittapi::pause();
        eprintln!("[bench_vtune_concurrent] paused VTune collection (measurement done)");
        r
    });

    // aggregate: max(thread elapsed) を全体 wall-clock とする (= bench_concurrent と同じ)。
    let max_elapsed_ns = results.iter().map(|(e, _, _)| *e).max().unwrap_or(0);
    let total_hits: u64 = results.iter().map(|(_, h, _)| *h).sum();
    let total_misses: u64 = results.iter().map(|(_, _, m)| *m).sum();

    Stats {
        elapsed_ns: max_elapsed_ns,
        hits: total_hits,
        misses: total_misses,
    }
}

fn main() {
    let args = parse_args();

    let ways = match args.variant {
        Variant::R1 { ways } => ways,
        _ => 1,
    };
    eprintln!(
        "[bench_vtune_concurrent] variant={} ways={} threads={} cap={} keys={} skew={} \
         warmup={} ops={} seed={} shards={}",
        args.variant.as_str(),
        ways,
        args.threads,
        args.cap,
        args.keys,
        args.skew,
        args.warmup,
        args.ops,
        args.seed,
        args.shards,
    );

    let s = match args.variant {
        Variant::C8 => dispatch_c8(&args),
        Variant::C9 => run::<ConcurrentSieveC9<u64, u64>>(&args),
        Variant::C14s => run::<ConcurrentSieveC14S<u64, u64, 64>>(&args),
        Variant::C16s => run::<ConcurrentSieveC16S<u64, u64, 64>>(&args),
        Variant::C17s => run::<ConcurrentSieveC17S<u64, u64, 64>>(&args),
        Variant::C18s => run::<ConcurrentSieveC18S<u64, u64, 64>>(&args),
        Variant::R1 { .. } => run::<ConcurrentSieveR1<u64, u64, 64>>(&args),
    };

    let total = s.hits + s.misses;
    let hr = s.hits as f64 / total.max(1) as f64;
    let ns_per_op = s.elapsed_ns as f64 / total.max(1) as f64;
    let mops = (total as f64) / (s.elapsed_ns as f64 / 1e3);

    println!(
        "variant,ways,threads,shards,cap,keys,skew,warmup,ops,elapsed_ns,hits,misses,hit_ratio,ns_per_op,aggregate_mops"
    );
    println!(
        "{},{},{},{},{},{},{},{},{},{},{},{},{:.6},{:.3},{:.3}",
        args.variant.as_str(),
        ways,
        args.threads,
        args.shards,
        args.cap,
        args.keys,
        args.skew,
        args.warmup,
        args.ops,
        s.elapsed_ns,
        s.hits,
        s.misses,
        hr,
        ns_per_op,
        mops
    );
}

/// 実行時 `--shards` 値を c8 の const generic に dispatch する。
/// `bench_concurrent.rs::run_c8` と同型。
fn dispatch_c8(args: &Args) -> Stats {
    match args.shards {
        8 => run::<ConcurrentSieveC8<u64, u64, 8>>(args),
        16 => run::<ConcurrentSieveC8<u64, u64, 16>>(args),
        32 => run::<ConcurrentSieveC8<u64, u64, 32>>(args),
        64 => run::<ConcurrentSieveC8<u64, u64, 64>>(args),
        128 => run::<ConcurrentSieveC8<u64, u64, 128>>(args),
        256 => run::<ConcurrentSieveC8<u64, u64, 256>>(args),
        512 => run::<ConcurrentSieveC8<u64, u64, 512>>(args),
        n => panic!("c8 shards={n} not in supported set (assert above should have caught this)"),
    }
}
