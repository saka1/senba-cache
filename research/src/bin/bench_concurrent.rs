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
//! ### Value type (`--value u64|string`)
//! V=`u64` (default) は SIEVE 系の Slot16 自然形、V=`String` (`format!("v{k:08}")`)
//! は Slot32 自然形 (24B `String` header + 8B key = 32B Entry)。`format!` は
//! hot path 内で呼ばれるが全 String variant に同条件でかかるため、relative
//! 比較は公平 (perf-gate のシナリオ 7/8 と同じ作法)。`c8` は `V: Copy` を要求するため
//! `--value string` 不可、parse 時に弾く。
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

use senba::concurrent::PartitionedCache;
use senba_research::experimental::sieve_c8::ConcurrentSieveCache;
use senba_research::experimental::sieve_c9::ConcurrentSieveCache as ConcurrentSieveC9;
use senba_research::experimental::sieve_c14s::ConcurrentSieveCache as ConcurrentSieveC14S;
use senba_research::experimental::sieve_c15s::ConcurrentSieveCache as ConcurrentSieveC15S;
use senba_research::experimental::sieve_c16s::ConcurrentSieveCache as ConcurrentSieveC16S;
use senba_research::experimental::sieve_c17s::ConcurrentSieveCache as ConcurrentSieveC17S;
use senba_research::experimental::sieve_c18s::ConcurrentSieveCache as ConcurrentSieveC18S;
use senba_research::experimental::sieve_r1::ConcurrentSieveR1;
use senba_research::experimental::sieve_r2h::ConcurrentSieveR2h;
use senba_research::workload::arc_preset;
use senba_research::workload::file;
use senba_research::workload::zipf::ZipfGen;

/// per-op Instant を取らずに chunk 平均を取る単位。
/// 大きすぎると tail latency が見えず、小さすぎると Instant overhead が支配する。
/// 1024 は Caffeine の bench (chunk_size=1k) を踏襲。
const CHUNK_OPS: usize = 1024;

/// 各 variant が共通で受け付ける value 型。bench loop は `V::make(k)` で
/// hot path 内に value を生成する。生成コストは全 String variant に
/// 同条件でかかるため relative 比較は公平。
trait MakeValue: Clone + Send + Sync + 'static {
    fn make(k: u64) -> Self;
}

impl MakeValue for u64 {
    #[inline]
    fn make(k: u64) -> Self {
        k
    }
}

impl MakeValue for String {
    #[inline]
    fn make(k: u64) -> Self {
        format!("v{k:08}")
    }
}

/// 各 variant を同じ driver で叩くための最小 trait。
/// - `&self` のみ (どの実装も interior mutability を持つ)。
/// - `get` は hit/miss だけ返す (値は bench で使わない、clone コスト分が乗るのは
///   どの variant も同条件)。
trait ConcCache<V>: Send + Sync + 'static {
    fn build(capacity: usize, shards: usize) -> Arc<Self>;
    /// r1 等の routing variant 用に `ways` を渡す経路。default は `build` 互換。
    fn build_with_ways(capacity: usize, shards: usize, _ways: usize) -> Arc<Self> {
        Self::build(capacity, shards)
    }
    fn get_hit(&self, key: &u64) -> bool;
    fn insert(&self, key: u64, value: V);
}

// c8: `V: Copy` を要求する legacy variant。u64 専用。
impl<const S: usize> ConcCache<u64> for ConcurrentSieveCache<u64, u64, S> {
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

impl<V, const S: usize> ConcCache<V> for ConcurrentSieveC14S<u64, V, S>
where
    V: Clone + Send + Sync + 'static,
{
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveC14S::new(capacity))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveC14S::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: V) {
        let _ = ConcurrentSieveC14S::insert(self, key, value);
    }
}

impl<V, const S: usize> ConcCache<V> for ConcurrentSieveC16S<u64, V, S>
where
    V: Clone + Send + Sync + 'static,
{
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveC16S::new(capacity))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveC16S::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: V) {
        let _ = ConcurrentSieveC16S::insert(self, key, value);
    }
}

impl<V, const S: usize> ConcCache<V> for ConcurrentSieveC17S<u64, V, S>
where
    V: Clone + Send + Sync + 'static,
{
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveC17S::new(capacity))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveC17S::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: V) {
        let _ = ConcurrentSieveC17S::insert(self, key, value);
    }
}

impl<V, const S: usize> ConcCache<V> for ConcurrentSieveC18S<u64, V, S>
where
    V: Clone + Send + Sync + 'static,
{
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveC18S::new(capacity))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveC18S::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: V) {
        let _ = ConcurrentSieveC18S::insert(self, key, value);
    }
}

/// r1: `ways` を取る別 builder。c-series と異なり constructor 引数が変わるため
/// `ConcCache::build` シグネチャに収まらず、`R1Wrapper(ways)` 経由で newtype 化する。
struct R1Wrapper<V, const S: usize> {
    inner: ConcurrentSieveR1<u64, V, S>,
}

impl<V, const S: usize> R1Wrapper<V, S>
where
    V: Clone + Send + Sync + 'static,
{
    fn new(capacity: usize, ways: usize) -> Self {
        Self {
            inner: ConcurrentSieveR1::with_ways(capacity, ways),
        }
    }
}

impl<V, const S: usize> ConcCache<V> for R1Wrapper<V, S>
where
    V: Clone + Send + Sync + 'static,
{
    /// fallback (= `ways=1`)。実際の sweep は `build_with_ways` 経由。
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(Self::new(capacity, 1))
    }
    fn build_with_ways(capacity: usize, _shards: usize, ways: usize) -> Arc<Self> {
        Arc::new(Self::new(capacity, ways))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        self.inner.get(key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: V) {
        let _ = self.inner.insert(key, value);
    }
}

/// r2h: r1 の hash-routed control。同 routing 構造を持つので R1Wrapper と同型の薄い
/// newtype を一個増やすだけ。`--ways` 軸は r1 と同じ意味で取る。
struct R2hWrapper<V, const S: usize> {
    inner: ConcurrentSieveR2h<u64, V, S>,
}

impl<V, const S: usize> R2hWrapper<V, S>
where
    V: Clone + Send + Sync + 'static,
{
    fn new(capacity: usize, ways: usize) -> Self {
        Self {
            inner: ConcurrentSieveR2h::with_ways(capacity, ways),
        }
    }
}

impl<V, const S: usize> ConcCache<V> for R2hWrapper<V, S>
where
    V: Clone + Send + Sync + 'static,
{
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(Self::new(capacity, 1))
    }
    fn build_with_ways(capacity: usize, _shards: usize, ways: usize) -> Arc<Self> {
        Arc::new(Self::new(capacity, ways))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        self.inner.get(key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: V) {
        let _ = self.inner.insert(key, value);
    }
}

/// partitioned: `senba::concurrent::PartitionedCache` を `--partitions N` 経由で
/// 構築する newtype。`--shards` / `--ways` を無視し、`run_partitioned` が
/// 直接 `PartitionedWrapper::new(cap, partitions)` を呼ぶ (= `ConcCache::build*`
/// は fallback でのみ使われる)。
struct PartitionedWrapper<V> {
    inner: PartitionedCache<u64, V>,
}

impl<V> PartitionedWrapper<V>
where
    V: Clone + Send + Sync + 'static,
{
    fn new(capacity: usize, partitions: usize) -> Self {
        Self {
            inner: PartitionedCache::new(capacity, partitions),
        }
    }
}

impl<V> ConcCache<V> for PartitionedWrapper<V>
where
    V: Clone + Send + Sync + 'static,
{
    /// fallback (= `partitions=1`)。実際の sweep は `run_partitioned` 経由で
    /// `PartitionedWrapper::new` を直接呼ぶ。
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(Self::new(capacity, 1))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        self.inner.get(key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: V) {
        let _ = self.inner.insert(key, value);
    }
}

impl<V, const S: usize, const B: u32> ConcCache<V> for ConcurrentSieveC15S<u64, V, S, B>
where
    V: Clone + Send + Sync + 'static,
{
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveC15S::new(capacity))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveC15S::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: V) {
        let _ = ConcurrentSieveC15S::insert(self, key, value);
    }
}

impl<V> ConcCache<V> for ConcurrentSieveC9<u64, V>
where
    V: Clone + Send + Sync + 'static,
{
    fn build(capacity: usize, shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveC9::with_shards(capacity, shards))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveC9::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: V) {
        let _ = ConcurrentSieveC9::insert(self, key, value);
    }
}

impl<V> ConcCache<V> for moka::sync::Cache<u64, V>
where
    V: Clone + Send + Sync + 'static,
{
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
    fn insert(&self, key: u64, value: V) {
        moka::sync::Cache::insert(self, key, value);
    }
}

impl<V> ConcCache<V> for mini_moka::sync::Cache<u64, V>
where
    V: Clone + Send + Sync + 'static,
{
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(mini_moka::sync::Cache::new(capacity as u64))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        self.get(key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: V) {
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

/// CLI `--value` のパースから dispatch 用に持ち回す。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    U64,
    String,
}

impl ValueKind {
    fn as_str(self) -> &'static str {
        match self {
            ValueKind::U64 => "u64",
            ValueKind::String => "string",
        }
    }
}

/// CLI `--source` の workload 種別。Zipf は per-thread 独立生成、Twitter/Arc は trace 共有
/// + sliced replay (thread t は `trace[t*L/T .. (t+1)*L/T)` を循環)。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Zipf,
    /// libCacheSim 同梱 `# time, object, size, next_access_vtime` 形式
    /// (`external/NSDI24-SIEVE/libCacheSim/data/twitter_cluster52.csv`)。
    Twitter,
    /// OSDI'20 Yang `time,key,key_size,value_size,client,op,ttl` 形式
    /// (`external/twitter-cache-trace/cluster006` 等)。String key を u64 に hash。
    TwitterYang,
    Arc,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Zipf => "zipf",
            Source::Twitter => "twitter",
            Source::TwitterYang => "twitter-yang",
            Source::Arc => "arc",
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
    value_kind: ValueKind,
    /// r1 の routing affinity 軸 (power of 2、1 <= ways <= shards)。非 r1 variant では無視。
    ways: usize,
    /// partitioned の partition 数 (power of 2、>= 1)。`--partitions` 経由。非 partitioned
    /// variant では無視。設計: `docs/reports/2026-05-12-partitioned-design.md` §(T × N) sweep。
    partitions: usize,
    /// workload 種別。
    source: Source,
    /// `--source twitter|twitter-yang|arc` のとき trace ファイルパス必須。
    trace_file: Option<String>,
    /// CSV 用 metadata 列。例: `"cluster18"`, `"OLTP"`。Zipf では空文字。
    workload_param: String,
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
    let mut value_kind = ValueKind::U64;
    let mut ways: usize = 1;
    let mut partitions: usize = 1;
    let mut source = Source::Zipf;
    let mut trace_file: Option<String> = None;
    let mut workload_param: String = String::new();

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
            "--value" => {
                let v = val();
                value_kind = match v.as_str() {
                    "u64" => ValueKind::U64,
                    "string" => ValueKind::String,
                    other => panic!("--value must be u64|string, got: {other}"),
                };
            }
            "--ways" => ways = val().parse().expect("--ways is usize"),
            "--partitions" => partitions = val().parse().expect("--partitions is usize"),
            "--source" => {
                let v = val();
                source = match v.as_str() {
                    "zipf" => Source::Zipf,
                    "twitter" => Source::Twitter,
                    "twitter-yang" => Source::TwitterYang,
                    "arc" => Source::Arc,
                    other => panic!("--source must be zipf|twitter|twitter-yang|arc, got: {other}"),
                };
            }
            "--trace-file" => trace_file = Some(val().to_string()),
            "--workload-param" => workload_param = val().to_string(),
            "--arc-preset" => {
                // ARC trace を mokabench preset 名で一括解決。
                // 明示指定 (--source / --trace-file / --workload-param) が無いフィールドだけ埋める。
                // --cap は preset 側で複数候補を持つので、harness 側で `--cap N` を都度渡す
                // (bench.rs 流の "complete sweep を一発" 経路は本ツールでは取らない、
                //  T × cap の二重 sweep は harness 側で記述するため)。
                let name = val().to_string();
                let preset = arc_preset::lookup(&name)
                    .unwrap_or_else(|| panic!("unknown --arc-preset name: {name}"));
                source = Source::Arc;
                if trace_file.is_none() {
                    trace_file = Some(preset.trace_path.to_string());
                }
                if workload_param.is_empty() {
                    workload_param = name;
                }
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: bench_concurrent [--variant ...] [--shards N] [--ways N] [--partitions N] \
                     [--op-mix gim|read-heavy] [--value u64|string] \
                     [--source zipf|twitter|twitter-yang|arc] [--trace-file PATH] [--workload-param S] \
                     [--arc-preset NAME] \
                     --cap N --threads N --skew F --keys N --ops N --warmup N \
                     --trials N --seed N"
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
        shards.is_power_of_two() && (1..=131_072).contains(&shards),
        "--shards must be a power of two in [1, 131072]"
    );
    assert!(
        ways.is_power_of_two() && ways >= 1 && ways <= shards,
        "--ways must be a power of two in [1, --shards]"
    );
    assert!(
        partitions.is_power_of_two() && partitions >= 1,
        "--partitions must be a power of two >= 1"
    );
    for v in &variants {
        assert!(
            matches!(
                v.as_str(),
                "c8" | "c9"
                    | "moka"
                    | "mini_moka"
                    | "c14s"
                    | "c15s_16"
                    | "c15s_8"
                    | "c15s_4"
                    | "c16s"
                    | "c17s"
                    | "c18s"
                    | "r1"
                    | "r2h"
                    | "partitioned"
            ),
            "unknown variant: {v} (expected c8|c9|moka|mini_moka|c14s|c15s_{{16,8,4}}|c16s|c17s|c18s|r1|r2h|partitioned)"
        );
    }
    if matches!(value_kind, ValueKind::String) && variants.iter().any(|v| v == "c8") {
        panic!(
            "c8 requires --value u64 (V: Copy); remove c8 from --variant when using --value string"
        );
    }
    if !matches!(source, Source::Zipf) && trace_file.is_none() {
        panic!("--source {} requires --trace-file PATH", source.as_str());
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
        value_kind,
        ways,
        partitions,
        source,
        trace_file,
        workload_param,
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

/// 1 op で `get` 用 / `insert` 用のキーを供給する per-thread feed。
///
/// - `Zipf` は thread ごとに seed 違いの 2 つの `ZipfGen` を持ち、`get` 側 / `insert` 側で
///   独立 draw する (`read-heavy` で cache を hot 直撃しないため)。
/// - `Trace` は **sliced replay**: thread t は `trace[start..end)` の独立 slice を循環
///   再生する。`get` / `insert` 両方が同一 slice の循環ストリームを共有する (= 実
///   trace ベースでは read-heavy の insert 側も同 workload 分布で構わない)。
///
/// `ZipfGen` の CDF テーブル (~680 B) が enum sizeof を支配するが、per-thread に 1 個
/// 持つだけ (T<=32 で <= 22 KB) なので heap box 化せず stack inline で保持する。
/// hot path (`next_get` / `next_ins`) は branch 1 段 + indexed access のみ。
#[allow(clippy::large_enum_variant)]
enum ThreadFeed {
    Zipf {
        get_gen: ZipfGen,
        ins_gen: ZipfGen,
    },
    Trace {
        trace: Arc<Vec<u64>>,
        start: usize,
        end: usize,
        pos_get: usize,
        pos_ins: usize,
    },
}

impl ThreadFeed {
    #[inline]
    fn next_get(&mut self) -> u64 {
        match self {
            ThreadFeed::Zipf { get_gen, .. } => get_gen.next().unwrap(),
            ThreadFeed::Trace {
                trace,
                start,
                end,
                pos_get,
                ..
            } => {
                let idx = *start + (*pos_get % (*end - *start));
                *pos_get += 1;
                trace[idx]
            }
        }
    }
    #[inline]
    fn next_ins(&mut self) -> u64 {
        match self {
            ThreadFeed::Zipf { ins_gen, .. } => ins_gen.next().unwrap(),
            ThreadFeed::Trace {
                trace,
                start,
                end,
                pos_ins,
                ..
            } => {
                let idx = *start + (*pos_ins % (*end - *start));
                *pos_ins += 1;
                trace[idx]
            }
        }
    }
}

fn build_feed(tid: usize, args: &Args, trace: Option<&Arc<Vec<u64>>>) -> ThreadFeed {
    let seed = args.seed ^ (tid as u64);
    match args.source {
        Source::Zipf => ThreadFeed::Zipf {
            get_gen: ZipfGen::new(args.skew, args.keys, seed),
            ins_gen: ZipfGen::new(args.skew, args.keys, seed ^ 0x00C0_FFEE_DEAD_BEEF_u64),
        },
        Source::Twitter | Source::TwitterYang | Source::Arc => {
            let trace = trace
                .expect("trace must be loaded for --source twitter|twitter-yang|arc")
                .clone();
            let n = trace.len();
            let start = (tid * n) / args.threads;
            let end = ((tid + 1) * n) / args.threads;
            assert!(
                end > start,
                "trace slice for tid={tid} is empty (trace len={n} threads={})",
                args.threads
            );
            ThreadFeed::Trace {
                trace,
                start,
                end,
                pos_get: 0,
                pos_ins: 0,
            }
        }
    }
}

/// Twitter / ARC trace を 1 度だけ memory に load する。Zipf では `None`。
fn load_trace(args: &Args) -> Option<Arc<Vec<u64>>> {
    let path = args.trace_file.as_deref()?;
    let trace: Vec<u64> = match args.source {
        Source::Zipf => return None,
        Source::Twitter => file::libcachesim_csv_from_path(path)
            .unwrap_or_else(|e| panic!("twitter trace open failed ({path}): {e}"))
            .collect(),
        Source::TwitterYang => file::twitter_csv_from_path(path)
            .unwrap_or_else(|e| panic!("twitter-yang trace open failed ({path}): {e}"))
            .collect(),
        Source::Arc => file::arc_from_path(path)
            .unwrap_or_else(|e| panic!("arc trace open failed ({path}): {e}"))
            .collect(),
    };
    assert!(!trace.is_empty(), "trace is empty: {path}");
    Some(Arc::new(trace))
}

fn run_trial<V, C>(args: &Args, trace: Option<Arc<Vec<u64>>>) -> TrialResult
where
    V: MakeValue,
    C: ConcCache<V>,
{
    let cache = C::build_with_ways(args.cap, args.shards, args.ways);
    run_trial_with::<V, C>(cache, args, trace)
}

/// run_trial の inner loop。cache 構築を caller に外出しすることで partitioned
/// variant 等の non-uniform constructor (= `--partitions` 経由) も同じ harness で
/// 叩けるようにする。
fn run_trial_with<V, C>(cache: Arc<C>, args: &Args, trace: Option<Arc<Vec<u64>>>) -> TrialResult
where
    V: MakeValue,
    C: ConcCache<V>,
{
    let barrier = Arc::new(Barrier::new(args.threads + 1));
    let warmup_per_thread = args.warmup / args.threads;
    let ops_per_thread = args.ops / args.threads;
    let op_mix = args.op_mix;

    let results: Vec<ThreadResult> = std::thread::scope(|s| {
        let mut handles = Vec::new();
        for tid in 0..args.threads {
            let cache = Arc::clone(&cache);
            let barrier = Arc::clone(&barrier);
            let mut feed = build_feed(tid, args, trace.as_ref());
            handles.push(s.spawn(move || {
                for _ in 0..warmup_per_thread {
                    let k = feed.next_get();
                    if !cache.get_hit(&k) {
                        cache.insert(k, V::make(k));
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
                        OpMix::Gim => {
                            let k = feed.next_get();
                            if cache.get_hit(&k) {
                                hits += 1;
                            } else {
                                cache.insert(k, V::make(k));
                            }
                        }
                        OpMix::ReadHeavy => {
                            if i.is_multiple_of(READ_HEAVY_INSERT_EVERY) {
                                let k = feed.next_ins();
                                cache.insert(k, V::make(k));
                            } else {
                                let k = feed.next_get();
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
    let shards_col = if matches!(
        variant,
        "c8" | "c9"
            | "c14s"
            | "c15s_16"
            | "c15s_8"
            | "c15s_4"
            | "c16s"
            | "c17s"
            | "c18s"
            | "r1"
            | "r2h"
    ) {
        args.shards
    } else {
        0
    };
    // ways 軸を持つのは r1 / r2h 系。それ以外は退化値 `1` で CSV を symmetric に。
    let ways_col = if matches!(variant, "r1" | "r2h") {
        args.ways
    } else {
        1
    };
    // partitioned のみが `--partitions` を意味的に解釈する。他 variant は退化値 `1`。
    // 集計側は `(variant, partitions)` join で (T × N) の N 軸を読む。
    let partitions_col = if variant == "partitioned" {
        args.partitions
    } else {
        1
    };
    println!(
        "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{:.4},{:.4},{:.2},{:.2},{:.4}",
        variant,
        trial,
        ways_col,
        partitions_col,
        args.source.as_str(),
        args.workload_param,
        args.op_mix.as_str(),
        args.value_kind.as_str(),
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
        "  [{}] trial {} ways={} partitions={} per-thread Mops: [{}]",
        variant,
        trial,
        ways_col,
        partitions_col,
        r.per_thread_mops
            .iter()
            .map(|m| format!("{:.3}", m))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

fn main() {
    let args = parse_args();
    let trace = load_trace(&args);

    println!(
        "variant,trial,ways,partitions,source,workload_param,op_mix,value,skew,keys,threads,cap,shards,ops,total_elapsed_ns,aggregate_mops,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv"
    );
    for variant in &args.variants {
        for trial in 0..args.trials {
            let r = match (variant.as_str(), args.value_kind) {
                ("c8", ValueKind::U64) => run_c8(&args, trace.clone()),
                ("c8", ValueKind::String) => {
                    unreachable!("parse_args rejects c8 + --value string")
                }
                ("c9", ValueKind::U64) => {
                    run_trial::<u64, ConcurrentSieveC9<u64, u64>>(&args, trace.clone())
                }
                ("c9", ValueKind::String) => {
                    run_trial::<String, ConcurrentSieveC9<u64, String>>(&args, trace.clone())
                }
                ("moka", ValueKind::U64) => {
                    run_trial::<u64, moka::sync::Cache<u64, u64>>(&args, trace.clone())
                }
                ("moka", ValueKind::String) => {
                    run_trial::<String, moka::sync::Cache<u64, String>>(&args, trace.clone())
                }
                ("mini_moka", ValueKind::U64) => {
                    run_trial::<u64, mini_moka::sync::Cache<u64, u64>>(&args, trace.clone())
                }
                ("mini_moka", ValueKind::String) => {
                    run_trial::<String, mini_moka::sync::Cache<u64, String>>(&args, trace.clone())
                }
                ("c14s", v) => run_c14s(&args, v, trace.clone()),
                ("c16s", v) => run_c16s(&args, v, trace.clone()),
                ("c17s", v) => run_c17s(&args, v, trace.clone()),
                ("c18s", v) => run_c18s(&args, v, trace.clone()),
                ("c15s_16", v) => run_c15s::<4>(&args, v, trace.clone()),
                ("c15s_8", v) => run_c15s::<3>(&args, v, trace.clone()),
                ("c15s_4", v) => run_c15s::<2>(&args, v, trace.clone()),
                ("r1", v) => run_r1(&args, v, trace.clone()),
                ("r2h", v) => run_r2h(&args, v, trace.clone()),
                ("partitioned", v) => run_partitioned(&args, v, trace.clone()),
                (other, _) => panic!("unknown variant: {other}"),
            };
            emit(variant, trial, &args, &r);
        }
    }
}

/// 実行時 `--shards` 値を c8 の const generic に dispatch する。
fn run_c8(args: &Args, trace: Option<Arc<Vec<u64>>>) -> TrialResult {
    match args.shards {
        8 => run_trial::<u64, ConcurrentSieveCache<u64, u64, 8>>(args, trace),
        16 => run_trial::<u64, ConcurrentSieveCache<u64, u64, 16>>(args, trace),
        32 => run_trial::<u64, ConcurrentSieveCache<u64, u64, 32>>(args, trace),
        64 => run_trial::<u64, ConcurrentSieveCache<u64, u64, 64>>(args, trace),
        128 => run_trial::<u64, ConcurrentSieveCache<u64, u64, 128>>(args, trace),
        256 => run_trial::<u64, ConcurrentSieveCache<u64, u64, 256>>(args, trace),
        512 => run_trial::<u64, ConcurrentSieveCache<u64, u64, 512>>(args, trace),
        n => panic!("c8 shards={n} not in supported set (assert above should have caught this)"),
    }
}

fn run_c14s(args: &Args, v: ValueKind, trace: Option<Arc<Vec<u64>>>) -> TrialResult {
    assert_eq!(args.shards, 64, "c14s requires --shards 64");
    match v {
        ValueKind::U64 => run_trial::<u64, ConcurrentSieveC14S<u64, u64, 64>>(args, trace),
        ValueKind::String => run_trial::<String, ConcurrentSieveC14S<u64, String, 64>>(args, trace),
    }
}

fn run_c16s(args: &Args, v: ValueKind, trace: Option<Arc<Vec<u64>>>) -> TrialResult {
    assert_eq!(args.shards, 64, "c16s requires --shards 64");
    match v {
        ValueKind::U64 => run_trial::<u64, ConcurrentSieveC16S<u64, u64, 64>>(args, trace),
        ValueKind::String => run_trial::<String, ConcurrentSieveC16S<u64, String, 64>>(args, trace),
    }
}

fn run_c17s(args: &Args, v: ValueKind, trace: Option<Arc<Vec<u64>>>) -> TrialResult {
    // SHARDS は const generic で arm を展開。cap-axis sweep で
    // shards = next_pow2(cap/64) (= senba::Cache auto-shard 同等) を harness 側で渡す。
    // 範囲外の SHARDS で呼ばれた場合は明確に panic させる (silent fallback はしない)。
    macro_rules! arm_c17s {
        ($s:literal) => {
            match v {
                ValueKind::U64 => run_trial::<u64, ConcurrentSieveC17S<u64, u64, $s>>(args, trace),
                ValueKind::String => {
                    run_trial::<String, ConcurrentSieveC17S<u64, String, $s>>(args, trace)
                }
            }
        };
    }
    match args.shards {
        4 => arm_c17s!(4),
        8 => arm_c17s!(8),
        16 => arm_c17s!(16),
        32 => arm_c17s!(32),
        64 => arm_c17s!(64),
        128 => arm_c17s!(128),
        256 => arm_c17s!(256),
        512 => arm_c17s!(512),
        1024 => arm_c17s!(1024),
        2048 => arm_c17s!(2048),
        4096 => arm_c17s!(4096),
        8192 => arm_c17s!(8192),
        16384 => arm_c17s!(16384),
        32768 => arm_c17s!(32768),
        65536 => arm_c17s!(65536),
        131072 => arm_c17s!(131072),
        n => panic!("c17s shards={n} not in supported set (4,8,16,32,...,131072)"),
    }
}

fn run_c18s(args: &Args, v: ValueKind, trace: Option<Arc<Vec<u64>>>) -> TrialResult {
    assert_eq!(args.shards, 64, "c18s requires --shards 64");
    match v {
        ValueKind::U64 => run_trial::<u64, ConcurrentSieveC18S<u64, u64, 64>>(args, trace),
        ValueKind::String => run_trial::<String, ConcurrentSieveC18S<u64, String, 64>>(args, trace),
    }
}

fn run_c15s<const SAMPLE_BITS: u32>(
    args: &Args,
    v: ValueKind,
    trace: Option<Arc<Vec<u64>>>,
) -> TrialResult {
    assert_eq!(args.shards, 64, "c15s_* requires --shards 64");
    match v {
        ValueKind::U64 => {
            run_trial::<u64, ConcurrentSieveC15S<u64, u64, 64, SAMPLE_BITS>>(args, trace)
        }
        ValueKind::String => {
            run_trial::<String, ConcurrentSieveC15S<u64, String, 64, SAMPLE_BITS>>(args, trace)
        }
    }
}

/// r1: shard 間 routing affinity variant。`--ways` を取り、`R1Wrapper::build_with_ways`
/// 経由で constructor に伝わる。c17s と同様に `--shards` で SHARDS を選び、
/// `next_pow2(cap/64)` を harness 側で渡す前提。`ways <= shards` の制約は
/// constructor 側で assert される。
fn run_r1(args: &Args, v: ValueKind, trace: Option<Arc<Vec<u64>>>) -> TrialResult {
    macro_rules! arm_r1 {
        ($s:literal) => {
            match v {
                ValueKind::U64 => run_trial::<u64, R1Wrapper<u64, $s>>(args, trace),
                ValueKind::String => run_trial::<String, R1Wrapper<String, $s>>(args, trace),
            }
        };
    }
    match args.shards {
        4 => arm_r1!(4),
        8 => arm_r1!(8),
        16 => arm_r1!(16),
        32 => arm_r1!(32),
        64 => arm_r1!(64),
        128 => arm_r1!(128),
        256 => arm_r1!(256),
        512 => arm_r1!(512),
        1024 => arm_r1!(1024),
        2048 => arm_r1!(2048),
        4096 => arm_r1!(4096),
        8192 => arm_r1!(8192),
        16384 => arm_r1!(16384),
        32768 => arm_r1!(32768),
        65536 => arm_r1!(65536),
        131072 => arm_r1!(131072),
        n => panic!("r1 shards={n} not in supported set (4,8,16,32,...,131072)"),
    }
}

/// r2h: r1 の hash-routed control。dispatch 構造は r1 と完全に同型 (SHARDS 軸 +
/// `--ways`)。差分は `R2hWrapper` の routing rule のみ。
fn run_r2h(args: &Args, v: ValueKind, trace: Option<Arc<Vec<u64>>>) -> TrialResult {
    macro_rules! arm_r2h {
        ($s:literal) => {
            match v {
                ValueKind::U64 => run_trial::<u64, R2hWrapper<u64, $s>>(args, trace),
                ValueKind::String => run_trial::<String, R2hWrapper<String, $s>>(args, trace),
            }
        };
    }
    match args.shards {
        4 => arm_r2h!(4),
        8 => arm_r2h!(8),
        16 => arm_r2h!(16),
        32 => arm_r2h!(32),
        64 => arm_r2h!(64),
        128 => arm_r2h!(128),
        256 => arm_r2h!(256),
        512 => arm_r2h!(512),
        1024 => arm_r2h!(1024),
        2048 => arm_r2h!(2048),
        4096 => arm_r2h!(4096),
        8192 => arm_r2h!(8192),
        16384 => arm_r2h!(16384),
        32768 => arm_r2h!(32768),
        65536 => arm_r2h!(65536),
        131072 => arm_r2h!(131072),
        n => panic!("r2h shards={n} not in supported set (4,8,16,32,...,131072)"),
    }
}

/// partitioned: `senba::concurrent::PartitionedCache` を `--partitions N` 経由で
/// 構築。`run_trial_with` の inner harness をそのまま使うため、cache 構築だけ
/// 直接行い `Arc<PartitionedWrapper>` を inner に渡す。
///
/// `--partitions` は **`--threads` と独立な軸** として sweep する (設計
/// `docs/reports/2026-05-12-partitioned-design.md` §(T × N) sweep)。
fn run_partitioned(args: &Args, v: ValueKind, trace: Option<Arc<Vec<u64>>>) -> TrialResult {
    assert!(
        args.cap >= args.partitions,
        "partitioned: --cap ({}) must be >= --partitions ({})",
        args.cap,
        args.partitions
    );
    match v {
        ValueKind::U64 => {
            let cache = Arc::new(PartitionedWrapper::<u64>::new(args.cap, args.partitions));
            run_trial_with::<u64, PartitionedWrapper<u64>>(cache, args, trace)
        }
        ValueKind::String => {
            let cache = Arc::new(PartitionedWrapper::<String>::new(args.cap, args.partitions));
            run_trial_with::<String, PartitionedWrapper<String>>(cache, args, trace)
        }
    }
}
