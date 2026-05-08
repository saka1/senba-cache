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

use senba_research::CacheImpl;
// 2026-05-05: ベースラインを orig vs j7 に絞り込み。過去 variant は
// 必要になれば use と matcher を復活させる (テスト・実装は残置)。
// use senba_research::experimental::sieve_j3::SieveCache as J3;
// use senba_research::experimental::sieve_j4::SieveCache as J4;
// use senba_research::experimental::sieve_j5::SieveCache as J5;
// use senba_research::experimental::sieve_j6::SieveCache as J6;
use senba::Cache as Senba;
use senba_research::experimental::sieve_j7::SieveCache as J7;
use senba_research::experimental::sieve_j8::SieveCache as J8;
use senba_research::experimental::sieve_orig::SieveCache as Orig;

/// W-TinyLFU 比較用に `mini_moka::sync::Cache<u64,u64>` を `Cache<u64,u64>` に被せる
/// thin wrapper。bench でのみ使うので bench.rs 内に閉じる。
///
/// **重要**: mini-moka sync は read/write log を内部 buffer にためて amortize するため、
/// 明示的に `ConcurrentCacheExt::sync()` を呼ばないと CMSketch 更新と admission 決定が
/// 反映されない。upstream の test code (sync/cache.rs `basic_single_thread`) が毎回
/// sync() を呼んでいることから、決定的な HR を測るには get/insert 後に sync() が必須。
/// 呼ばないと admission 判定が遅延し、新規 key が write buffer overflow で落ちて
/// HR が崩壊する。本 adapter では HR の正しさを優先し、毎 op 後に sync() を呼ぶ。
/// その分 ns/op は実態より悪化するが、HR が screening の gate なので許容する。
///
/// 単スレッド比較で sync overhead 抜きの「純 admission policy」評価が欲しい時は
/// `MiniMokaUnsync` を使う (`mini_moka::unsync::Cache`)。
///
/// 制約:
/// - mini_moka の `insert` は `()` を返すため、追い出された (K,V) は取れない。
///   `Cache::insert` は常に `None` を返す → CSV の evictions 列は 0 固定で**意味が無い**。
///   HR と ns/op だけ参照すること。
/// - `get` は `Option<V>` (clone) を返す。trait は `Option<&V>` 要求。bench の `drive`
///   は `.is_some()` しか見ないので、ヒット時はダミー静的参照を返して整合させる。
/// - `max_capacity` は重み合計の budget。default weighter は entry あたり 1 なので
///   おおむね entry 数 == capacity と見做せる。
struct MiniMokaSync<K> {
    inner: mini_moka::sync::Cache<K, u64, senba::Xxh3Build>,
    cap: u64,
}

const MINI_MOKA_DUMMY: u64 = 0;

impl<K> senba_research::CacheImpl<K, u64> for MiniMokaSync<K>
where
    K: std::hash::Hash + Eq + Send + Sync + Clone + 'static,
{
    fn new(capacity: usize) -> Self {
        Self {
            inner: mini_moka::sync::Cache::builder()
                .max_capacity(capacity as u64)
                .build_with_hasher(senba::Xxh3Build),
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
    fn get(&mut self, key: &K) -> Option<&u64> {
        use mini_moka::sync::ConcurrentCacheExt;
        let hit = self.inner.get(key).is_some();
        self.inner.sync();
        if hit { Some(&MINI_MOKA_DUMMY) } else { None }
    }
    fn insert(&mut self, key: K, value: u64) -> Option<(K, u64)> {
        use mini_moka::sync::ConcurrentCacheExt;
        self.inner.insert(key, value);
        self.inner.sync();
        None
    }
    fn contains_key(&self, key: &K) -> bool {
        self.inner.contains_key(key)
    }
}

/// `mini_moka::unsync::Cache` を `senba_research::CacheImpl` に被せる thin wrapper。
/// `MiniMokaSync` と違い `&mut self` 版で内部 atomic / write log が無く、
/// 単スレッド公平条件で senba::Cache (`&mut self` 単スレ) と直接突き合わせるための
/// 駆動系。hasher は senba::Cache のデフォルトと同じ `senba::Xxh3Build` を
/// 注入してハッシュ品質差を消す。
///
/// 制約:
/// - `insert` は `()` 返しで追い出された (K,V) は取れない → CSV の evictions は 0 固定。
/// - `unsync::Cache::contains_key` は `&mut self` だが trait は `&self` 要求のため
///   bench drive 経路では呼ばれない (使うのは get/insert のみ)。trait 要求を満たすために
///   `unimplemented!()` で stub する。実 bench は壊れない。
struct MiniMokaUnsync<K> {
    inner: mini_moka::unsync::Cache<K, u64, senba::Xxh3Build>,
    cap: u64,
}

impl<K> senba_research::CacheImpl<K, u64> for MiniMokaUnsync<K>
where
    K: std::hash::Hash + Eq + 'static,
{
    fn new(capacity: usize) -> Self {
        Self {
            inner: mini_moka::unsync::Cache::builder()
                .max_capacity(capacity as u64)
                .build_with_hasher(senba::Xxh3Build),
            cap: capacity as u64,
        }
    }
    fn capacity(&self) -> usize {
        self.cap as usize
    }
    fn len(&self) -> usize {
        self.inner.entry_count() as usize
    }
    fn get(&mut self, key: &K) -> Option<&u64> {
        self.inner.get(key)
    }
    fn insert(&mut self, key: K, value: u64) -> Option<(K, u64)> {
        self.inner.insert(key, value);
        None
    }
    fn contains_key(&self, _key: &K) -> bool {
        unimplemented!("MiniMokaUnsync::contains_key not exercised by bench drive")
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

impl senba_research::CacheImpl<u64, u64> for Moka {
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
// use senba_research::experimental::sieve_v0::SieveCache as V0;
// use senba_research::experimental::sieve_v3::SieveCache as V3;
use senba_research::workload::file;
use senba_research::workload::zipf::ZipfGen;

struct Args {
    source: String,
    skew: f64,
    keys: u64,
    seed: u64,
    len: Option<usize>,
    path: Option<String>,
    capacities: Vec<usize>,
    variants: Vec<String>,
    /// trace を何周流すか。warmup 込みで連続実行したい時の amortize 用。
    /// default 1。
    repeat: u32,
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
    let mut source_explicit = false;
    let mut skew = f64::NAN;
    let mut keys = 0u64;
    let mut seed = 0u64;
    let mut len: Option<usize> = None;
    let mut path: Option<String> = None;
    let mut capacities: Vec<usize> = Vec::new();
    let mut variants: Vec<String> = Vec::new();
    let mut arc_preset: Option<String> = None;
    let mut repeat: u32 = 1;

    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let mut val = || {
            it.next()
                .unwrap_or_else(|| panic!("expected value after {flag}"))
        };
        match flag.as_str() {
            "--source" => {
                source = val().clone();
                source_explicit = true;
            }
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
            "--arc-preset" => arc_preset = Some(val().clone()),
            "--repeat" => repeat = val().parse().expect("--repeat is u32 >= 1"),
            "-h" | "--help" => {
                eprintln!(
                    "usage: bench --source <zipf|file|twitter|twitter-string|libcachesim-csv|arc> [--skew F --keys N --seed N --len N | --path P] --capacity C1,C2,... --variant orig,v0\n  --arc-preset <s3|oltp|ds1|p1..p14|s1|s2|concat|merge-p|merge-s>  ARC trace の path / capacity 配列を一括解決\n  --repeat <N>  trace を N 周流す (default 1)"
                );
                std::process::exit(0);
            }
            other => panic!("unknown flag: {other}"),
        }
    }

    // --arc-preset 解決: 明示指定が無いフィールドだけ preset で埋める。
    if let Some(name) = &arc_preset {
        let (preset_path, preset_caps) =
            arc_preset_lookup(name).unwrap_or_else(|| panic!("unknown --arc-preset name: {name}"));
        if !source_explicit {
            source = String::from("arc");
        }
        if path.is_none() {
            path = Some(preset_path.to_string());
        }
        if capacities.is_empty() {
            capacities = preset_caps.to_vec();
        }
    }

    if repeat == 0 {
        panic!("--repeat must be >= 1");
    }
    if capacities.is_empty() {
        panic!("--capacity (or --arc-preset) is required");
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
        repeat,
    }
}

/// mokabench の `TraceFile::default_capacities` (`external/mokabench/src/trace_file.rs`)
/// の ARC 行を転記。trace は `external/mokabench/cache-trace/arc/<NAME>.lis.zst` に
/// 配置済みの zstd 圧縮形式を直接読む。spc1likeread は split zst (`.zst.00`/...) で
/// 連結 reader が必要なため preset から外す。
fn arc_preset_lookup(name: &str) -> Option<(&'static str, &'static [usize])> {
    let entry: (&'static str, &'static [usize]) = match name.trim().to_ascii_lowercase().as_str() {
        "concat" => (
            "external/mokabench/cache-trace/arc/ConCat.lis.zst",
            &[200_000, 400_000, 3_200_000],
        ),
        "ds1" => (
            "external/mokabench/cache-trace/arc/DS1.lis.zst",
            &[1_000_000, 4_000_000, 8_000_000],
        ),
        "merge-p" | "mergep" => (
            "external/mokabench/cache-trace/arc/MergeP.lis.zst",
            &[400_000, 1_000_000, 3_200_000],
        ),
        "merge-s" | "merges" => (
            "external/mokabench/cache-trace/arc/MergeS.lis.zst",
            &[400_000, 1_000_000, 3_200_000],
        ),
        "oltp" => (
            "external/mokabench/cache-trace/arc/OLTP.lis.zst",
            &[256, 512, 1_000, 2_000],
        ),
        "p1" => (
            "external/mokabench/cache-trace/arc/P1.lis.zst",
            &[20_000, 160_000],
        ),
        "p2" => (
            "external/mokabench/cache-trace/arc/P2.lis.zst",
            &[20_000, 160_000],
        ),
        "p3" => (
            "external/mokabench/cache-trace/arc/P3.lis.zst",
            &[20_000, 160_000],
        ),
        "p4" => (
            "external/mokabench/cache-trace/arc/P4.lis.zst",
            &[20_000, 160_000],
        ),
        "p5" => (
            "external/mokabench/cache-trace/arc/P5.lis.zst",
            &[20_000, 160_000],
        ),
        "p6" => (
            "external/mokabench/cache-trace/arc/P6.lis.zst",
            &[20_000, 160_000],
        ),
        "p7" => (
            "external/mokabench/cache-trace/arc/P7.lis.zst",
            &[20_000, 160_000],
        ),
        "p8" => (
            "external/mokabench/cache-trace/arc/P8.lis.zst",
            &[20_000, 160_000],
        ),
        "p9" => (
            "external/mokabench/cache-trace/arc/P9.lis.zst",
            &[20_000, 160_000],
        ),
        "p10" => (
            "external/mokabench/cache-trace/arc/P10.lis.zst",
            &[20_000, 160_000],
        ),
        "p11" => (
            "external/mokabench/cache-trace/arc/P11.lis.zst",
            &[20_000, 160_000],
        ),
        "p12" => (
            "external/mokabench/cache-trace/arc/P12.lis.zst",
            &[20_000, 160_000],
        ),
        "p13" => (
            "external/mokabench/cache-trace/arc/P13.lis.zst",
            &[20_000, 160_000],
        ),
        "p14" => (
            "external/mokabench/cache-trace/arc/P14.lis.zst",
            &[80_000, 640_000],
        ),
        "s1" => (
            "external/mokabench/cache-trace/arc/S1.lis.zst",
            &[100_000, 800_000],
        ),
        "s2" => (
            "external/mokabench/cache-trace/arc/S2.lis.zst",
            &[100_000, 800_000],
        ),
        "s3" => (
            "external/mokabench/cache-trace/arc/S3.lis.zst",
            &[100_000, 400_000, 800_000],
        ),
        _ => return None,
    };
    Some(entry)
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
        // ARC paper trace (mokabench 同梱 `external/mokabench/cache-trace/arc/*.lis[.zst]`)。
        // 各行 `start len` を `start..start+len` に展開して u64 key 列にする。
        // 出典: mokabench (https://github.com/moka-rs/mokabench)、trace は
        // cache-trace submodule (https://github.com/moka-rs/cache-trace)。
        "arc" => {
            let p = args
                .path
                .as_ref()
                .expect("--path required for --source arc");
            let it = file::arc_from_path(p).expect("open trace");
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
    let trace_once = build_trace_string(args);
    let trace: Vec<String> = if args.repeat == 1 {
        trace_once
    } else {
        let mut v = Vec::with_capacity(trace_once.len() * args.repeat as usize);
        for _ in 0..args.repeat {
            v.extend(trace_once.iter().cloned());
        }
        v
    };
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
                "mini_moka" | "mini_moka_sync" => drive_str::<MiniMokaSync<String>>(&trace, cap),
                "mini_moka_unsync" => drive_str::<MiniMokaUnsync<String>>(&trace, cap),
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
    let trace_once = build_trace(&args);
    // --repeat: trace を N 周分連結。default 1 なので clone コストはゼロ。
    let trace: Vec<u64> = if args.repeat == 1 {
        trace_once
    } else {
        let mut v = Vec::with_capacity(trace_once.len() * args.repeat as usize);
        for _ in 0..args.repeat {
            v.extend_from_slice(&trace_once);
        }
        v
    };

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
                // senba::Cache<u64, u64> (Slot32 default)。`senba` は `Cache::new(cap)`
                // が SHARDS を自動選択する canonical 経路 (next_pow2(ceil(cap/64)))。
                // `senba_nNNN` は SHARDS を pin してスイープするための bench 専用
                // variant — per-shard 上限は 64 (tag 内 6-bit ID) なので、変な N を
                // 指定すると assert で落ちる / per-shard が小さすぎて HR が劣化する。
                "senba" => drive::<Senba<u64, u64>>(&trace, cap),
                "senba_n16" => drive_senba::<senba::Slot32>(&trace, cap, 16),
                "senba_n32" => drive_senba::<senba::Slot32>(&trace, cap, 32),
                "senba_n64" => drive_senba::<senba::Slot32>(&trace, cap, 64),
                "senba_n128" => drive_senba::<senba::Slot32>(&trace, cap, 128),
                "senba_n256" => drive_senba::<senba::Slot32>(&trace, cap, 256),
                "senba_n512" => drive_senba::<senba::Slot32>(&trace, cap, 512),
                "senba_n1024" => drive_senba::<senba::Slot32>(&trace, cap, 1024),
                "senba_n2048" => drive_senba::<senba::Slot32>(&trace, cap, 2048),
                // W-TinyLFU 比較。HR と ns/op のみ意味あり、evictions は 0 固定。
                // `mini_moka` は後方互換 alias (sync 実装に解決)。
                "mini_moka" | "mini_moka_sync" => drive::<MiniMokaSync<u64>>(&trace, cap),
                // 単スレ公平比較用: unsync 版 (内部 atomic / write log 無し)。
                "mini_moka_unsync" => drive::<MiniMokaUnsync<u64>>(&trace, cap),
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
