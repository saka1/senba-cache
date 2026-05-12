# `senba::concurrent::PartitionedCache` 設計

## 仮説

c-series / r-series の単スレ ceiling は `2026-05-12-mt-overhead-vs-lib.md`
で **lib 比 4–5x (read-dominant) ~ 15.7x (miss-heavy) の ns/op overhead** と定量化済で、
T=16 ピーク 159 Mops は ceiling 推定 640 Mops の 25–27% に留まる。残り 3–4x は
reader fast path で touch する **`path_c_epoch` / `AlignedTags` / `visited` の Acquire
load 3 本累積 (~3–5 ns × 3)** が memory-order の構造的下限に近い。

仮説: **`senba::Cache` を N 個並べて thread-id でルーティング**するだけで、
各スレッドは ST lib そのものを叩く構造になり、reader fast path の Acquire load
累積を構造的に **0** にできる。HR penalty (= 同一 key の最大 N 倍 duplicate) は
workload class 依存だが、c-series が払っている同期コスト (workload に依らず一律)
と裏返しのトレードオフになる。`r1` の WAYS=SHARDS 極限と本質同形だが、内部に
SHARDS=64 を持つ `senba::Cache` をそのまま並べる方が実装が圧倒的に単純で、
**lib の単スレ性能をそのまま T 倍積めるか** を直接ベンチマークできる baseline。

成果物は (T × N) sweep の throughput vs HR の Pareto 曲線 / 領域マップ。
c17s / r1 / moka / mini-moka との同 sweep で **partitioned が支配する cell の割合**
を可視化することが本企画の目的。

## なぜ lib (publishable surface) に置くか

1. **ベースラインは長生きする**。c-series / r-series は実験変種だが、
   PartitionedCache は「N 個並べて lock を取る」という業界標準パターン
   (dashmap / Java 5–7 ConcurrentHashMap / Folly StripedMap) の SIEVE 適用で、
   後続変種が比較される基準としてずっと残る。
2. **API surface が極小**。lib 既存の `senba::Cache` の thin wrapper にしかならず、
   並行版独自の semantic (seqlock / lock-free protocol / V: Copy 制約) は持ち込まない。
3. **downstream user に直接価値がある**: scan-heavy / per-thread workload では
   c-series より HR / throughput 両面で勝つ可能性が高く (r1-results の cluster019 等を
   参照)、moka / mini-moka に代わる選択肢として library 価値が独立にある。
4. dep を増やさない: `std::sync::Mutex` のみ使用、senba の既存 dep (`xxhash-rust`) を
   除き新規追加ゼロ。

`senba::Cache` の `&mut self` API は wrap 不能 (interior mutability 必須) のため、
`PartitionedCache` は新しい `concurrent` module 配下に独立した型として置く。
publishable な並行 API として初の登場となる。

## API スケッチ

```rust
// senba::concurrent
pub struct PartitionedCache<K, V, S: SlotSize = Slot32, H: BuildHasher = Xxh3Build> {
    partitions: Box<[Mutex<Cache<K, V, S, H>>]>,
    partition_mask: usize, // partitions.len() - 1 (must be power of two)
}

impl<K, V, S, H> PartitionedCache<K, V, S, H>
where
    K: Hash + Eq,
    V: Clone,
    S: SlotSize,
    H: BuildHasher + Clone + Default,
{
    pub fn new(capacity: usize, partitions: usize) -> Self;
    pub fn with_hasher(capacity: usize, partitions: usize, hasher: H) -> Self;

    pub fn capacity(&self) -> usize;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn partitions(&self) -> usize;

    pub fn get(&self, key: &K) -> Option<V>;
    pub fn insert(&self, key: K, value: V) -> Option<(K, V)>;
    pub fn contains_key(&self, key: &K) -> bool;
    pub fn remove(&self, key: &K) -> Option<V>;
}
```

返り型が `Option<V>` (`V: Clone`) で、`&mut self` ではなく `&self` ベース。
これは concurrent variants と整合し、`Arc<PartitionedCache>` で fan-out できる。

## Routing — thread-id ベース

```rust
#[inline]
fn partition_of(&self) -> usize {
    (current_tls_id() as usize) & self.partition_mask
}
```

- **hash には依存しない**。同一 thread からの全 access は同一 partition に行く。
- 異 thread から同一 key を触ると、それぞれの partition に独立 copy が入る
  (= HR penalty の正体)。
- `current_tls_id()` は senba 内部で TLS counter + `AtomicU32::fetch_add` で
  払い出す `u32`、`r1` で確立済みのパターン (research の `tls_id.rs` を lib に複製)。

代替案として **hash ベース routing** (`partition_of(hash) = hash & mask`) も
検討したが、ナイーブ実装では (i) 同一 key の HR 等価性が保たれる代わりに
(ii) writer Mutex が key-affinity 由来で hot shard に集中し c-series の問題が
そっくり再現する。本企画では **thread-id ベース一択**で割り切る。
hash 形式は別 variant として将来検討の余地あり (本書スコープ外、`PartitionedCacheH` 等)。

## 構造的特性

| 観点 | c17s | r1 | partitioned (本書) |
|---|---|---|---|
| Routing 引数 | `hash` 1 個 | `(hash, tls)` 2 個 | `tls` 1 個 |
| 同期コスト | 大 (`Mutex` + seqlock + epoch) | 大 (c17s 同) | **小 (`Mutex` のみ)** |
| HR 等価性 | 完全保持 | way 数まで部分 duplicate | 最悪 N 倍 duplicate |
| effective cap (per thread) | total_cap | total_cap / WAYS | total_cap / N |
| 内部 SIMD scan | あり (`AVX2`) | あり (`AVX2`) | あり (lib の SIMD scan を継承) |
| Mutex 粒度 | per-shard (= 64 個) | per-shard (= 64 個) | per-partition (= N 個) |

c17s の 64 shard × per-shard Mutex に対し、partitioned は N partition × per-partition
Mutex。N << 64 なら **Mutex 粒度は粗くなる**が、各 partition の内部は ST lib そのもの
なので「Mutex を取った後の hot path が 4–15x 速い」効果が勝るかを測る。

## 期待挙動の事前モデル

ST lib の単スレ 1 op = ~25 ns (mt-overhead-vs-lib §4 の lib base) を前提に、
T 個の thread が異なる partition を叩く場合 (= **N ≥ T かつ uncontended**):

- Mutex acquire (uncontended `std::sync::Mutex` ~10 ns) + `senba::Cache::insert/get`
  (~25 ns) + clone (V=u64 で ~0、V=String で ~10 ns) ≈ **35 ns/op**。
- T=16 で `16 * 1e9 / 35 ≈ 457 Mops` 上限。c17s の 159 Mops の **2.9×**。
- N < T の領域では Mutex 競合 + `senba::Cache::get` の `&mut self` 直列化で
  c17s より明確に劣後する見込み (確認したい contrast)。

HR は workload 依存:

- **per-thread keyspace (= 各 thread が独自 zipf seed で draw)**: 同一 key の
  thread 跨ぎ 0 → duplicate 0 → HR 完全保持。
- **shared hot key**: top-100 hot key が thread 跨ぎで共有 → N 個の partition すべてに
  duplicate → effective cap 縮小。ARC OLTP / Twitter cluster006 で大きく出る見込み
  (r1-results §で cluster006 が WAYS=4 で HR -28pp の前例あり、partitioned は更に
  悪化する側に振れる)。

## 実装計画

### `src/concurrent.rs` (新設、~150 行)

1. `pub struct PartitionedCache<K, V, S, H>`
2. `fn new(capacity, partitions)`:
   - assert `partitions.is_power_of_two()` && `partitions >= 1`
   - assert `capacity >= partitions` (各 partition cap >= 1)
   - 各 partition に `capacity / partitions + (i < extra)` を配る (lib の
     `with_shards` と同じ均等化ロジック)
3. `fn current_tls_id() -> u32` を private に同梱
   (research/tls_id.rs と同形、lib に複製。process-global counter、
   `u32::MAX` sentinel)
4. `get(&self, key) -> Option<V>`:
   - `let i = self.partition_of(); self.partitions[i].lock().unwrap().get(key).cloned()`
5. `insert`, `contains_key`, `remove`, `len`, `is_empty`, `capacity`, `partitions`
   も同型 (内部で Mutex を取り、senba::Cache の対応 method に委譲)
6. `unsafe impl Send / Sync`: 自動導出 (Mutex<Cache<K,V,S,H>> が Send + Sync なので
   自然に派生)

### `src/lib.rs` への追加 (~3 行)

```rust
pub mod concurrent;
```

### `Cargo.toml` の `package.include` (~1 行)

`src/**/*.rs` glob で既にカバーされている。明示的な追加不要。

### Unit tests (`src/concurrent.rs::tests`)

1. `new_distributes_capacity_evenly`: 100 / 4 で各 partition cap=25
2. `new_handles_capacity_remainder`: 103 / 4 で {26, 26, 26, 25}
3. `single_thread_basic`: insert / get / remove が同一 thread から動く
4. `concurrent_invariants_smoke`: 4 thread × 50_000 Zipf op で
   `len <= capacity` / `get` した value の正しさ
5. `partitions_isolate_keys_by_thread`: 2 thread に分けて同一 key を入れる →
   別 partition に居住することを確認 (`partition_of` の thread-id 依存を検証)
6. `power_of_two_partitions_asserted`: `partitions=3` で panic

## (T × N) sweep 計画

これが本企画の **主成果物** で、c-series / r-series の単一 acceptance gate (≥+5%
Mops とか) は使わない。

### sweep 軸 (independent)

| 軸 | 値 | 備考 |
|---|---|---|
| T (`--threads`) | 1, 2, 4, 8, 16 | bench_concurrent 既存の thread sweep |
| **N (`--partitions`)** | **1, 2, 4, 8, 16, 32, 64** | 新規軸、partitioned 専用 |
| workload (`--source` + `--workload-param`) | Zipf {0.8, 1.0, 1.4} × {gim, read-heavy}、Twitter {cluster006, 016, 018, 019, 034}、ARC {OLTP, DS1, MergeP, S1} | r1-results §と同枠 |
| value (`--value`) | u64, string | r1-results で string 帯が増幅した実績あり |
| cap (`--cap`) | r1-results 既存の workload 別代表 cap | per-workload 固定 |
| trials | 3 (smoke) / 5 (確定値取得時) | 必要に応じ |

総 cell 数 = **5 (T) × 7 (N) × (3+3+5+4) (workload) × 2 (value) = 1050 cell**。
これを 1 sweep で叩く想定。

### 比較対象

同 sweep で `--variant partitioned,c17s,r1,moka,mini_moka` を並走させる。
c-series / r-series との同フォーマット比較が肝。

### 領域マップの読み方

Cell ごとに以下のスカラーを抽出: `(Mops, hit_ratio)`。

- **partitioned が pareto-dominant な cell**: `Mops_partitioned > Mops_c17s` かつ
  `HR_partitioned >= HR_c17s - ε` (ε = 0.5pp 程度の許容)。
  → lib として推奨できる workload class が確定。
- **partitioned が Mops 圧勝 / HR 退行 cell**: HR loss を許容できるユーザー向けに
  推奨。doc に明記。
- **partitioned が完敗 cell**: c-series が必要な領域、partitioned を library から
  選ばない理由を doc に明記。

### 鍵となる contrast

期待事前モデルを検証する **特定 cell** を要約表に必ず含める:

1. **(T=16, N=16, Zipf 1.4 read-heavy u64)** — uncontended 極限、ceiling 検証。
   ~457 Mops 推定、c17s 159 Mops の 2.9x を期待。
2. **(T=16, N=1)** — 全 thread が 1 Mutex に殺到する degenerate 構成、c17s より
   明確に遅いことの sanity 確認。
3. **(T=4, N=64)** — partition 余りの構成、HR 維持と single-thread-like 性能。
4. **(T=16, N=16, ARC OLTP)** — HR-sensitive workload、partitioned が HR で
   明確に負けることの定量化 (effective cap = cap/16 の影響)。
5. **(T=16, N=16, Twitter cluster019)** — HR-tolerant workload、partitioned 圧勝
   想定 cell (r1 が +77.7% を出した cell の更に上振れを期待)。

### Sweep が触らないこと (out of scope)

- **perf-gate (`research/benches/sieve_cache_perf.rs`) への追加**: 本変更は ST 性能の
  `Cache` hot path を一切触らない (concurrent module は別 module、`Cache` は
  完全に従来通り `&mut self` を保つ) ので、perf-gate は走らない / 影響ゼロ。
  PartitionedCache 専用の perf-gate を追加するかは sweep 結果を見てから判断。
- **adaptive N**: workload に応じて N を動的選択する仕組みは持たない。
  user が constructor で渡す静的値。

## Acceptance / Reject 基準 (loose、baseline 特性)

c-series の単一 gate と異なり、partitioned は領域分割が前提なので **採用 / reject
の二値判定ではなく、領域マップそのものが成果物**。最低限の sanity:

- **採用条件 (lib に残す)**:
  - (T=16, N=16, Zipf 1.4 read-heavy u64) で c17s 比 **+50% 以上**の Mops
    (ceiling 推定 2.9x の 50% 程度が控えめ目標)
  - 1050 cell のうち **最低 100 cell** で c17s pareto-dominant
- **reject 条件 (lib から削除、experimental 行き)**:
  - (T=16, N=16, Zipf 1.4 read-heavy u64) で c17s と同等またはそれ以下
    (= Mutex<Cache> approach の前提が崩れる)
  - r1 に全 workload で支配される (= partitioned の存在意義が消える)

reject の場合、`senba::concurrent::PartitionedCache` を `senba-research`
に移動。lib API は **削除する**: backwards compat の名目で残すと publishable
surface を汚す。

## Risk register

- **R1**: `std::sync::Mutex` が `parking_lot::Mutex` より遅く ceiling 推定 (~35 ns/op)
  に届かない。→ 観測したら parking_lot 採用を再検討、lib 側に新規 dep として
  入れるか別 module で分けるか別途判断。
- **R2**: `current_tls_id()` の TLS slot 確保コストが見えないが thread spawn 時の
  1 回限りなので bench loop には乗らない。問題なし想定。
- **R3**: N が大きいと **メモリ footprint が N 倍に膨らむ**
  (cap=4096, N=64 なら entries / tags 配列 64 重複)。capacity を increase したい
  user は cap/N で渡す形になり、API doc で明記が必要。
- **R4**: `partition_mask` が power-of-two 仮定なので `N=3` 等を panic で弾く。
  user friendly な制約だが doc に書く必要あり。

## 関連レポート

- `2026-05-12-mt-overhead-vs-lib.md` — lib vs c17s overhead の定量化 (ceiling 算出基)
- `2026-05-12-r1-design.md` / `2026-05-12-r1-results.md` — set-associative 流の
  routing variant、本書の "WAYS=SHARDS かつ Mutex 簡素化" 極限と本質同形
- `2026-05-10-write-contention-design-space.md` — write contention 改善策の全体図、
  本書は §A の「直列化 (= partitioned Mutex)」象限の lib 化版
