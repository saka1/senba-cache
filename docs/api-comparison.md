# senba::Cache vs moka / lru / quick_cache / stretto API 比較

> **Living document.** 元は `docs/reports/2026-05-06-api-comparison-moka-lru.md`
> として一回限りの調査レポートだったが、API 拡張のチェックリストも兼ねるため
> `docs/` 直下に昇格させて継続更新する形にした。**senba 列の凡例**:
>
> - `O` = 当初から提供あり
> - `〇` = この比較を起点に **新規追加した** API(着手済み)
> - `△` = 部分的 / 制約付き
> - `x` = 未提供
>
> 新しく API を生やしたら、サマリ表 / 各セクションのギャップ記述 / §9 の
> 「欠けているもの一覧」の三箇所を `〇` に塗り替えて、§11 の更新履歴に
> 一行残すこと。

`senba::Cache`(`src/sieve_cache.rs`) の公開 API を、Rust のキャッシュ系ライブラリ
代表格である **moka**(`moka::sync::Cache`) と **lru**(`lru::LruCache`) を主軸に、
補助として **quick_cache**(`quick_cache::sync::Cache`)・**stretto**(`stretto::Cache`)
と並べて、メソッド単位で機能差を整理する。本ドキュメントは「senba にどんな機能が
欠けているか」を事実ベースで列挙し、API 追加作業の進捗を追跡することを目的とする。

調査対象は以下の docs.rs 最新ページ:

- moka: <https://docs.rs/moka/latest/moka/sync/struct.Cache.html>
- lru: <https://docs.rs/lru/latest/lru/struct.LruCache.html>
- quick_cache: <https://docs.rs/quick_cache/latest/quick_cache/sync/struct.Cache.html>
- stretto: <https://docs.rs/stretto/latest/stretto/struct.Cache.html>

## サマリ表

凡例: O = 当初から提供あり, 〇 = 本ドキュメント起点で追加済み, x = なし, △ = 部分的 / 制約付き

| カテゴリ / 機能 | senba::Cache | moka::sync::Cache | lru::LruCache | quick_cache | stretto |
|---|---|---|---|---|---|
| `new(capacity)` | O | O (`new(u64)`) | O (`NonZeroUsize`) | O | O |
| Builder パターン | x | O (`builder()`) | x | △ (`with_options`) | O (`builder()`) |
| Unbounded | x | x | O (`unbounded()`) | x | x |
| カスタム hasher | x (`Xxh3Build` 固定) | O (builder 経由) | O (`with_hasher`) | O | △ |
| capacity / len / is_empty | O | O (`entry_count`, `weighted_size`) | O (`cap`, `len`, `is_empty`) | O | O |
| TTL / TTI | x | O | x | △ (lifecycle) | O (`insert_with_ttl`) |
| Weighted size | x | O (`weigher`) | x | O | O (`cost`) |
| eviction listener | x | O | x | △ (lifecycle) | x |
| `get` 受け取り | `&mut self -> Option<&V>` | `&self -> Option<V>` (clone) | `&mut self -> Option<&V>` | `&self -> Option<V>` (clone) | `&self -> Option<ValueRef<V>>` |
| `peek` (非昇格 get) | 〇 | x | O (`peek`, `peek_mut`) | O | x |
| `peek_lru` / `peek_mru` | x | x | O | x | x |
| `get_mut` | 〇 | x | O | x | O |
| `Borrow<Q>` 一般化 (`&str` で `String` キャッシュ引ける等) | 〇 | O | O | O | △ |
| `insert` | O `-> Option<(K,V)>` | O `()` | O (`put -> Option<V>`, `push -> Option<(K,V)>`) | O | O `-> bool` |
| `remove` | O `-> Option<V>` | O `-> Option<V>` | O (`pop`, `pop_entry`) | O `-> Option<(K,V)>` | O |
| `pop_lru` / `pop_mru` | x | x | O | x | x |
| `contains_key` | O | O | O (`contains`) | O | x |
| `clear` | 〇 | O (`invalidate_all`) | O | O | O |
| `iter` | 〇 | O | O (`iter`, `iter_mut`) | O | x |
| `keys` / `values` | x | x (iter 経由) | x (iter 経由) | x (iter 経由) | x |
| `drain` | x | x | x | O | x |
| `retain` / `invalidate_entries_if` | x | O | x | O | x |
| `get_or_insert*` (entry API 含む) | △ (`get_or_insert_with` のみ 〇) | O (多数) | O (多数) | O | x |
| `try_get_or_insert*` | x | O | O | O | x |
| `promote` / `demote` | x | x | O | x | x |
| `resize` / `set_capacity` | x | x (policy 経由) | O | O | O (`update_max_cost`) |
| 統計 (hits/misses) | x | x | x | O (feature) | O (`metrics`) |
| 並行 (`&self` で書き換え可) | x (要 `&mut self`) | O | x | O | O |
| `Send + Sync` | △ (`K,V: Send/Sync` 依存) | O | △ | O | O |
| `Clone` | x | O (cheap, Arc) | O (where K,V,S: Clone) | x | O (Arc) |
| `Debug` | x | O | O | x | x |
| `IntoIterator` | x | O (`&Cache`) | O (3 形態) | x | x |
| async API | x | O (`moka::future`) | x | O (`*_async`) | x |

---

## 1. Construction / sizing

### senba

```rust
pub fn new(capacity: usize) -> Self;
pub fn with_shards(capacity: usize, shards: usize) -> Self;
```

- `capacity > 0` 必須。`with_shards` は `shards` が 2 冪、かつ
  `ceil(capacity / shards) <= MAX_PER_SHARD (= 64)` を満たすこと。
- shard 数は `Cache::new` で自動選択(`next_pow2(ceil(cap/64))`)。
- hasher は `Xxh3Build` 固定(構築時に注入する API はない)。
- `SlotSize` ジェネリック(`Slot16` / `Slot32` (default) / `Slot64`)で
  entries arena の stride を型レベル指定。

### moka

- `Cache::new(max_capacity: u64)` の他、`Cache::builder()` 経由で TTL / TTI /
  weigher / max_capacity / initial_capacity / eviction_listener / name / hasher
  などをまとめて設定する。

### lru

- `LruCache::new(NonZeroUsize)` / `unbounded()` / `with_hasher` /
  `unbounded_with_hasher`。`NonZeroUsize` で容量 0 を型で禁止している。

### quick_cache / stretto

- `quick_cache::sync::Cache::with_options` / `with_weighter` で
  weight ベースの容量指定が中心。
- `stretto::Cache::builder(num_counters, max_cost)` で TinyLFU の
  カウンタ数と総コスト上限を分離して指定。

### ギャップ

senba には builder / unbounded / カスタム hasher 注入 API がない。容量は
`usize` 直書きのため、ゼロ容量は実行時 panic で弾く構造(lru の
`NonZeroUsize` のような型レベル保証はない)。

---

## 2. 基本オペレーション

### senba

```rust
pub fn contains_key<Q>(&self, key: &Q) -> bool                                 // 〇 Borrow<Q>
    where K: Borrow<Q>, Q: Hash + Eq + ?Sized;
pub fn get<Q>(&mut self, key: &Q) -> Option<&V>                                // 〇 Borrow<Q>
    where K: Borrow<Q>, Q: Hash + Eq + ?Sized;
pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>                        // 〇 added
    where K: Borrow<Q>, Q: Hash + Eq + ?Sized;
pub fn peek<Q>(&self, key: &Q) -> Option<&V>                                   // 〇 added
    where K: Borrow<Q>, Q: Hash + Eq + ?Sized;
pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)>;
pub fn remove<Q>(&mut self, key: &Q) -> Option<V>                              // 〇 Borrow<Q>
    where K: Borrow<Q>, Q: Hash + Eq + ?Sized;
pub fn get_or_insert_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &V;    // 〇 added
pub fn clear(&mut self);                                                       // 〇 added
```

- `get` は **`&mut self`**。SIEVE の visited bit 更新があるため不可避。
- `get_mut` も同じく `&mut self` で VISITED を立てる(in-place 更新は SIEVE
  上「アクセスした」と見做すのが lru / std 系と整合的)。〇 追加済み。
- `peek` は visited bit を立てない `&self` 版 lookup(〇 追加済み)。
- `insert` の戻り値はキャパ超過時に追い出された `(K, V)`。既存キー上書き時は
  `None`(更新のみで evict なし)。
- `get_or_insert_with` はミス時のみクロージャを評価して挿入、ヒット時は
  `get` と同じく VISITED を立てる(〇 追加済み)。
- 参照系メソッド (`get` / `get_mut` / `peek` / `contains_key` / `remove`)
  は `K: Borrow<Q>, Q: Hash + Eq + ?Sized` で一般化されており(〇 追加済み)、
  `Cache<String, V>` を `&str` で引けるなど std `HashMap` と同じ感覚で使える。

### moka

- `get<Q>(&self, &Q) -> Option<V>`(値クローン)。`&self` で並行アクセス可。
- `insert(&self, K, V)` は戻り値なし。evict 通知は eviction_listener で受ける。
- `remove<Q>(&self, &Q) -> Option<V>`。

### lru

- `get(&mut self, &Q) -> Option<&V>`(LRU 順を更新), `get_mut`,
  `get_key_value`, `get_key_value_mut`。
- 非昇格アクセスとして `peek`, `peek_mut`, `peek_lru`, `peek_mru` を持つ。
  senba には対応 API なし。
- `put -> Option<V>`(追い出された value のみ)と
  `push -> Option<(K, V)>`(追い出された (K,V))の 2 種類。
  senba の `insert` は後者と同形だが「容量超過のときだけ」値を返す。
- `pop`, `pop_entry`, `pop_lru`, `pop_mru` で LRU / MRU 端を直接抜ける。

### quick_cache / stretto

- quick_cache は `&self` で `get` / `insert` / `peek` / `remove` /
  `remove_if` / `clear` / `retain` / `drain` を提供。`replace` で
  「既存キーがあれば更新」セマンティクスを切り出している。
- stretto は `insert(key, val, cost) -> bool`(TinyLFU が拒否すれば false)、
  `insert_with_ttl`、`insert_if_present`、`get_mut`、`wait()` で
  background write スレッドの drain を待つモデル。

### ギャップ

senba に欠けている基本 API:

- LRU/MRU 端を取り出す `pop_lru` 系(SIEVE には MRU/LRU の概念がそのまま
  ない代わりに「一番古い tail = `tags[0]`」を持っているので、
  `pop_oldest()` 相当を出すことは構造上可能)。
- `peek_mut`(`get_mut` は VISITED を立てる版のみ。非昇格な mut アクセスは未提供)。
- `get_key_value` 系(`(&K, &V)` ペアを返す)。

---

## 3. イテレーション / 内省

### senba

- `len()`, `is_empty()`, `capacity()`, `shards()`。
- **`iter() -> Iter<'_, K, V, S>`**(〇 追加済み): `(&K, &V)` を全 shard 順に
  yield。順序は未規定、VISITED 非昇格。
- `keys`, `values`, `drain` は未実装。`Debug` も未実装。

### moka / lru / quick_cache

- moka: `iter()` で `(Arc<K>, V)` を返すイテレータ。`IntoIterator for &Cache`。
- lru: `iter`, `iter_mut`, さらに `IntoIterator` を 3 形態(`&`, `&mut`,
  値所有)で実装。
- quick_cache: `iter()`(`Key: Clone` 必須)、`drain()`。

### ギャップ

senba の観測手段は当初 len 系の集計値のみだった。`iter()` を追加して項目単位の
観測は可能になったが、依然として欠けているのは:

- `iter_mut()`
- `keys()` / `values()`
- `IntoIterator` 系
- `Clone` / `Debug`

---

## 4. バルクオペレーション / entry API / get-or-insert

### senba

- `clear`, `extend`, `Extend`, `FromIterator` いずれも未実装。
- entry API なし(`Entry` ハンドル相当は存在しない)。
- **`get_or_insert_with(K, F) -> &V`**(〇 追加済み): ミス時のみクロージャ評価、
  ヒット時は VISITED を立てて参照を返す。`try_get_or_insert*` 系は未実装。

### moka

- `entry(K)` / `entry_by_ref(&Q)` の 2 系統 entry API。
- `get_with`, `optionally_get_with`, `try_get_with` および
  `*_by_ref` 版を網羅。
- `invalidate_entries_if(predicate)` で述語ベース一括削除。
- `invalidate_all()` で論理 clear。

### lru

- `get_or_insert`, `get_or_insert_with_key`, `get_or_insert_ref`,
  `get_or_insert_mut*` × 6 種、`try_*` × 6 種。
- `clear`, `resize` を持つ。

### quick_cache

- `get_or_insert_with`, `get_value_or_guard`(同期 / async 版),
  `entry()` API、`replace`, `retain`, `drain`。

### ギャップ

`get_or_insert_with` は 〇 追加済みで、上記 `if let` イディオムは

```rust
let v = cache.get_or_insert_with(k, || compute());
```

に置き換えられる。ただし `try_get_or_insert_with`(`Result` を返すクロージャ)、
`get_or_insert_mut`、`entry(K)` ハンドル API は依然欠けている。
なお `clear()` は 〇 追加済みで、容量・shard 構成・hand を保ったまま全エントリを
drop して空にできる(再ビルド不要)。

---

## 5. 立ち退きリスナー / 通知

### senba

- `insert` の戻り値 `Option<(K, V)>` で「いま evict された 1 件」を伝えるのみ。
- リスナー登録・キュー API は無し。`remove` の戻り値も `Option<V>` のみ。

### moka

- builder の `eviction_listener(...)` でクロージャを登録、各 evict 時に
  `(Arc<K>, V, RemovalCause)` を受け取る。`RemovalCause` で
  Replaced / Size / Expired / Explicit / Pending を区別。
- `invalidate_entries_if` で述語ベース、`run_pending_tasks` で同期化。

### ギャップ

senba は「全 evict を観測する」手段が無い。`insert` の戻り値を見るしかない
ため、`remove`(明示削除で `Drop` するもの)とは経路が分かれる。

---

## 6. 並行モデル

### senba

- `get`, `insert`, `remove` はいずれも `&mut self`(`contains_key` のみ
  `&self`)。
- shard はあるが API レイヤでは shard mutex を露出していない(現状は ST
  ライブラリ)。並行版 `c8` は別実装で本 crate には含まれない。
- `Cache: Send + Sync` は `K, V: Send + Sync` 依存(明示 unsafe impl はなし)。

### moka

- 全公開メソッドが `&self`。内部で per-shard lock + write buffer + scheduler
  を持つ。`Clone` は cheap(Arc 参照カウント増加)。
- `moka::future::Cache` で async 版あり。

### lru

- `&mut self` ベース。`Send`/`Sync` は `K, V, S` の境界に従う。
- 並行アクセスはユーザ側で `Mutex`/`RwLock` で包む前提。

### quick_cache / stretto

- いずれも `&self` で並行アクセス。stretto は内部に OS thread を 2 本持つ
  特殊な構造(eviction policy / write)。

### ギャップ

senba は **構造的に ST 専用 API**。並行で使うなら呼び出し側で
`Mutex<Cache>` する必要があり、shard 並列性は外から取り出せない。

---

## 7. 統計

### senba

- なし。hit / miss / eviction counter は外部で手動カウントするしかない。

### 他

- moka: 公開 API レベルでは `entry_count` / `weighted_size` のみで
  hit/miss は出さない(builder の `name` を付けて metrics 配線するのが
  一般的)。
- quick_cache: `hits()` / `misses()`(`stats` feature gate)。
- stretto: `metrics` フィールドで詳細統計。

### ギャップ

senba は hit ratio / eviction count を **公開 API では一切観測できない**。
本リポジトリの bench 経路は内部状態を直接読んで集計しているが、
ライブラリ利用者には同じ手段が無い。

---

## 8. その他(serialize / 拡張点)

| 機能 | senba | moka | lru | quick_cache | stretto |
|---|---|---|---|---|---|
| Serde 直接サポート | x | x | x | x | x |
| `Default` impl | x | x | x | x | x |
| `From<HashMap>` 等の変換 | x | x | x | x | x |
| 名前付け(`name()`) | x | O | x | x | x |

---

## 9. 「senba::Cache に欠けているもの」一覧

事実として欠けている機能。重要度の主観評価は付けず、列挙のみ。

**Construction / sizing**

- `Cache::builder()` パターン
- カスタム `BuildHasher` の注入(`with_hasher` / builder)
- unbounded mode
- TTL / TTI(time-to-live / time-to-idle)
- 重み付け容量(weight / weigher)

**基本オペレーション**

- `peek(&K)` 〇 / `peek_mut(&mut K)` (mut 版は未)
- `get_mut(&mut K) -> Option<&mut V>` 〇
- `peek_lru` / `peek_mru` / `pop_lru` / `pop_mru` 相当
  (SIEVE では tail/head に対応する `tags[0]` / `tags[len-1]` を露出する余地がある)
- `Borrow<Q>` 一般化(`get<Q>` で `&str` 経由 lookup できる) 〇
- `get_key_value` 系

**バルク / イテレーション**

- `clear()` 〇
- `iter()` 〇 / `iter_mut()` / `keys()` / `values()`
- `drain()`
- `retain(predicate)` / `invalidate_entries_if`
- `IntoIterator` 各種
- `Extend` / `FromIterator`
- `resize(new_cap)`

**Entry API / get-or-insert**

- `entry(K)` / `entry_by_ref(&Q)`
- `get_or_insert_with` 〇 / `get_or_insert_mut` / `try_get_or_insert*`

**通知 / 統計**

- eviction listener(全 evict を hook する API)
- hit / miss / eviction カウンタ

**並行**

- `&self` での `get` / `insert` / `remove`(SIEVE の visited 更新を
  `AtomicU16::fetch_or` 化すれば理論上可能。本 crate では未実装で別系統 `c8` 扱い)
- `Clone`(現在は `Arc<Mutex<Cache>>` で包むしかない)
- `moka::future::Cache` 相当の async 版

**派生 trait**

- `Clone`, `Debug`, `Default`
- 名前(`name()`)
- Serde サポート

---

## 10. メモ

- senba の `insert -> Option<(K, V)>` は lru の `push` と同形のシグネチャ。
  「容量超過したときだけ Some」というセマンティクスは独自で、moka の
  「戻り値なし + リスナー」、stretto の「`bool` で受理可否」とは設計思想が違う。
- `&mut self` を要求する `get` は SIEVE のアルゴリズム由来であり、lru も
  同じ理由で `get` は `&mut self`(LRU 順を更新するため)。**moka /
  quick_cache / stretto が `&self` で済むのは内部に lock + atomic を
  抱えているから**であり、API の差は ST/MT 構造の差にほぼ対応する。
- 本 crate の研究目的は SIEVE 実装の比較・最適化であり、API の網羅は
  目的ではない。本ドキュメントは現状の API 表面を他ライブラリと突き合わせて
  事実として記録するもの。

---

## 11. 更新履歴

- **2026-05-06**: 初版(`docs/reports/2026-05-06-api-comparison-moka-lru.md`
  として作成)。
- **2026-05-06**: `docs/api-comparison.md` に昇格(living document 化)。
  `Cache::peek` / `Cache::iter` / `Cache::get_or_insert_with` を実装し、
  サマリ表 / §2 / §3 / §4 / §9 を 〇 に更新。
- **2026-05-06**: `Cache::clear` を実装。容量・shard 構成・hand を保ったまま
  全エントリを drop する。サマリ表 / §2 / §4 / §9 を 〇 に更新。
- **2026-05-07**: `Cache::get_mut` を追加(VISITED を立てる、`get` と同じく
  `&mut self`)。参照系メソッド (`get` / `get_mut` / `peek` / `contains_key` /
  `remove`) を `K: Borrow<Q>, Q: Hash + Eq + ?Sized` で一般化し、
  `Cache<String, V>` を `&str` で引けるようにした。perf-gate (`insert_u64` /
  `mixed_u64` / `insert_string`) は全シナリオで no change。サマリ表 / §2 /
  §9 を 〇 に更新。
