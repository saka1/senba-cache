# 2026-05-08 sieve_c9 — per-shard `Mutex<Shard>` 並行 SIEVE: 設計と比較計画

## 目的とスコープ

`sieve_c8` (`K, V: Copy` + lock-free seqlock-via-tag、`docs/reports/2026-05-06-c8-design.md`)
は moka 0.12 比 5-15× scaling を実機計測済みの並行 SIEVE バージョンだが、
**`Copy` 制約**ゆえに `String` / `Vec` / `Box<T>` を value に取れず、実 cache use case
の射程が極めて狭い。crates.io に publishable な並行 cache を出すには、業界主流に
整合する V: Clone API が必要。

主要外部実装の API 形を `docs/api-comparison.md` の前提情報に加えて確認すると:

| ライブラリ | V 制約 | get() 戻り値 | 内部 |
|---|---|---|---|
| moka 0.12 (`sync::Cache`) | `V: Clone + Send + Sync + 'static` | `Option<V>` (clone) | `MiniArc<ValueEntry<K, V>>` を `cht::SegmentedHashMap` に格納 |
| mini-moka 0.10 | 同上 | `Option<V>` (clone) | 同様 |
| quick_cache | `Val: Clone` | `Option<Val>` (clone) | "wrap with `Arc<_>` if expensive" — Arc 化はユーザー責任 |
| stretto | `V: Send + Sync + 'static` | `Option<ValueRef<'_>>` (RAII guard) | guard 寿命で参照を返す |
| dashmap | (V 制約最小) | `Option<Ref<'a>>` (RAII guard) | per-shard `RwLock`、guard で参照を返す |
| jedisct1 sieve-cache `ShardedSieveCache` | `K: Eq + Hash + Clone + Send + Sync, V: Send + Sync` (get で `V: Clone`) | `Option<V>` (clone) | per-shard `Mutex<SieveCache>` |
| **c8 (現状 / 本 repo experimental)** | `K, V: Copy` | `Option<V>` (bitwise copy) | seqlock-via-tag、特殊 |

**業界主流は明らかに「`V: Clone` + `get` は `Option<V>` を clone で返す」**。重い V は
ユーザーが `Arc<V>` 化して使う規約 (quick_cache の `with_options` doc に明文)。
c8 の Copy-only は seqlock-via-tag が要求した実装都合であり、業界の API 形からは外れている。

正式版 `senba::concurrent::Cache` は `V: Clone` + `Option<V>` 路線で出すことが望ましい。
ただし「per-shard `Mutex` で wrap した SIEVE は、構造的にロックを持たない c8 の scaling
とどこまで張り合えるか / 1T overhead で勝てるか」は brainstorming で解決できない経験的な
問題で、ベンチで潰すしかない。

本 spec はこれを 2 phase に分解する:

| Phase | 内容 | 本 spec での扱い |
|---|---|---|
| **P1** | `sieve_c9` を `research/src/experimental/` にスタンドアロン実装。最新の `senba::Cache` (j8 + shift-on-evict + AlignedTags) を直列版下敷きにし、`Box<[Mutex<Shard>]>` で wrap する | **コード設計まで本 spec で確定** |
| **P2** | `bench_concurrent` に c9 を統合し、c8 / c9 / moka / mini-moka を thread × cap × skew × shards × op-mix で sweep。レポート 1 本 (`2026-05-08-c8-vs-c9-thread-sweep.md`) | **比較計画と harness 拡張内容を本 spec で確定。実行は実装後** |

正式版 `senba::concurrent::Cache` への昇格判定 (= P3) は P2 結果に強く依存するため、
**現段階ではスコープアウト**。P2 を回した後に別 design doc で起案する。

## c9 アーキテクチャ

`research/src/experimental/sieve_c9.rs` に**スタンドアロン**実装。コードは
`senba::Cache` の最新版 (`src/lib.rs` + `src/shard.rs`、shift-on-evict + AlignedTags +
SlotSize) を c9 内に複製する。research 側でのコード重複は許容方針 (CLAUDE.md
"Adding a new SIEVE variant" の通り)。

```rust
pub struct ConcurrentSieveCache<K, V, const SHARDS: usize = DEFAULT_SHARDS> {
    shards: Box<[Mutex<Shard<K, V>>]>,   // parking_lot::Mutex
    hasher: Xxh3Build,                    // 固定 (experimental)
    has_avx2_bmi1: bool,                  // 起動時 detect、Shard::find_avx2 dispatch 用
}

struct Shard<K, V> {
    capacity: usize,
    tags: AlignedTags,                    // senba::Cache の AlignedTags 同形
    entries: Vec<MaybeUninit<Entry<K, V>>>, // shift-on-evict、stride = sizeof(Entry)
    hand: usize,
    len: usize,
    hits: u64, misses: u64,
    insertions: u64, evictions: u64,      // Mutex 配下なので plain u64 で十分
}
```

### 簡素化方針

c9 は research artifact なので、senba::Cache の publishable surface が持っていた
抽象は最小限に削る:

- **SlotSize 抽象は持たない** (固定 stride = `sizeof(Entry<K, V>)`)。bench 主軸は
  `<u64, u64>` で SlotSize の意味が薄い。最終格上げ時 (P3) に SlotSize を戻す
- **Hasher は generic にしない** (`Xxh3Build` 固定)。最終格上げ時に `with_hasher` を戻す
- **SHARDS は const generic** (c8 と同形)。`bench_concurrent` の既存 dispatch がそのまま使える

### 並列性モデル

writer / reader 両方 `Mutex<Shard>` 配下で動く:

- Mutex は `parking_lot::Mutex` (uncontended ~5-10 ns、poisoning なし、c8 と合わせる)
- `get` は Mutex 配下で senba::Cache::get と同じ「SIMD scan + VISITED bit set」を
  そのまま実行し、最後に `V::clone()` して `Option<V>` を返す
- `insert` は senba::Cache::insert と同じ shift-on-evict ロジックを Mutex 配下で実行
- atomic / seqlock dance / UnsafeCell は一切登場しない。**c8 の formal correctness
  ギャップ (Rust 抽象機械上の data race UB) は構造的に存在しない** — miri が回る
- 並列性は SHARDS で決まる。hot key が乗る 1 shard だけ直列化、残り SHARDS-1 は無競合

## API (最小 set)

`bench_concurrent` の `ConcCache` trait が要求する最低限 + HR 計測用の `stats`。

```rust
impl<K, V, const SHARDS: usize> ConcurrentSieveCache<K, V, SHARDS>
where
    K: Hash + Eq + Send + Sync,
    V: Send + Sync,
{
    pub fn new(capacity: usize) -> Self;
    pub fn capacity(&self) -> usize;
    pub fn len(&self) -> usize;             // 各 shard の Mutex を 1 回ずつ取って合計
    pub fn is_empty(&self) -> bool;
    pub fn stats(&self) -> Stats;           // bench 終了時のみ呼ぶ前提

    pub fn contains_key(&self, key: &K) -> bool;
    pub fn get(&self, key: &K) -> Option<V> where V: Clone;
    pub fn insert(&self, key: K, value: V) -> Option<(K, V)>;
}
```

### 省略する API (= 非 scope)

- `clear` / `remove` / `iter` / `iter_mut` / `keys` / `values` / `drain`
- `peek` / `peek_mut` / `peek_key_value` / `get_key_value`
- `get_mut` / `get_or_insert_with`
- `retain` / `Extend` / `IntoIterator` / `Clone` / `Debug`
- `with_hasher` / `with_shards` (const generic で置換)

これらは **bench harness で叩かない、c8 にも存在しない、最小 API 路線**。最終格上げ
時 (P3) に senba::Cache のフル surface に揃える。

## shard 内部のプロトコル

senba::Cache (ST) の shift-on-evict をそのまま再現する。Mutex 配下なので並行特有の
工夫は一切不要。具体的には senba::Cache の `Shard::get` / `Shard::insert` のロジックを
**`&self` で Mutex 取得して内部の `&mut` 化に切り替える**だけ:

```rust
impl<K, V, const SHARDS: usize> ConcurrentSieveCache<K, V, SHARDS>
where K: Hash + Eq + Send + Sync, V: Send + Sync,
{
    pub fn get(&self, key: &K) -> Option<V> where V: Clone {
        let h = self.hasher.hash_one(key);
        let mut sh = self.shards[Self::shard_of_hash(h)].lock();
        sh.get(key, h, self.has_avx2_bmi1).cloned()
    }

    pub fn insert(&self, key: K, value: V) -> Option<(K, V)> {
        let h = self.hasher.hash_one(&key);
        let mut sh = self.shards[Self::shard_of_hash(h)].lock();
        sh.insert(key, value, h, self.has_avx2_bmi1)
    }
}
```

`Shard::{get, insert, contains, ...}` の中身は senba::Cache の Shard をそのまま
コピーして持ち込む。c8 のように `AtomicU16` / `UnsafeCell` / seqlock dance は登場しない。
**ただの ST 実装を Mutex で wrap した形**。

### c8 との実装比較

| | c8 | c9 |
|---|---|---|
| 値型 | `K, V: Copy` | `K: Hash+Eq+Send+Sync, V: Send+Sync` (get で `V: Clone`) |
| 直列版下敷き | j8 (tail/compact あり) | senba::Cache (shift-on-evict、tail/compact なし) |
| read path | lock-free seqlock-via-tag | Mutex 配下 ST get |
| write path | per-shard Mutex (writer のみ) | per-shard Mutex (read/write 共用) |
| tag 表現 | `Box<[AtomicU16]>` | `AlignedTags` (= `Vec<TagsChunk>`) |
| entries 表現 | `UnsafeCell<Box<[MaybeUninit<Entry>]>>` | `Vec<MaybeUninit<Entry>>` |
| compact 操作 | あり (tail が order_cap に到達したとき) | **なし** (shift-on-evict なので I4' で常に hole なし) |
| 形式的健全性 | 抽象機械上 data race UB (実機 x86_64 で well-defined を主張) | 完全に well-defined |
| miri | 並列 test を `#[cfg(not(miri))]` で抑制 | 全 test pass 期待 |
| SIMD scan | best-effort filter + 候補ごと seqlock dance | senba::Cache::find_avx2 そのまま |

## bench_concurrent への c9 統合

既存 `research/src/bin/bench_concurrent.rs` の `ConcCache` trait に c9 を追加する。
c8 と同じ const-generic SHARDS 解決 (`match args.shards`) パターンを踏襲:

```rust
impl<const S: usize> ConcCache for ConcurrentSieveCache<u64, u64, S> {  // c9 側の型
    fn build(capacity: usize) -> Arc<Self> { Arc::new(ConcurrentSieveCache::new(capacity)) }
    fn get_hit(&self, key: &u64) -> bool { self.get(key).is_some() }
    fn insert(&self, key: u64, value: u64) { let _ = self.insert(key, value); }
}
```

CLI の `--variant` フラグに `c9` を追加。既存 c8 / moka / mini-moka と完全同 harness で叩ける。

## 比較計画 (P2)

### sweep 軸

`docs/reports/2026-05-06-c8-vs-moka-thread-sweep.md` §拡張を母体にし、c9 を
4 番目の variant として追加。memory-fair な比較条件を維持:

| 軸 | 値 |
|---|---|
| variants | `c8`, `c9`, `moka`, `mini_moka` |
| threads | 1, 2, 4, 8, 16 |
| skew | **0.7, 1.0, 1.2** (現行 1.0 のみから 3 点に拡張、Zipf-low/mid/high で挙動分離) |
| cap | 16384 |
| shards (c8/c9) | 256 (per_shard=64) |
| keys | 1,000,000 |
| ops | 4,000,000 (per thread) |
| warmup | 200,000 |
| trials | 3 |
| seed | 42 |

variant × threads × skew = 4 × 5 × 3 = 60 cells / op-mix。

### op-mix 拡張

現行 `bench_concurrent` は GIM (= miss なら insert) のみ。c9 と c8 の差を見るには
**read-heavy ワークロード** を追加したい (SIEVE の read 純粋経路が一番効く軸):

- CLI フラグ `--op-mix {gim, read-heavy}` を追加
- `read-heavy`: 95% get / 5% insert、insert は別の Zipf draw を使う
  (cache を汚さないように seed をずらす)
- gim と read-heavy の両方で sweep を回し、別 csv に分けて出す

### メトリクス (現行を維持)

aggregate Mops/s、p50 / p99 chunk latency (CHUNK_OPS=1024)、thread CV、hit ratio。

### 想定する 1T 性能予測

- `senba::Cache::get` の 1T 性能 (約 30 ns/op @ Slot32 + Zipf 1.0) + parking_lot
  uncontended Mutex (~5-10 ns) + V::clone (`<u64, u64>` で 0 ns) ≈ **40-50 ns/op**
- c8 の 1T overhead は seqlock dance + AtomicU16 fetch_or + parking_lot lock の合計で
  ~92 ns/op (`docs/reports/2026-05-06-c8-design.md` §第一手 smoke 表より)
- **予測: 1T では c9 が c8 より速い**。c8 の lock-free read は read 自体は速いが
  seqlock dance のオーバーヘッドが Mutex acquire を上回る可能性が高い

scaling の予測は不確定 (= 計測動機そのもの):

- c9 は SHARDS=256 で hot key が 1 shard を占有するだけで残り 255 shard は無競合
  → c8 と同じ scaling pattern が出る可能性
- ただし「reader も Mutex を取る」分、hot shard の writer 直列が c8 より長く詰まる
  リスクがある (c8 は reader が無競合で同 shard を読み続けられる)
- read-heavy で c8 と c9 の差が一番大きく出ると予想

### レポート骨格

`docs/reports/2026-05-08-c8-vs-c9-thread-sweep.md`:

```
# c8 vs c9 vs moka — concurrent thread sweep
## TL;DR (1T overhead と 16T scaling の数値で結論)
## Setup (cmd / csv paths / hardware)
## §1 Throughput (gim) — 4 variant × 5 thread × 3 skew
## §2 Throughput (read-heavy) — Mutex contention の代理を見る軸
## §3 Tail latency (p99 が thread 数で線形か / 飽和点)
## §4 HR 一致確認 (c8 / c9 が同 HR を出していることを確認、moka とのオフセット説明)
## §5 解釈 (c9 の Mutex overhead は c8 の seqlock dance に勝てたか / scaling の壁の所在)
## §6 後続候補 (P3 昇格判定 / 別路線 c10 探索)
```

raw csv は `docs/reports/data/2026-05-08-c8-vs-c9-thread-sweep.csv` に置く
(既存 `data/2026-05-06-c8-vs-moka-realistic-cap.csv` と同じ形式)。

## テスト戦略

### 単体テスト (c9 内)

senba::Cache の test を mirror。`cache_initially_empty`、`insert_then_get`、
`evicts_oldest_when_full_and_unvisited`、`visited_entry_survives_first_pass` 等の
SIEVE 不変条件 ~20 件 (c8 のテスト群とほぼ同集合)。

### oracle 一致 (`research/tests/oracle.rs`)

1 shard 同期で c9 の eviction sequence が `sieve_orig` と bit-exact 一致する
(shift-on-evict なので senba::Cache と同じ性質を継承)。同形のテスト
`oracle_cache_match.rs` も追加する。

### 並列 invariants (`#[cfg(not(miri))]`)

c8 と同形のテスト: N thread で Zipf を流して終了後の不変条件のみ検証:

- I-conc-1: 全体 `len <= cap`
- I-conc-2: 各 shard で LIVE tag 数 == `shard.len`
- I-conc-3: 各 shard で live id 集合に重複なし
- I-conc-4: hit する key の value は key と一致

データ競合が Mutex で構造的に排除されるので、c8 のような phantom tag バグは
原理的に出ない。

### miri

c8 と違って **全 test が miri pass する**見込み (UnsafeCell / atomic raw read なし、
`Mutex<Shard>` + plain field のみ)。これは c9 の構造的優位として明記する。

## 実装順序の目安

1. `research/src/experimental/sieve_c9.rs` 新規作成 (senba::Cache::Shard を複製、Mutex で wrap)
2. `research/src/experimental/mod.rs` に登録
3. 単体テスト (senba::Cache の test を mirror)
4. `research/tests/oracle.rs` / `oracle_cache_match.rs` に c9 ケース追加
5. `research/src/bin/bench_concurrent.rs` に c9 variant を追加 (`--variant c9`)
6. `bench_concurrent` に `--op-mix {gim, read-heavy}` フラグ追加
7. P2 sweep を実行 (4 variant × 5 thread × 3 skew × 2 op-mix = 120 cells)
8. レポート `2026-05-08-c8-vs-c9-thread-sweep.md` 起草
9. `docs/reports/index.md` 更新

## 非 scope (= 本 spec で扱わない)

- 正式版 `senba::concurrent::Cache` への格上げ (= P3、P2 結果に強く依存するため別 spec)
- SlotSize 抽象の c9 導入
- Hasher generic 化 (`Xxh3Build` 固定で十分)
- 省略 API (`clear` / `remove` / `iter` / `peek` 系等) の追加
- async 版
- Drop listener / removal cause 通知
- jemalloc / mimalloc 等 allocator の影響評価 (別軸)
- TTL / weight-based eviction (SIEVE の射程外)

## 依存追加

なし。c9 は parking_lot を既存 c8 と共有。`bench_concurrent` の `--op-mix` フラグ
追加は既存 ZipfGen のみで完結する。
