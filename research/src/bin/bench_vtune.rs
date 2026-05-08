//! VTune (Windows) 向けの自己完結型 micro-bench。
//!
//! 目的: `docs/reports/2026-05-08-external-lib-sweep.md` の
//! 「高 HR 帯で senba < orig になる」現象 (Zipf cap=32k, ConCat 1M 帯) を、
//! 外部 trace ファイルや mini-moka / moka などの third-party cache に依存
//! せずに再現する。Zipf 列を事前に Vec<u64> へ展開してから timing 区間に
//! 入るので、ZipfGen のコストは VTune の hot-spot に混入しない。
//!
//! `variant=senba` と `variant=orig` の比較を 1 バイナリで行えるようにし、
//! Windows ネイティブビルドまたは Linux からのクロスビルドの成果物を
//! VTune `hotspots` / `microarchitecture-exploration` で μarch 解析する想定。
//!
//! ## Linux からのクロスビルド (MSVC ABI, VTune の PDB symbol 解決と互換)
//!
//! ```bash
//! cargo install cargo-xwin
//! rustup target add x86_64-pc-windows-msvc
//! # cargo-xwin は cc 経由で clang-cl を呼ぶ。Ubuntu の `clang` パッケージは
//! # clang-cl を含まない (driver mode のみ) ので、~/.cargo/bin に symlink:
//! ln -sf /usr/bin/clang ~/.cargo/bin/clang-cl
//! cargo xwin build --release -p senba-research --bin bench_vtune \
//!     --target x86_64-pc-windows-msvc
//! # 成果物: target/x86_64-pc-windows-msvc/release/bench_vtune.{exe,pdb}
//! ```
//!
//! `senba-research` の `external-traces` feature (zstd) は **default off**。
//! このバイナリは zstd を使わないので、クロスビルド時に Windows 用 C
//! toolchain を準備しなくて済む (clang-cl は msvcrt の include path を辿る
//! だけで C ソースのコンパイルは発生しない)。
//!
//! 使い方:
//!   bench_vtune --variant senba --cap 1048576 --keys 4000000 \
//!       --skew 0.9 --warmup 5000000 --ops 20000000 --seed 42
//!
//!   bench_vtune --variant orig  --cap 1048576 --keys 4000000 \
//!       --skew 0.9 --warmup 5000000 --ops 20000000 --seed 42
//!
//! ## VTune ワークフロー (ITT API による自動 pause/resume)
//!
//! バイナリが `ittapi::pause()` / `ittapi::resume()` を呼ぶので、VTune の
//! collection 範囲は **measurement loop の開始/終了に正確に張り付く**。
//! warmup と Zipf trace 生成、プロセス起動 / 終了は全部 collection 範囲外。
//! 人間タイミングの揺れは無くなる。
//!
//! 推奨起動 (CLI):
//!   vtune -collect uarch-exploration -start-paused -- \
//!       bench_vtune.exe --variant senba --cap 1048576 ...
//!
//! `-start-paused` を付けると、最初の `ittapi::resume()` が呼ばれるまで
//! データ収集自体が停止しているので、サンプル数の無駄が無い。GUI から
//! 起動する場合は Configure Analysis の "Start Paused" を ON にする。
//! VTune が attach されていない場合は ITT call は no-op になるので、
//! 通常の `cargo run` でも安全に動く。

use std::time::Instant;

use senba::Cache as Senba;
use senba_research::experimental::sieve_orig::SieveCache as Orig;
use senba_research::workload::zipf::ZipfGen;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    Senba,
    Orig,
}

impl Variant {
    fn parse(s: &str) -> Self {
        match s {
            "senba" => Variant::Senba,
            "orig" => Variant::Orig,
            other => panic!("--variant must be senba|orig, got: {other}"),
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Variant::Senba => "senba",
            Variant::Orig => "orig",
        }
    }
}

struct Args {
    variant: Variant,
    cap: usize,
    keys: u64,
    skew: f64,
    warmup: usize,
    ops: usize,
    seed: u64,
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut variant = Variant::Senba;
    let mut cap: usize = 1 << 20; // 1M
    let mut keys: u64 = 4_000_000;
    let mut skew: f64 = 0.9;
    let mut warmup: usize = 5_000_000;
    let mut ops: usize = 20_000_000;
    let mut seed: u64 = 42;

    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let mut val = || {
            it.next()
                .unwrap_or_else(|| panic!("expected value after {flag}"))
        };
        match flag.as_str() {
            "--variant" => variant = Variant::parse(&val().clone()),
            "--cap" => cap = val().parse().expect("--cap usize"),
            "--keys" => keys = val().parse().expect("--keys u64"),
            "--skew" => skew = val().parse().expect("--skew f64"),
            "--warmup" => warmup = val().parse().expect("--warmup usize"),
            "--ops" => ops = val().parse().expect("--ops usize"),
            "--seed" => seed = val().parse().expect("--seed u64"),
            "-h" | "--help" => {
                eprintln!(
                    "usage: bench_vtune --variant {{senba,orig}} \
                     --cap N --keys N --skew F --warmup N --ops N --seed N\n\
                     defaults: variant=senba cap=1048576 keys=4000000 skew=0.9 \
                     warmup=5000000 ops=20000000 seed=42\n\
                     ITT API drives VTune collection: warmup runs paused, \
                     measurement loop runs resumed."
                );
                std::process::exit(0);
            }
            other => panic!("unknown flag: {other}"),
        }
    }

    assert!(cap > 0, "--cap > 0");
    assert!(keys > 0, "--keys > 0");
    assert!(skew > 0.0 && skew.is_finite(), "--skew finite > 0");
    assert!(ops > 0, "--ops > 0");

    Args {
        variant,
        cap,
        keys,
        skew,
        warmup,
        ops,
        seed,
    }
}

fn build_trace(skew: f64, n_keys: u64, len: usize, seed: u64) -> Vec<u64> {
    let mut g = ZipfGen::new(skew, n_keys, seed);
    let mut v = Vec::with_capacity(len);
    for _ in 0..len {
        v.push(g.next().unwrap());
    }
    v
}

struct Stats {
    elapsed_ns: u128,
    hits: u64,
    misses: u64,
}

trait MicroCache {
    fn new(cap: usize) -> Self;
    fn get_or_insert(&mut self, k: u64) -> bool;
}

struct SenbaWrap(Senba<u64, u64>);
impl MicroCache for SenbaWrap {
    fn new(cap: usize) -> Self {
        SenbaWrap(Senba::<u64, u64>::new(cap))
    }
    #[inline]
    fn get_or_insert(&mut self, k: u64) -> bool {
        if self.0.get(&k).is_some() {
            true
        } else {
            self.0.insert(k, k);
            false
        }
    }
}

struct OrigWrap(Orig<u64, u64>);
impl MicroCache for OrigWrap {
    fn new(cap: usize) -> Self {
        OrigWrap(Orig::<u64, u64>::new(cap))
    }
    #[inline]
    fn get_or_insert(&mut self, k: u64) -> bool {
        if self.0.get(&k).is_some() {
            true
        } else {
            self.0.insert(k, k);
            false
        }
    }
}

fn run<C: MicroCache>(args: &Args, warmup: &[u64], trace: &[u64]) -> Stats {
    // VTune が attach 済みでも、ここから resume するまで collection は止まる。
    // attach されていなければ no-op (no panic, no spurious output)。
    ittapi::pause();

    let mut c = C::new(args.cap);

    // warmup: 計測対象外。steady state に近づける。
    for &k in warmup {
        c.get_or_insert(k);
    }

    eprintln!("[bench_vtune] warmup done; resuming VTune collection");
    ittapi::resume();

    // measurement window — VTune の hot-spot はこの区間に張り付く。
    let mut hits = 0u64;
    let mut misses = 0u64;
    let t0 = Instant::now();
    for &k in trace {
        if c.get_or_insert(k) {
            hits += 1;
        } else {
            misses += 1;
        }
    }
    let elapsed_ns = t0.elapsed().as_nanos();

    ittapi::pause();
    eprintln!("[bench_vtune] measurement done; pausing VTune collection");

    Stats {
        elapsed_ns,
        hits,
        misses,
    }
}

fn main() {
    let args = parse_args();

    eprintln!(
        "[bench_vtune] variant={} cap={} keys={} skew={} warmup={} ops={} seed={}",
        args.variant.as_str(),
        args.cap,
        args.keys,
        args.skew,
        args.warmup,
        args.ops,
        args.seed,
    );

    // Zipf trace を事前展開: timing 区間に CDF 二分探索 / RNG が入らないように。
    eprintln!(
        "[bench_vtune] generating warmup trace ({} ops)...",
        args.warmup
    );
    let warmup = build_trace(args.skew, args.keys, args.warmup, args.seed);
    eprintln!(
        "[bench_vtune] generating measurement trace ({} ops)...",
        args.ops
    );
    let trace = build_trace(args.skew, args.keys, args.ops, args.seed ^ 0xDEAD_BEEF);

    let s = match args.variant {
        Variant::Senba => run::<SenbaWrap>(&args, &warmup, &trace),
        Variant::Orig => run::<OrigWrap>(&args, &warmup, &trace),
    };

    let total = s.hits + s.misses;
    let hr = s.hits as f64 / total.max(1) as f64;
    let ns_per_op = s.elapsed_ns as f64 / total.max(1) as f64;
    let mops = (total as f64) / (s.elapsed_ns as f64 / 1e3);

    println!("variant,cap,keys,skew,warmup,ops,elapsed_ns,hits,misses,hit_ratio,ns_per_op,mops");
    println!(
        "{},{},{},{},{},{},{},{},{},{:.6},{:.3},{:.3}",
        args.variant.as_str(),
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
