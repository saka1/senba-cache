//! 1 トレースを各 variant に流して CSV を stdout に吐く CLI。
//!
//! 例:
//!   cargo run --release --bin bench -- \
//!     --source zipf --skew 1.1 --keys 100000 --len 1000000 --seed 42 \
//!     --capacity 1024,4096,16384 --variant orig,v0
//!
//!   cargo run --release --bin bench -- \
//!     --source file --path external/NSDI24-SIEVE/mydata/zipf/zipf_1.0 \
//!     --capacity 4096 --variant orig,v0
//!
//!   cargo run --release --bin bench -- \
//!     --source twitter --path external/twitter-cache-trace/cluster018 \
//!     --capacity 4096 --variant orig,j5_n32

use std::time::Instant;

use senba::CacheImpl;
// 2026-05-05: ベースラインを orig vs j7 に絞り込み。過去 variant は
// 必要になれば use と matcher を復活させる (テスト・実装は残置)。
// use senba::experimental::sieve_j3::SieveCache as J3;
// use senba::experimental::sieve_j4::SieveCache as J4;
// use senba::experimental::sieve_j5::SieveCache as J5;
// use senba::experimental::sieve_j6::SieveCache as J6;
use senba::Cache as Senba;
use senba::experimental::sieve_j7::SieveCache as J7;
use senba::experimental::sieve_j8::SieveCache as J8;
use senba::experimental::sieve_orig::SieveCache as Orig;

/// W-TinyLFU 比較用に `mini_moka::sync::Cache<u64,u64>` を `Cache<u64,u64>` に被せる
/// thin wrapper。bench でのみ使うので bench.rs 内に閉じる。
///
/// **重要**: mini-moka は read/write log を内部 buffer にためて amortize するため、
/// 明示的に `ConcurrentCacheExt::sync()` を呼ばないと CMSketch 更新と admission 決定が
/// 反映されない。upstream の test code (sync/cache.rs `basic_single_thread`) が毎回
/// sync() を呼んでいることから、決定的な HR を測るには get/insert 後に sync() が必須。
/// 呼ばないと admission 判定が遅延し、新規 key が write buffer overflow で落ちて
/// HR が崩壊する。本 adapter では HR の正しさを優先し、毎 op 後に sync() を呼ぶ。
/// その分 ns/op は実態より悪化するが、HR が screening の gate なので許容する。
///
/// 制約:
/// - mini_moka の `insert` は `()` を返すため、追い出された (K,V) は取れない。
///   `Cache::insert` は常に `None` を返す → CSV の evictions 列は 0 固定で**意味が無い**。
///   HR と ns/op だけ参照すること。
/// - `get` は `Option<V>` (clone) を返す。trait は `Option<&V>` 要求。bench の `drive`
///   は `.is_some()` しか見ないので、ヒット時はダミー静的参照を返して整合させる。
/// - `max_capacity` は重み合計の budget。default weighter は entry あたり 1 なので
///   おおむね entry 数 == capacity と見做せる。
struct MiniMoka {
    inner: mini_moka::sync::Cache<u64, u64>,
    cap: u64,
}

const MINI_MOKA_DUMMY: u64 = 0;

impl senba::CacheImpl<u64, u64> for MiniMoka {
    fn new(capacity: usize) -> Self {
        Self {
            inner: mini_moka::sync::Cache::new(capacity as u64),
            cap: capacity as u64,
        }
    }
    fn capacity(&self) -> usize {
        self.cap as usize
    }
    fn len(&self) -> usize {
        use mini_moka::sync::ConcurrentCacheExt;
        self.inner.sync();
        self.inner.entry_count() as usize
    }
    fn get(&mut self, key: &u64) -> Option<&u64> {
        use mini_moka::sync::ConcurrentCacheExt;
        let hit = self.inner.get(key).is_some();
        self.inner.sync();
        if hit { Some(&MINI_MOKA_DUMMY) } else { None }
    }
    fn insert(&mut self, key: u64, value: u64) -> Option<(u64, u64)> {
        use mini_moka::sync::ConcurrentCacheExt;
        self.inner.insert(key, value);
        self.inner.sync();
        None
    }
    fn contains_key(&self, key: &u64) -> bool {
        self.inner.contains_key(key)
    }
}

/// `moka 0.12::sync::Cache<u64,u64>` を `Cache<u64,u64>` に被せる thin wrapper。
/// mini_moka との違いは:
/// - moka 0.12+ は adaptive window sizing (Caffeine の hill-climbing 由来) が入っており、
///   scan-heavy では window を広げて mini-moka 0.10 の HR 崩壊を回避できるはず。
/// - flush API は `run_pending_tasks(&self)` (mini_moka の `sync()` 相当)。
/// - `get` は `Option<V>` (clone) を返すのは同じ。
/// - `insert` は `()` を返すので CSV evictions は 0 固定。
struct Moka {
    inner: moka::sync::Cache<u64, u64>,
    cap: u64,
}

const MOKA_DUMMY: u64 = 0;

impl senba::CacheImpl<u64, u64> for Moka {
    fn new(capacity: usize) -> Self {
        Self {
            inner: moka::sync::Cache::new(capacity as u64),
            cap: capacity as u64,
        }
    }
    fn capacity(&self) -> usize {
        self.cap as usize
    }
    fn len(&self) -> usize {
        self.inner.run_pending_tasks();
        self.inner.entry_count() as usize
    }
    fn get(&mut self, key: &u64) -> Option<&u64> {
        let hit = self.inner.get(key).is_some();
        self.inner.run_pending_tasks();
        if hit { Some(&MOKA_DUMMY) } else { None }
    }
    fn insert(&mut self, key: u64, value: u64) -> Option<(u64, u64)> {
        self.inner.insert(key, value);
        self.inner.run_pending_tasks();
        None
    }
    fn contains_key(&self, key: &u64) -> bool {
        self.inner.contains_key(key)
    }
}
// use senba::experimental::sieve_v0::SieveCache as V0;
// use senba::experimental::sieve_v3::SieveCache as V3;
use senba::workload::file;
use senba::workload::zipf::ZipfGen;

struct Args {
    source: String,
    skew: f64,
    keys: u64,
    seed: u64,
    len: Option<usize>,
    path: Option<String>,
    capacities: Vec<usize>,
    variants: Vec<String>,
}

struct Stats {
    elapsed_ns: u128,
    hits: u64,
    misses: u64,
    evictions: u64,
}

fn drive<C: CacheImpl<u64, u64>>(trace: &[u64], cap: usize) -> Stats {
    let mut c = C::new(cap);
    let mut hits = 0u64;
    let mut misses = 0u64;
    let mut evictions = 0u64;
    let t0 = Instant::now();
    for &k in trace {
        if c.get(&k).is_some() {
            hits += 1;
        } else {
            misses += 1;
            if c.insert(k, k).is_some() {
                evictions += 1;
            }
        }
    }
    Stats {
        elapsed_ns: t0.elapsed().as_nanos(),
        hits,
        misses,
        evictions,
    }
}

/// senba::Cache 専用 driver。`with_shards` を呼ぶために CacheImpl 経由ではなく
/// 具体型を直接構築する。`drive` と同じ計測ロジック。
fn drive_senba<S: senba::SlotSize>(trace: &[u64], cap: usize, shards: usize) -> Stats {
    let mut c = Senba::<u64, u64, S>::with_shards(cap, shards);
    let mut hits = 0u64;
    let mut misses = 0u64;
    let mut evictions = 0u64;
    let t0 = Instant::now();
    for &k in trace {
        if c.get(&k).is_some() {
            hits += 1;
        } else {
            misses += 1;
            if c.insert(k, k).is_some() {
                evictions += 1;
            }
        }
    }
    Stats {
        elapsed_ns: t0.elapsed().as_nanos(),
        hits,
        misses,
        evictions,
    }
}

fn drive_senba_str<S: senba::SlotSize>(trace: &[String], cap: usize, shards: usize) -> Stats {
    let mut c = Senba::<String, u64, S>::with_shards(cap, shards);
    let mut hits = 0u64;
    let mut misses = 0u64;
    let mut evictions = 0u64;
    let t0 = Instant::now();
    for (i, k) in trace.iter().enumerate() {
        if c.get(k).is_some() {
            hits += 1;
        } else {
            misses += 1;
            if c.insert(k.clone(), i as u64).is_some() {
                evictions += 1;
            }
        }
    }
    Stats {
        elapsed_ns: t0.elapsed().as_nanos(),
        hits,
        misses,
        evictions,
    }
}

/// String キー版 driver。Twitter trace の生 anonymized_key を `Cache<String, u64>` 系に
/// 流す。value は `u64` 固定 (= ヒット行を index にした値) で、insert 時に `key.clone()` する。
fn drive_str<C: CacheImpl<String, u64>>(trace: &[String], cap: usize) -> Stats {
    let mut c = C::new(cap);
    let mut hits = 0u64;
    let mut misses = 0u64;
    let mut evictions = 0u64;
    let t0 = Instant::now();
    for (i, k) in trace.iter().enumerate() {
        if c.get(k).is_some() {
            hits += 1;
        } else {
            misses += 1;
            if c.insert(k.clone(), i as u64).is_some() {
                evictions += 1;
            }
        }
    }
    Stats {
        elapsed_ns: t0.elapsed().as_nanos(),
        hits,
        misses,
        evictions,
    }
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut source = String::from("zipf");
    let mut skew = f64::NAN;
    let mut keys = 0u64;
    let mut seed = 0u64;
    let mut len: Option<usize> = None;
    let mut path: Option<String> = None;
    let mut capacities: Vec<usize> = Vec::new();
    let mut variants: Vec<String> = Vec::new();

    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let mut val = || {
            it.next()
                .unwrap_or_else(|| panic!("expected value after {flag}"))
        };
        match flag.as_str() {
            "--source" => source = val().clone(),
            "--skew" => skew = val().parse().expect("--skew is f64"),
            "--keys" => keys = val().parse().expect("--keys is u64"),
            "--seed" => seed = val().parse().expect("--seed is u64"),
            "--len" => len = Some(val().parse().expect("--len is usize")),
            "--path" => path = Some(val().clone()),
            "--capacity" => {
                capacities = val()
                    .split(',')
                    .map(|s| {
                        s.trim()
                            .parse::<usize>()
                            .expect("--capacity entry is usize")
                    })
                    .collect();
            }
            "--variant" => {
                variants = val().split(',').map(|s| s.trim().to_string()).collect();
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: bench --source <zipf|file|twitter|twitter-string|libcachesim-csv> [--skew F --keys N --seed N --len N | --path P] --capacity C1,C2,... --variant orig,v0"
                );
                std::process::exit(0);
            }
            other => panic!("unknown flag: {other}"),
        }
    }

    if capacities.is_empty() {
        panic!("--capacity is required");
    }
    if variants.is_empty() {
        panic!("--variant is required");
    }

    Args {
        source,
        skew,
        keys,
        seed,
        len,
        path,
        capacities,
        variants,
    }
}

fn build_trace_string(args: &Args) -> Vec<String> {
    // 現状 String trace は twitter-string ソース固有なので分岐は単純。
    let p = args
        .path
        .as_ref()
        .expect("--path required for --source twitter-string");
    let it = file::twitter_csv_from_path_string(p).expect("open trace");
    match args.len {
        Some(n) => it.take(n).collect(),
        None => it.collect(),
    }
}

fn build_trace(args: &Args) -> Vec<u64> {
    match args.source.as_str() {
        "zipf" => {
            let n = args.len.expect("--len required for --source zipf");
            assert!(args.keys > 0, "--keys required for --source zipf");
            assert!(args.skew.is_finite(), "--skew required for --source zipf");
            ZipfGen::new(args.skew, args.keys, args.seed)
                .take(n)
                .collect()
        }
        "file" => {
            let p = args
                .path
                .as_ref()
                .expect("--path required for --source file");
            let it = file::from_path(p).expect("open trace");
            match args.len {
                Some(n) => it.take(n).collect(),
                None => it.collect(),
            }
        }
        // `file` (NSDI24 zipf_1.0 = u64 1 列) と `twitter` (OSDI'20 CSV) は
        // 別 source として扱う。auto-detect は拡張子なし trace で誤判定しうる
        // し、将来 oracleGeneral binary を増やすときも同じパターンで分岐できる。
        "twitter" => {
            let p = args
                .path
                .as_ref()
                .expect("--path required for --source twitter");
            let it = file::twitter_csv_from_path(p).expect("open trace");
            match args.len {
                Some(n) => it.take(n).collect(),
                None => it.collect(),
            }
        }
        // libCacheSim 同梱 CSV: `# time, object, size, next_access_vtime` 形式。
        // object 列が数値 u64 なので String hash を経由せず直接食う。
        "libcachesim-csv" => {
            let p = args
                .path
                .as_ref()
                .expect("--path required for --source libcachesim-csv");
            let it = file::libcachesim_csv_from_path(p).expect("open trace");
            match args.len {
                Some(n) => it.take(n).collect(),
                None => it.collect(),
            }
        }
        other => panic!("unknown --source: {other}"),
    }
}

fn run_string_keys(args: &Args) {
    let trace = build_trace_string(args);
    println!("variant,source,skew,keys,len,capacity,elapsed_ns,hits,misses,evictions");
    for v in &args.variants {
        for &cap in &args.capacities {
            // 現状 String 経路は orig と senba::Cache (default Slot32, 8 shards) のみ。
            // senba::Cache<String, u64> は Entry<String, u64> = 32B で Slot32 にちょうど収まる。
            // Slot32 default: Entry<String, u64> = 24 + 8 = 32B ちょうど。
            // SHARDS は cap / per_shard に応じて選択 (per-shard ≤ 64 制約のため
            // cap が大きいほど SHARDS を増やす必要がある)。
            let s = match v.as_str() {
                "orig" => drive_str::<Orig<String, u64>>(&trace, cap),
                "senba" => drive_str::<Senba<String, u64>>(&trace, cap),
                "senba_n16" => drive_senba_str::<senba::Slot32>(&trace, cap, 16),
                "senba_n32" => drive_senba_str::<senba::Slot32>(&trace, cap, 32),
                "senba_n64" => drive_senba_str::<senba::Slot32>(&trace, cap, 64),
                "senba_n128" => drive_senba_str::<senba::Slot32>(&trace, cap, 128),
                "senba_n256" => drive_senba_str::<senba::Slot32>(&trace, cap, 256),
                "senba_n512" => drive_senba_str::<senba::Slot32>(&trace, cap, 512),
                "senba_n1024" => drive_senba_str::<senba::Slot32>(&trace, cap, 1024),
                "senba_n2048" => drive_senba_str::<senba::Slot32>(&trace, cap, 2048),
                other => panic!("unknown variant for twitter-string: {other}"),
            };
            println!(
                "{v},{},{},{},{},{cap},{},{},{},{}",
                args.source,
                args.skew,
                args.keys,
                trace.len(),
                s.elapsed_ns,
                s.hits,
                s.misses,
                s.evictions
            );
        }
    }
}

fn main() {
    let args = parse_args();
    if args.source == "twitter-string" {
        run_string_keys(&args);
        return;
    }
    let trace = build_trace(&args);

    println!("variant,source,skew,keys,len,capacity,elapsed_ns,hits,misses,evictions");
    for v in &args.variants {
        for &cap in &args.capacities {
            let s = match v.as_str() {
                "orig" => drive::<Orig<u64, u64>>(&trace, cap),
                // 2026-05-05: ベースラインは orig vs j7 のみ。過去 variant は
                // モジュール自体は残置 (`src/sieve_*.rs` + `src/lib.rs`)、必要な
                // 比較が再発したらここを復活させる。
                // "v0" => drive::<V0<u64, u64>>(&trace, cap),
                // "v3" => drive::<V3<u64, u64>>(&trace, cap),
                // "j3" => drive::<J3<u64, u64>>(&trace, cap),
                // "j4" => drive::<J4<u64, u64>>(&trace, cap),
                // "j4_n1" => drive::<J4<u64, u64, 1>>(&trace, cap),
                // "j4_n2" => drive::<J4<u64, u64, 2>>(&trace, cap),
                // "j4_n4" => drive::<J4<u64, u64, 4>>(&trace, cap),
                // "j4_n8" => drive::<J4<u64, u64, 8>>(&trace, cap),
                // "j4_n16" => drive::<J4<u64, u64, 16>>(&trace, cap),
                // "j4_n32" => drive::<J4<u64, u64, 32>>(&trace, cap),
                // "j4_n64" => drive::<J4<u64, u64, 64>>(&trace, cap),
                // "j4_n128" => drive::<J4<u64, u64, 128>>(&trace, cap),
                // "j5" => drive::<J5<u64, u64>>(&trace, cap),
                // "j5_n1" => drive::<J5<u64, u64, 1>>(&trace, cap),
                // "j5_n2" => drive::<J5<u64, u64, 2>>(&trace, cap),
                // "j5_n4" => drive::<J5<u64, u64, 4>>(&trace, cap),
                // "j5_n8" => drive::<J5<u64, u64, 8>>(&trace, cap),
                // "j5_n16" => drive::<J5<u64, u64, 16>>(&trace, cap),
                // "j5_n32" => drive::<J5<u64, u64, 32>>(&trace, cap),
                // "j5_n64" => drive::<J5<u64, u64, 64>>(&trace, cap),
                // "j5_n128" => drive::<J5<u64, u64, 128>>(&trace, cap),
                // "j5_n256" => drive::<J5<u64, u64, 256>>(&trace, cap),
                // "j5_n512" => drive::<J5<u64, u64, 512>>(&trace, cap),
                // "j5_n1024" => drive::<J5<u64, u64, 1024>>(&trace, cap),
                // "j5_n2048" => drive::<J5<u64, u64, 2048>>(&trace, cap),
                // "j6" => drive::<J6<u64, u64>>(&trace, cap),
                // "j6_n1" => drive::<J6<u64, u64, 1>>(&trace, cap),
                // "j6_n2" => drive::<J6<u64, u64, 2>>(&trace, cap),
                // "j6_n4" => drive::<J6<u64, u64, 4>>(&trace, cap),
                // "j6_n8" => drive::<J6<u64, u64, 8>>(&trace, cap),
                // "j6_n16" => drive::<J6<u64, u64, 16>>(&trace, cap),
                // "j6_n32" => drive::<J6<u64, u64, 32>>(&trace, cap),
                // "j6_n64" => drive::<J6<u64, u64, 64>>(&trace, cap),
                // "j6_n128" => drive::<J6<u64, u64, 128>>(&trace, cap),
                // "j6_n256" => drive::<J6<u64, u64, 256>>(&trace, cap),
                // "j6_n512" => drive::<J6<u64, u64, 512>>(&trace, cap),
                // "j6_n1024" => drive::<J6<u64, u64, 1024>>(&trace, cap),
                // "j6_n2048" => drive::<J6<u64, u64, 2048>>(&trace, cap),
                "j7" => drive::<J7<u64, u64>>(&trace, cap),
                "j7_n1" => drive::<J7<u64, u64, 1>>(&trace, cap),
                "j7_n2" => drive::<J7<u64, u64, 2>>(&trace, cap),
                "j7_n4" => drive::<J7<u64, u64, 4>>(&trace, cap),
                "j7_n8" => drive::<J7<u64, u64, 8>>(&trace, cap),
                "j7_n16" => drive::<J7<u64, u64, 16>>(&trace, cap),
                "j7_n32" => drive::<J7<u64, u64, 32>>(&trace, cap),
                "j7_n64" => drive::<J7<u64, u64, 64>>(&trace, cap),
                "j7_n128" => drive::<J7<u64, u64, 128>>(&trace, cap),
                "j7_n256" => drive::<J7<u64, u64, 256>>(&trace, cap),
                "j7_n512" => drive::<J7<u64, u64, 512>>(&trace, cap),
                "j7_n1024" => drive::<J7<u64, u64, 1024>>(&trace, cap),
                "j7_n2048" => drive::<J7<u64, u64, 2048>>(&trace, cap),
                // j8 は per_shard <= 64 (= MAX_PER_SHARD) を Inner::new で assert する。
                // 例: cap=4096 + j8_n64 ⇒ per_shard=64 で OK、cap=4096 + j8_n32 ⇒ per_shard=128 で panic。
                "j8" => drive::<J8<u64, u64>>(&trace, cap),
                "j8_n16" => drive::<J8<u64, u64, 16>>(&trace, cap),
                "j8_n32" => drive::<J8<u64, u64, 32>>(&trace, cap),
                "j8_n64" => drive::<J8<u64, u64, 64>>(&trace, cap),
                "j8_n128" => drive::<J8<u64, u64, 128>>(&trace, cap),
                "j8_n256" => drive::<J8<u64, u64, 256>>(&trace, cap),
                "j8_n512" => drive::<J8<u64, u64, 512>>(&trace, cap),
                "j8_n1024" => drive::<J8<u64, u64, 1024>>(&trace, cap),
                "j8_n2048" => drive::<J8<u64, u64, 2048>>(&trace, cap),
                // senba::Cache<u64, u64> (Slot32 default). per-shard <= 64 制約のため
                // cap が大きいときは SHARDS を増やす必要がある (cap=30000 → n512 等)。
                "senba_n16" => drive_senba::<senba::Slot32>(&trace, cap, 16),
                "senba_n32" => drive_senba::<senba::Slot32>(&trace, cap, 32),
                "senba_n64" => drive_senba::<senba::Slot32>(&trace, cap, 64),
                "senba_n128" => drive_senba::<senba::Slot32>(&trace, cap, 128),
                "senba_n256" => drive_senba::<senba::Slot32>(&trace, cap, 256),
                "senba_n512" => drive_senba::<senba::Slot32>(&trace, cap, 512),
                "senba_n1024" => drive_senba::<senba::Slot32>(&trace, cap, 1024),
                "senba_n2048" => drive_senba::<senba::Slot32>(&trace, cap, 2048),
                // W-TinyLFU 比較。HR と ns/op のみ意味あり、evictions は 0 固定。
                "mini_moka" => drive::<MiniMoka>(&trace, cap),
                // moka 0.12 (adaptive window sizing 付き W-TinyLFU)。
                "moka" => drive::<Moka>(&trace, cap),
                other => panic!("unknown variant: {other}"),
            };
            println!(
                "{v},{},{},{},{},{cap},{},{},{},{}",
                args.source,
                args.skew,
                args.keys,
                trace.len(),
                s.elapsed_ns,
                s.hits,
                s.misses,
                s.evictions
            );
        }
    }
}
