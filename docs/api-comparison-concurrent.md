# senba::concurrent::Cache vs moka / quick_cache / mini_moka / stretto / dashmap API 比較

> **Living document.** ST 版の [`docs/api-comparison.md`](./api-comparison.md) と
> 対をなす、`senba::concurrent::Cache`(`src/concurrent/cache.rs`) 用の
> API ギャップ追跡ドキュメント。**senba 列の凡例**:
>
> - `O` = 当初から提供あり
> - `〇` = この比較を起点に **新規追加した** API(着手済み)
> - `△` = 部分的 / 制約付き
> - `x` = 未提供
>
> 目的は「production ready な concurrent cache として最低限期待される API」を
> 並べて、何が欠けているかを事実ベースで列挙し、実装の進捗を追跡すること。
> 新しく API を生やしたら、サマリ表 / 各セクションのギャップ記述 / §9 の
> 「欠けているもの一覧」の三箇所を `〇` に塗り替えて、§11 の更新履歴に
> 一行残すこと。

`senba::concurrent::Cache`(`src/concurrent/cache.rs`) は sharded、lock-free-reader
の SIEVE 実装で、reader は `&self` で値クローンを返し、writer は per-shard
`parking_lot::Mutex` を取る。同じ「`&self` で並行アクセス可」モデルを持つ Rust の
代表的な concurrent cache / map と並べて、メソッド単位で機能差を整理する。

比較対象は以下の docs.rs 最新ページ:

- moka: <https://docs.rs/moka/latest/moka/sync/struct.Cache.html>
- quick_cache: <https://docs.rs/quick_cache/latest/quick_cache/sync/struct.Cache.html>
- mini_moka: <https://docs.rs/mini-moka/latest/mini_moka/sync/struct.Cache.html>
- stretto: <https://docs.rs/stretto/latest/stretto/struct.Cache.html>
- dashmap: <https://docs.rs/dashmap/latest/dashmap/struct.DashMap.html>(cache ではないが concurrent map のベースラインとして併記)

ST 版 (`senba::Cache`) との対比は別ドキュメント(`docs/api-comparison.md`)。本
ドキュメントは ST 版に存在する API のうち concurrent でも欠けているもの
(`peek` / `iter` / `clear` 等)も含めて、concurrent 文脈で改めてギャップとして
列挙する(ST と concurrent で実装難易度・soundness 制約が違うため別建て)。

## サマリ表

凡例: O = 当初から提供あり, 〇 = 本ドキュメント起点で追加済み, x = なし, △ = 部分的 / 制約付き

| カテゴリ / 機能 | senba::concurrent::Cache | moka::sync::Cache | quick_cache::sync::Cache | mini_moka::sync::Cache | stretto::Cache | dashmap::DashMap |
|---|---|---|---|---|---|---|
| `new(capacity)` | O | O | O | O | O | O |
| Builder パターン | x | O (`builder()`) | △ (`with_options`) | O (`builder()`) | O (`builder()`) | x |
| カスタム hasher | O (`with_hasher` / `with_shards_and_hasher`) | O | O | O | △ | O |
| 明示 shard 数 | O (`with_shards*`) | x (内部 hidden) | O (`shards`) | x | x | O (`with_shard_amount`) |
| `capacity` / `len` / `is_empty` | O | O (`entry_count` / `weighted_size`) | O | O | O | O |
| TTL / TTI | x | O | △ (lifecycle) | O | O | x |
| Weighted size | x | O (`weigher`) | O | O | O (`cost`) | x |
| `Send + Sync` | O (`K,V: Send + 'static`, `V: Clone`) | O | O | O | O | O |
| `Clone` (cheap, Arc) | x | O | x | O | O | x (`Arc<DashMap>` で包む) |
| `&self get` | O `-> Option<V>` (clone) | O `-> Option<V>` (clone) | O | O | O `-> Option<ValueRef<V>>` | O `-> Option<Ref<'_, K, V>>` |
| `&self insert` | O `-> ()` | O `-> ()` | O `-> ()` | O `-> ()` | O `-> bool` (admission) | O `-> Option<V>` |
| `&self remove` | O `-> Option<V>` | O `-> Option<V>` | O `-> Option<(K,V)>` | O | O | O |
| `contains_key` | O | O | O | O | x | O |
| `peek` (非昇格 get) | 〇 | x | O | x | x | n/a |
| `get_mut` (in-place 更新) | x | x | x | x | O | O (`get_mut` で write guard) |
| `get_key_value` | x | x | x | x | x | O |
| `clear` / `invalidate_all` | 〇 | O (`invalidate_all`) | O | O (`invalidate_all`) | O | O |
| `iter` | x | O | O (`Key: Clone` 必須) | O | x | O |
| `keys` / `values` | x | x | x | x | x | x (iter 経由) |
| `drain` | x | x | O | x | x | x |
| `retain` / `invalidate_entries_if` | x | O (`invalidate_entries_if`) | O | O (`invalidate_entries_if`) | x | O |
| `get_or_insert*` | x | O (多数 + `entry()` API) | O (`get_or_insert_with` / `get_value_or_guard`) | x | x | O (`entry().or_insert_with`) |
| `try_get_or_insert*` | x | O (`try_get_with`) | O | x | x | x |
| `entry(K)` ハンドル API | x | O (`entry` / `entry_by_ref`) | O (`entry` API) | x | x | O |
| eviction listener | △ (`insert_with` per-call のみ) | O (builder `eviction_listener`) | △ (lifecycle) | O (builder `eviction_listener`) | x | x |
| 統計 (hits / misses 等) | x | x | O (feature gate) | x | O (`metrics`) | x |
| `resize` / `set_capacity` | x | x (policy 経由) | O | x | O (`update_max_cost`) | x |
| async API | x | O (`moka::future`) | O (`*_async`) | x | x | x |
| `Borrow<Q>` 一般化 | O | O | O | O | △ | O |
| `Debug` | x | O | x | O | x | O |
| `Extend<(K,V)>` | x | x | x | x | x | O |
| `IntoIterator` | x | O (`&Cache`) | x | O (`&Cache`) | x | O |

---

## 1. Construction / sizing

### senba::concurrent

```rust
pub fn new(capacity: usize) -> Self;                                              // H = Xxh3Build
pub fn with_shards(capacity: usize, shards: usize) -> Self;
pub fn with_hasher(capacity: usize, hasher: H) -> Self;
pub fn with_shards_and_hasher(capacity: usize, shards: usize, hasher: H) -> Self;
```

- 境界: `K: Hash + Eq + Send + 'static`, `V: Clone + Send + 'static`
  (epoch reclamation のため `'static` 必須、reader が値クローンを返すため `V: Clone` 必須)。
- `capacity > 0`、`shards` は 2 冪、`capacity >= shards` が必要。
- shard 数は `Cache::new` で自動選択(`next_pow2(cap/8)`、`MIN_PER_SHARD = 4` /
  `MAX_PER_SHARD = 64` でクランプ。`docs/reports/2026-05-13-c17s-shard-heuristic.md`)。
- AVX2+BMI1 検出は `with_shards_and_hasher` で一度だけ `is_x86_feature_detected!`
  を引いて bool で保持。reader 経路の dispatch は単一 load。

### moka

- `Cache::new(max_capacity: u64)` と `Cache::builder()`。builder で
  TTL / TTI / weigher / eviction_listener / name / hasher / initial_capacity を一括設定。
- shard 数は内部 detail で API には出ない。

### quick_cache

- `Cache::new(capacity)` / `with(estimated_items_capacity, weight_capacity, weighter, hasher, lifecycle)` /
  `with_options(Options, weighter, hasher, lifecycle)`。`Options` 経由で shards / hot allocation 比率
  などを露出。

### mini_moka / stretto / dashmap

- mini_moka: `Cache::new(max_capacity: u64)` + `builder()`。moka と同じ流儀で API は簡素化。
- stretto: `Cache::builder(num_counters, max_cost)` で TinyLFU の counter 数とコスト上限を分離指定。
- dashmap: `DashMap::new` / `with_capacity` / `with_shard_amount` / `with_hasher`。

### ギャップ

- **builder パターン**: moka / mini_moka / stretto が持っており、TTL / weigher /
  eviction_listener を追加する時点で必須になる。今は positional 4 種の `with_*` で
  軸を増やしているが、TTL / weigher を入れた瞬間に組み合わせ爆発するので、その前に
  builder へ移行するのが筋。
- **TTL / TTI**: production cache としては「絶対時刻ベース expire」「最終アクセス
  からの相対 expire」の両方が要る局面が多い。SIEVE は visited bit ベースで時間軸を
  持たないので、TTL を入れる場合は entry に deadline timestamp を持たせて lookup 時に
  期限切れを EMPTY 扱いする実装になる(ST 版の議論 §1 と同じく構造変更が大きい)。
- **Weighted capacity**: entry ごとに cost を持たせて合計 cost で eviction を判断する。
  SIEVE は per-entry のサイズ概念を持たないので、外付け weigher + per-shard cost 集計の
  追加が必要。
- **`Clone` (cheap, Arc)**: moka / mini_moka / stretto は `Arc` 内包で cheap clone を
  提供している。現在の senba::concurrent は `Box<[Shard]>` 直保有なので、
  `Cache: Clone` させるなら内部を `Arc<Inner>` 化するリファクタが入る。pub API は
  `&self` なので Arc 化のコストは reader hot path には乗らない(load 一段増えるだけ)。

---

## 2. 基本オペレーション

### senba::concurrent

```rust
pub fn contains_key<Q>(&self, key: &Q) -> bool
    where K: Borrow<Q>, Q: Hash + Eq + ?Sized;
pub fn get<Q>(&self, key: &Q) -> Option<V>
    where K: Borrow<Q>, Q: Hash + Eq + ?Sized;
pub fn peek<Q>(&self, key: &Q) -> Option<V>                                        // 〇 added
    where K: Borrow<Q>, Q: Hash + Eq + ?Sized;
pub fn insert(&self, key: K, value: V);
pub fn insert_with<F: FnOnce(K, V) + Send + 'static>(&self, key: K, value: V, on_evict: F);
pub fn remove<Q>(&self, key: &Q) -> Option<V>
    where K: Borrow<Q>, Q: Hash + Eq + ?Sized;
pub fn clear(&self);                                                               // 〇 added
```

- `get` は値クローン(reader は seqlock + epoch pin で snapshot を取って `V::clone` する)。
  `V: Clone` 必須はこの設計から来ている。SIEVE の visited bit を `AtomicU16::fetch_or`
  で立てるので reader 自身も atomic 書き込みは行う(ただし共有 cacheline で
  contended になりにくいよう per-entry に分散)。
- `insert` は戻り値なし。evict 観測は `insert_with(.., on_evict)` で per-call にコールバック。
- `insert_with` の `on_evict` は **当該 `insert` で起きた Path C 1 件のみ**を観測する
  (warmup Path B / 既存キー更新 Path A は呼ばれない)。callback は shard writer mutex
  内で呼ばれるので長時間処理を入れてはいけない。

### moka

- `get<Q>(&self, &Q) -> Option<V>` で値クローン(`V: Clone` 必須)。
- `insert(&self, K, V)`(戻り値なし)、`remove<Q>(&self, &Q) -> Option<V>`。
- `contains_key`、`get_with` / `optionally_get_with` / `try_get_with` / `entry()` / `entry_by_ref()`。

### quick_cache

- `get` / `peek` / `insert` / `replace` / `remove` / `remove_if` / `clear` / `retain` /
  `drain` を `&self` で提供。`peek` は非昇格 lookup を明示的に分けている。

### mini_moka

- moka とほぼ同じ surface だが entry API や `try_get_with` は無い。
- `get<Q>` / `insert` / `invalidate` / `invalidate_all` / `invalidate_entries_if` / `iter`。

### stretto

- `insert(key, val, cost) -> bool`(TinyLFU が admission を判断、却下時 false)、
  `insert_with_ttl`、`insert_if_present`、`get`、`get_mut`、`wait()` で
  background scheduler の drain を待つ。

### dashmap

- 概念的に concurrent `HashMap` なので eviction policy を持たない。`insert -> Option<V>`、
  `get -> Option<Ref<'_,K,V>>` で borrow guard を返す(senba と違って値クローン不要)。

### ギャップ

senba::concurrent に欠けている基本 API:

- **`peek`(非昇格 lookup)**: 〇 追加済み。`get` から VISITED の `fetch_or` を
  外しただけの parallel path で、`const PROMOTE: bool` を `find_get` family に
  thread して monomorphize 共有(`get` 経路は const-fold で生成不変、perf-gate
  4 cells で no-change)。
- **`get_or_insert_with` / `try_get_or_insert_with` / `entry` API**: 並行 cache の
  キラー API。「lookup → miss → 計算 → insert」を caller 側で書くと、その間に他
  thread が同じキーで計算を走らせて重複 work が出る(thundering herd)。moka /
  quick_cache はこの問題を `entry` API + 内部 per-key 排他で解決している。senba::concurrent
  にも同種の API が要る(per-shard mutex を保持して計算する単純版 + Future-aware な
  `get_value_or_guard` 系の階層になる見込み)。
- **`get_key_value`**: `Borrow<Q>` 経由で引いた canonical な `K` を取り出すケース。
  reader は値クローン経路なので `(K, V)` の同時クローンになるが、構造的には可能。

---

## 3. イテレーション / 内省

### senba::concurrent

- `len()` / `is_empty()` / `capacity()` / `shards()` のみ。`shards()` は `#[doc(hidden)]`
  (auto-shard heuristic を internal detail に保つため)。
- `iter()` / `keys()` / `values()` / `drain()` は **未実装**。

### moka / quick_cache / mini_moka / dashmap

- moka: `iter()` が `(Arc<K>, V)` を yield、`IntoIterator for &Cache`。
- quick_cache: `iter()`(`Key: Clone` 必須)、`drain()`。`peek_iter()` も別途。
- mini_moka: `iter()`、`IntoIterator for &Cache`。
- dashmap: `iter()` / `iter_mut()` / `keys()`(値所有 Iter)、`shards()` で per-shard
  guard を直接覗ける。

### ギャップ

- **`iter()` / `keys()` / `values()`**: concurrent では「snapshot iteration」と
  「live iteration」のどちらにするかが設計判断。moka / quick_cache は live(進行中の
  writer と並行に進む)で、各エントリの取り出しは「その瞬間の値クローン」になる。
  senba::concurrent も同じセマンティクスで `(K, V)` クローンを yield する形が
  自然(reader 経路の延長で seqlock + epoch pin を使い、shard を順に消化する)。
- **`drain()`**: concurrent では珍しく、quick_cache が持つ。論理的に「全エントリを
  取り出して空にする」一括操作だが、live iteration 中に並行 insert が来ると
  「drain で取った後に入った要素」が残ることを許容する semantics になる(strict な
  「呼出後 cache は空」を保証するなら writer 全 shard 排他になる)。
- 観測系として「shard 別 len 分布」「hot shard 検出」など、production tuning に
  使える introspection は今後の検討。

---

## 4. バルクオペレーション / entry API / get-or-insert

### senba::concurrent

- `clear` / `retain` / `Extend` / `IntoIterator` 未実装。
- entry API / `get_or_insert_with` も未実装。

### moka

- `invalidate_all()` で論理 clear、`invalidate_entries_if(predicate)` で述語ベース。
- `entry(K)` / `entry_by_ref(&Q)` ハンドル API + `get_with` / `optionally_get_with` /
  `try_get_with` / `*_by_ref` を網羅。

### quick_cache

- `clear` / `retain` / `drain` / `get_or_insert_with` / `get_value_or_guard`
  (同期 / async)、`entry()`、`replace`。

### mini_moka

- `invalidate_all` / `invalidate_entries_if`。entry / get_with 系は無い。

### stretto

- `clear`、`wait()` で write buffer drain。

### dashmap

- `clear` / `retain` / `entry().or_insert_with` / `alter` / `view`。

### ギャップ

- **`clear()`**: 〇 追加済み。per-shard で writer mutex を取って `ManuallyDrop::take`
  で K/V を抜き、`crossbeam-epoch` で defer drop、tag を EMPTY 化、`len/hand=0` /
  `free_ids/next_fresh_id` リセット、`path_c_epoch` を bump して racing reader を
  retry させる。evict カウンタには載せない(将来 stats が入った時)。
- **`retain(predicate)`**: writer mutex を per-shard で取って ST 版同様の 1 パス
  compact をかける。senba::concurrent の reader が seqlock retry で safe に観測できる
  ので、ST と同等の `O(n)` ベースで実装可能。
- **`entry(K)` / `get_or_insert_with`**: §2 のギャップで触れたとおり最優先。
  thundering herd 抑止は production cache の存在意義の一つ。

---

## 5. 立ち退きリスナー / 通知

### senba::concurrent

- `insert_with(.., on_evict)` で **当該 `insert` 起因の 1 件のみ**を観測。
- グローバルな listener 登録は無い。callback は writer mutex 内で同期実行される
  (`'static + Send` 必須)。

### moka / mini_moka

- builder の `eviction_listener(...)` でクロージャを登録、各 evict 時に
  `(Arc<K>, V, RemovalCause)` を受け取る。`RemovalCause` で Replaced / Size /
  Expired / Explicit を区別。

### stretto / quick_cache / dashmap

- stretto: 専用 listener は無く、`Cost` を伴う `insert(bool)` の戻り値で admission
  を観測。
- quick_cache: `Lifecycle` trait で on_evict 相当を提供(insert ごとではなく
  hook 全体)。
- dashmap: cache ではないので listener なし。

### ギャップ

- **グローバル eviction listener**: production では「Prometheus に eviction を吐く」
  「downstream にトムストーンを送る」など、cache 全体に対する callback が標準的に
  必要。`insert_with` の per-call hook は thundering herd 抑止には使えるが、汎用な
  observability 用途にはならない。builder で 1 度登録する形が筋。
- **`RemovalCause` 相当**: Size / Explicit / Replaced を区別したい(将来 TTL を入れた
  なら Expired も)。senba::concurrent は今 Path A(replace)・Path C(size evict)・
  `remove`(explicit)を内部で区別しているので、enum を立てて callback 引数に
  載せるだけで分類可能。

---

## 6. 並行モデル

### senba::concurrent

- 全公開メソッドが `&self`。reader は **lock free**(seqlock + epoch pin + 値クローン)、
  writer は per-shard `parking_lot::Mutex`。
- AVX2+BMI1 で SIMD tag scan、`SENBA_FORCE_SCALAR=1` で scalar fallback を強制可能。
- `Cache: Send + Sync` は `K, V: Send + 'static`(epoch reclamation のため)、
  `V: Clone`(reader が値クローンを返すため)。

### moka / mini_moka

- 全 `&self`。内部で per-shard lock + write buffer + scheduler。`Clone` cheap(Arc)。
- `moka::future::Cache` で async 版あり。

### quick_cache

- 全 `&self`。`Cache: !Clone`(内部 `parking_lot::RwLock`)。`*_async` 版で async API。

### stretto

- 全 `&self`。内部に OS thread を 2 本持つ(eviction policy / write)。

### dashmap

- 全 `&self`。per-shard `RwLock`。

### ギャップ

- **async 版**: `moka::future` のような future-aware API が要るか。production の
  tokio 環境では `get_value_or_guard_async`(quick_cache)相当があると、計算が長い
  キーで thundering herd を抑える際に重要。設計上の難所は「block_on」とのインターフェイス。
- **`Cache: Clone`(cheap)**: 現状 `Arc<Cache>` で包む必要があり、moka 流の
  「Cache 自体が `Arc<Inner>`」モデルに比べてユーザコード側に boilerplate が増える。

---

## 7. 統計

### senba::concurrent

- `stats()` 未実装(**設計再考が必要**)。ST 版と同じ `Stats { hits, misses,
  insertions, evictions }` 公開を目指して、per-shard `AtomicU64::fetch_add(Relaxed)`
  を `ShardCounters` (`#[repr(C, align(64))]`) に置く naive 実装を試したが、
  並行 perf-gate (`sieve_concurrent_perf`) の **u64 / threads=16 / Zipf=1.4** cell
  で **+354%** という致命的な regression が観測された。Zipf 1.4 で hot key が
  少数の shard に集中 → 16 thread が同一 shard の `hits` cacheline を ping-pong する
  パターンで、CLAUDE.md の「>5% 即 commit-blocker」を大きく逸脱。
  reader hot path に `Relaxed atomic add` が乗っても OoO で実質ゼロコストに
  なる ST 版の見立ては concurrent では成立しない(共有 cacheline contention は
  OoO で隠せない)。
- 解決の選択肢(別セッション):
  - thread-banked counter array(per-shard `[CounterBank; N]`、`thread_local` で
    bank index を引く)— 確実だが TLS 参照コストが lookup 1 回ぶん乗る
  - sampled counter(1/N でしか加算しない)— 精度は犠牲、メモリは安い
  - `concurrent-stats` Cargo feature でデフォルト無効化(quick_cache 流)— 一番
    無難だが production 用途では結局有効化したい人が hot path 損失を払う
- 4 fields 全部の counter を諦めて writer-only(`insertions` / `evictions`)に
  絞る案もあるが、production 観測では hit ratio が最重要なので逃げになる。

### moka / mini_moka

- 公開 API レベルでは `entry_count` / `weighted_size` のみで hit/miss は出さない
  (`name` を付けて外部 metrics に配線する想定)。

### quick_cache

- `hits()` / `misses()`(`stats` feature gate)。

### stretto

- `metrics` フィールドで詳細統計。

### dashmap

- 統計は持たない。

### ギャップ

- **hit / miss / insertion / eviction カウンタ**: production 運用では必須(hit ratio
  が下がっていることを検知できないと cache のチューニングができない)。reader hot
  path に AtomicU64 が乗るが、per-shard でカウンタを分散させれば cacheline contention
  は writer 側と同じく shard 単位に局所化される。ST 版と違って `Relaxed` で十分。
- **原因別 eviction (`RemovalCause`)**: §5 と同じく Size / Explicit / Replaced を
  集計したい。listener とセットで設計するのが自然。

---

## 8. その他(serialize / 拡張点)

| 機能 | senba::concurrent | moka::sync | quick_cache | mini_moka | stretto | dashmap |
|---|---|---|---|---|---|---|
| Serde 直接サポート | x | x | x | x | x | △ (feature) |
| `Default` impl | x | x | x | x | x | O |
| `From<HashMap>` 等の変換 | x | x | x | x | x | x |
| 名前付け(`name()`) | x | O | x | x | x | x |

---

## 9. 「senba::concurrent::Cache に欠けているもの」一覧

事実として欠けている機能。重要度の主観評価は付けず、列挙のみ。production-ready
化に向けて優先度を決める際の素材として参照する。

**Construction / sizing**

- `Cache::builder()` パターン
- TTL / TTI(time-to-live / time-to-idle)
- 重み付け容量(weight / weigher)
- unbounded mode(SIEVE は容量起因 evict が動作の中核なので、需要があるかは要検討)

**基本オペレーション**

- `peek(&Q) -> Option<V>`(非昇格 lookup) 〇
- `get_key_value(&Q) -> Option<(K, V)>` / `peek_key_value(&Q)`
- `insert -> Option<(K, V)>` 版(現在の `insert_with` を戻り値ベースに直す案、または併設)

**バルク / イテレーション**

- `clear()` / `invalidate_all()` 〇
- `iter()` / `keys()` / `values()`
- `drain()`
- `retain(predicate)` / `invalidate_entries_if`
- `IntoIterator for &Cache`
- `Extend<(K, V)>`
- `resize(new_cap)` / `update_max_capacity`

**Entry API / get-or-insert**

- `entry(K)` / `entry_by_ref(&Q)`
- `get_or_insert_with` / `get_or_insert_mut`
- `try_get_or_insert_with`
- `get_value_or_guard*`(quick_cache 流の async-aware guard)

**通知 / 統計**

- グローバル eviction listener(builder 経由で全 evict を hook)
- `RemovalCause` 相当(Size / Explicit / Replaced の区別)
- `stats() -> Stats { hits, misses, insertions, evictions }`(ST 版と同等)
  — naive 実装(per-shard `AtomicU64`)で並行 perf-gate cell 2 (u64/T=16/Zipf=1.4)
  に **+354%** regression、設計再考が要る(§7 参照)

**並行 / async**

- `Cache: Clone`(`Arc<Inner>` 化、cheap clone)
- async 版 API(`moka::future::Cache` / `quick_cache *_async` 相当)

**派生 trait / 拡張**

- `Debug`(`K, V: Debug`、entries は `iter()` 経由で非昇格)
- 名前(`name()`)
- Serde サポート

---

## 10. メモ

- senba::concurrent の reader hot path は「seqlock + epoch pin + 値クローン」で
  動いていて、`V: Clone + 'static` が境界条件。これは moka / quick_cache が値
  クローンを返すのと同じ流儀で、API 表面の差は少ない(両者とも `get -> Option<V>`)。
  違いは listener / TTL / weigher / entry API 等の付帯機能の有無で、これらが
  「production ready」と「研究用」の境界線になっている。
- `insert_with` の per-call callback は thundering herd 抑止には使えるが、
  observability(全 evict を集計する)用途にはならない。builder + global
  listener を生やすときに `insert_with` をどう位置づけ直すか(残す / 統合する)は
  別途検討。
- SIEVE は「容量起因 evict」が動作の中核なので、unbounded mode / weighted mode は
  入れた瞬間に挙動の前提が崩れる。「concurrent な SIEVE」というアイデンティティを
  保つなら TTL までが拡張のスコープで、weight / unbounded は別 facade(別 struct)
  にする選択肢もある。
- 本ドキュメントは「production ready」を到達点と置いて API gap を列挙するが、
  「全部実装する」とは限らない。研究目的の最小 API に留める判断、別 facade に
  切り出す判断、senba::Cache の ST 版に閉じ込める判断はそれぞれの軸で個別検討する。

---

## 11. 更新履歴

- **2026-05-16**: 初版。`senba::concurrent::Cache` の現状 API(`new` / `with_shards*` /
  `with_hasher*` / `capacity` / `len` / `is_empty` / `shards` / `contains_key` /
  `get` / `insert` / `insert_with` / `remove`)を起点に、moka / quick_cache /
  mini_moka / stretto / dashmap と並べてギャップを整理。
- **2026-05-16**: `Cache::peek` を追加。`const PROMOTE: bool` を `find_get` /
  `find_get_avx2` / `find_get_scalar` / `try_candidate` に thread して
  monomorphize 共有(`PROMOTE=true` は VISITED `fetch_or` を保持、`PROMOTE=false`
  は const-fold で抜く)。`get` 経路は生成不変、並行 perf-gate 4 cells で
  ±5% 以内(-3.97% / +1.99% / +0.95% / -0.18%)。サマリ表 / §2 / §9 を 〇 に更新。
- **2026-05-16**: `Cache::clear` を追加。per-shard writer mutex 下で
  `ManuallyDrop::take` で K/V を抜き `crossbeam-epoch` で defer drop、tag を
  EMPTY 化、`len` / `hand` / `visited` / `free_ids` / `next_fresh_id` をリセット、
  `path_c_epoch` を bump して racing reader を retry させる。reader / writer hot
  path に分岐は乗らないので perf-gate は cold-path 規定に従い skip。サマリ表 /
  §4 / §9 を 〇 に更新。
- **2026-05-16**: `Cache::stats() -> Stats` 実装を試行 → 撤回。per-shard
  `ShardCounters { hits/misses/insertions/evictions: AtomicU64 }` を別 cacheline
  に配置し `Relaxed::fetch_add` で reader / writer hot path に挿入したが、並行
  perf-gate u64/T=16/Zipf=1.4 cell で **+354%** という致命的 regression
  (Zipf 1.4 で hot shard が固定され 16 thread が同一 `hits` cacheline を ping-pong)。
  CLAUDE.md の「>5% 即 commit-blocker」を大幅逸脱のため、コードは revert。§7 /
  §9 に thread-banked counter / sampled counter / feature-gate の選択肢を記録、
  別セッションで再設計する方針に。
