# SIEVE 比較ハーネス設計

- 作成日: 2026-05-03
- 対象: `senba-cache` プロジェクトに、複数 SIEVE 実装を **正しさ** と **性能** の両面で比較するためのハーネス基盤を導入する。

## 背景と目的

`senba-cache` は SIEVE (NSDI'24) の Rust 実装を複数 variant 育てて比較するサンドボックス。現状は次の 2 実装がある:

- `src/sieve_orig.rs` — 著者参照実装の忠実な Rust ポート (oracle)
- `src/sieve_v0.rs` — 連結リストを使わず、配列 + tombstone + 周期 compaction で組んだ最初の実験 variant

`CLAUDE.md` の要件「新 variant の正しさの基準は `sieve_orig` と evict されたキー列が完全一致すること」を automated に検証する仕組みが無い。性能比較の基盤も無い。本仕様はそれを埋める。

優先順位: **(1) 正しさ → (2) 性能**。両方をカバーするが、最初に通すのは正しさ。

## スコープ

含む:

- 全 variant が満たす最小限の `Cache` trait の定義
- ワークロード抽象 (`Trace = Iterator<Item = Key>`) と 2 つのソース (合成 Zipf, 同梱トレースファイル)
- `tests/oracle.rs` — `sieve_orig` を oracle として variant の動作一致を assert する差分テスト
- `benches/micro.rs` — Criterion による μs/op マイクロベンチ
- `src/bin/bench.rs` — 1 トレースを各 variant に流して CSV を吐く CLI

含まない (将来やる):

- メモリ使用量の自動計測 (RSS/dhat)
- 著者リポジトリと共通の `oracle.zst` 等のトレース形式対応
- 実 ICache/CDN トレース (Twitter, Wikipedia 等) のローダ
- get/insert を混ぜたシナリオの正しさハーネス (まずは insert-only)

## 既存ファイルの扱い

`src/cache.rs` (placeholder の `Cache` trait) と `src/error.rs` は impl 利用者が無い throwaway。本仕様で:

- `src/cache.rs` を新仕様の `Cache` trait に書き換える
- `src/error.rs` を削除する (新 trait は `Result` を返さない)

## アーキテクチャ

```
src/
  lib.rs              # モジュール宣言と再エクスポート (workload を追加, error を削除)
  cache.rs            # 新: Cache trait (SIEVE-shaped, 最小限)
  sieve_orig.rs       # 既存。Cache を実装する
  sieve_v0.rs         # 既存。Cache を実装する
  workload/
    mod.rs            # Key 型と Trace 抽象
    zipf.rs           # 合成 Zipf 生成器
    file.rs           # 1行1整数のテキストファイルから読む
  bin/
    bench.rs          # CLI ベンチランナー

tests/
  oracle.rs           # 正しさハーネス

benches/
  micro.rs            # Criterion ベンチ
```

3 つのハーネス (oracle / criterion / CLI) すべてが、同じ `Cache` trait + 同じ `workload::Trace` の上で動くので、共通ロジックは「trace を流して何かする driver」のみ。

## コンポーネント詳細

### `Cache` trait

`src/cache.rs` を以下で置き換える。

```rust
pub trait Cache<K, V> {
    fn new(capacity: usize) -> Self where Self: Sized;
    fn capacity(&self) -> usize;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool { self.len() == 0 }

    /// hit 時に visited bit を立てるため &mut self
    fn get(&mut self, key: &K) -> Option<&V>;

    /// 容量超過時に追い出された (K,V) を返す。oracle 比較の主データ
    fn insert(&mut self, key: K, value: V) -> Option<(K, V)>;

    fn contains_key(&self, key: &K) -> bool;
}
```

意図:

- `new` を trait に入れて `C::new(cap)` でジェネリック driver から生成可能にする
- `Result` は使わない。SIEVE の API は `Option` で過不足無い
- `remove` は trait からは外す。`sieve_orig` 固有メソッドとして残し、ハーネス対象外
- `clear` も外す。SIEVE のコアでなくテストでも使っていない

`sieve_orig::SieveCache` と `sieve_v0::SieveCache` の両方に `impl Cache<K, V> for SieveCache<K, V>` を追加する。既存の inherent メソッド名と完全一致しているので、trait impl は対応する inherent メソッドに委譲するだけ。

注: この trait は SIEVE 都合で決めている。LRU 等の他アルゴリズムが入った時には書き換える可能性があり、以降の修正コストは低いので問題視しない。

### `workload` モジュール

```rust
// src/workload/mod.rs
pub type Key = u64;

pub trait Trace: Iterator<Item = Key> {}
impl<T: Iterator<Item = Key>> Trace for T {}

pub mod zipf;
pub mod file;
```

`u64` 固定 (NSDI 同梱トレースが整数キー、Zipf 生成も整数、ジェネリックにする利用者が居ない)。

#### `workload::zipf`

```rust
pub struct ZipfGen { /* StdRng + Zipf */ }

impl ZipfGen {
    pub fn new(skew: f64, n_keys: u64, seed: u64) -> Self;
}

impl Iterator for ZipfGen { type Item = Key; ... }
```

`rand` + `rand_distr` を `[dev-dependencies]` に追加。seed 固定で完全再現可能。skew は `>1.0` (`rand_distr::Zipf` 仕様) を期待し、入力チェックは debug_assert! 程度に抑える (sandbox 用途)。

#### `workload::file`

```rust
pub fn from_path(path: impl AsRef<Path>) -> io::Result<impl Iterator<Item = Key>>;
```

`BufReader::lines()` を `parse::<u64>` するだけ。bundled `external/NSDI24-SIEVE/mydata/zipf/zipf_1.0` (1M 行・整数1列) 用。パース失敗は `expect`。

### 正しさハーネス: `tests/oracle.rs`

差分テスト 1 ファイル。同じトレースを `sieve_orig` と他 variant に流して、各 `insert` が返す evicted を比較する。

```rust
fn run<C: Cache<u64, u64>>(trace: impl Iterator<Item = u64>, cap: usize)
    -> Vec<Option<(u64, u64)>>
{
    let mut c = C::new(cap);
    trace.map(|k| c.insert(k, k)).collect()
}

#[test]
fn v0_matches_orig_on_zipf() {
    for &(skew, cap) in &[(1.05, 64), (1.1, 128), (1.2, 256), (1.5, 1024)] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let orig = run::<sieve_orig::SieveCache<_, _>>(trace_a, cap);
        let v0   = run::<sieve_v0::SieveCache<_, _>>(trace_b, cap);
        assert_eq!(orig, v0, "mismatch at skew={skew} cap={cap}");
    }
}

#[test]
fn v0_matches_orig_on_bundled_zipf() {
    let path = "external/NSDI24-SIEVE/mydata/zipf/zipf_1.0";
    for &cap in &[256, 1024, 4096] {
        let trace_a = workload::file::from_path(path).unwrap().take(100_000);
        let trace_b = workload::file::from_path(path).unwrap().take(100_000);
        let orig = run::<sieve_orig::SieveCache<_, _>>(trace_a, cap);
        let v0   = run::<sieve_v0::SieveCache<_, _>>(trace_b, cap);
        assert_eq!(orig, v0, "mismatch at cap={cap}");
    }
}
```

注:

- `Vec<Option<(K,V)>>` 全体を集めて `assert_eq!`。最初の不一致 index で問題箇所が分かる
- 取り扱い量が多すぎないよう bundled は 100k に絞る (1M req 全部は重い)
- get を混ぜたシナリオ (50/50, 80/20 hit) は将来追加。最初は insert-only

### 性能ハーネス 1: Criterion (`benches/micro.rs`)

μs/op のマイクロ性能。実装 × ワークロード × capacity をスイープ。

```rust
fn bench_insert_only(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_only");
    for skew in &[1.05, 1.1, 1.2] {
        for cap in &[1024usize, 8192, 65536] {
            let trace: Vec<u64> = ZipfGen::new(*skew, 100_000, 42)
                .take(200_000).collect();
            group.throughput(Throughput::Elements(trace.len() as u64));

            group.bench_with_input(
                BenchmarkId::new(format!("orig/skew{skew}"), cap),
                &(*cap, &trace),
                |b, (cap, trace)| b.iter_batched(
                    || sieve_orig::SieveCache::<u64, u64>::new(*cap),
                    |mut c| for &k in *trace {
                        c.insert(black_box(k), k);
                    },
                    BatchSize::LargeInput,
                ),
            );
            // 同様に sieve_v0 も
        }
    }
}

fn bench_mixed(c: &mut Criterion) { /* 80% get, 20% insert */ }

criterion_group!(benches, bench_insert_only, bench_mixed);
criterion_main!(benches);
```

設計判断:

- Trace は事前に `Vec` 展開してから `iter()`。Zipf 生成と RNG ノイズを計測対象から除外
- `iter_batched` で「キャッシュ生成は計測外、ループだけ計測」。eviction 経路は容量到達後に出るので、毎回新規構築する
- 2 種: `insert_only` (eviction 経路集中) と `mixed` (実用に近い 80% read)
  - `mixed` は 1 本の Zipf trace から事前に op 列を生成: 各キー `k` に対し RNG(同 seed) で 80% の確率で `get(&k)`、それ以外で `insert(k, k)`。事前に `Vec<(Op, Key)>` に展開して RNG コストを計測外にする
- `Throughput::Elements` で Criterion に ops/sec を出させる
- `cargo bench -- insert_only/orig` のような名前で絞れる構造にする

`criterion` を `[dev-dependencies]` に追加。`Cargo.toml` の `[[bench]]` セクションで `harness = false` 指定。

将来 (今回はやらない): `peakmem` や `dhat` での alloc 数計測。Criterion とは別ベンチに分ける。

### 性能ハーネス 2: CLI (`src/bin/bench.rs`)

エンドツーエンド: 1 トレースを各 variant に流して CSV を stdout に吐く。後段で `awk`/`pandas` で集計しやすい。

呼び出し例:

```
$ cargo run --release --bin bench -- \
    --source zipf --skew 1.1 --keys 100000 --len 1000000 --seed 42 \
    --capacity 1024,4096,16384 \
    --variant orig,v0
variant,source,skew,keys,len,capacity,elapsed_ns,hits,misses,evictions
orig,zipf,1.1,100000,1000000,1024,123456789,734521,265479,264455
v0,zipf,1.1,100000,1000000,1024,134567890,734521,265479,264455
...

$ cargo run --release --bin bench -- \
    --source file --path external/NSDI24-SIEVE/mydata/zipf/zipf_1.0 \
    --capacity 4096 --variant orig,v0
```

実装方針:

- 引数パースは `std::env::args` を手で。`--key value` のシンプル形式。CLI 依存を増やさない
- driver は trait ジェネリック関数:

  ```rust
  fn drive<C: Cache<u64, u64>>(trace: &[u64], cap: usize) -> Stats {
      let mut c = C::new(cap);
      let mut hits = 0u64;
      let mut evictions = 0u64;
      let t0 = Instant::now();
      for &k in trace {
          if c.get(&k).is_some() { hits += 1; }
          else if c.insert(k, k).is_some() { evictions += 1; }
      }
      Stats { elapsed: t0.elapsed(), hits, evictions, total: trace.len() as u64 }
  }
  ```
- `--variant` はカンマ区切りで指定し、文字列マッチで dispatch。trait オブジェクトは使わない (静的 dispatch を維持)
- `peak_resident` (RSS) は今回入れない。OS 依存が散らかる
- 出力は CSV のみ。`>` でファイルにリダイレクトすれば蓄積できる

## 依存関係の追加

`Cargo.toml`:

```toml
[dependencies]
rand = "0.8"
rand_distr = "0.4"

[dev-dependencies]
criterion = { version = "0.5", default-features = false }

[[bench]]
name = "micro"
harness = false

[[bin]]
name = "bench"
path = "src/bin/bench.rs"
```

`rand`/`rand_distr` は `workload::zipf` がライブラリ本体から使うので `[dependencies]`。CLI バイナリも同じワークロードを使うため。

## 検証

仕様完成の判定:

- `cargo test` が pass (`tests/oracle.rs` の 2 テスト + 既存の単体テスト)
- `cargo bench --no-run` が compile pass
- `cargo run --release --bin bench -- --source zipf --skew 1.1 --keys 1000 --len 10000 --capacity 256 --variant orig,v0` が CSV を 2 行 (+ヘッダ) 出力する
- bundled トレース上で `orig` と `v0` の hits/evictions が完全一致する (CLI 出力で目視確認)
