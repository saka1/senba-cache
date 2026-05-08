# SIEVE 実装 改善アイディア (living doc)

このドキュメントは date-stamped レポートではなく、現時点で活きている改善案の倉庫。
日付つきの実験レポートは `docs/reports/` 側を参照する。
旧名 `2026-05-04-improvement-ideas.md` を起源とし、以降の進捗をその場で反映する。

CLAUDE.md の通り、本リポジトリの当面の目標は「sandbox の研究結果を `senba::Cache`
(`src/lib.rs`) という publishable な crate API に収束させる」ことに移った。
そのため本ドキュメントの関心も **「ライブラリ surface に関する設計判断」+「c8 系列の
並行版継続」+ ST 路線で残った最適化** の 3 点に再集約している。アーカイブ的な過去の
ブレスト原文 (A〜J 章、M 章原文) は削除した — 必要なら git log と
`docs/reports/2026-05-0[3-5]-*.md` を参照する。

## 状況サマリ (〜2026-05-07)

| 系列 | 位置付け | 主レポート |
|---|---|---|
| `sieve_orig` | 著者参照 (NSDI'24) の忠実ポート。**oracle (eviction 列の正解) 専用**、速度比較対象としては既に陳腐化 | `sieve-orig-overhead-analysis` |
| `sieve_v0`〜`v3` | 連結リスト → array 化の試行錯誤。**いずれも orig に劣る or 同等で系列終了**、artifact として残置 | `v3-bench`, `v3-profile` |
| `sieve_j3`〜`j7` | Map 廃止 → set-associative → tag u16 化、と j8 に至る研究系列。artifact として残置 | `sieve-j3-bench`, `j5-twitter-pareto`, `j7-twitter-pareto` |
| `sieve_j8` | M5.3 + tag 内 ID embed + free_list 廃止 + BLSR×2 + sizeof-aware layout + c-hoist。**inline 20 B/cap で orig (25) を抜きつつ Twitter 5 cluster 中 4 cluster で throughput 勝ち** | `j8-twitter-pareto`, `j8-c-hoist`, `j8-vs-mini-moka-twitter`, `st-twitter-5cluster` |
| **`senba::Cache`** (`src/lib.rs`) | j8 の研究結果を `Cache<K, V, SlotSize, SHARDS, BuildHasher>` として抽象化した **ライブラリ surface**。`SlotSize` で任意 sizeof(Entry)、`Borrow<Q>`、`peek`/`iter`/`retain`/`stats` 等の公開 API を装備。compact 撤去 → shift-on-evict 化済み | `senba-sievecache-design`, `sieve-cache-shift-on-evict`, `senba-twitter-string-sweep`, `docs/api-comparison.md` |
| `sieve_c8` | `sieve_j8` の並行版 (read lock-free + write per-shard Mutex、seqlock-via-tag)。**cap=16384 / SHARDS=256 / T=16 で moka 0.12 比 15.4×、c8 は near-linear (5.99×)** | `c8-design`, `c8-vs-moka-thread-sweep` |
| `sieve_c9` | `senba::Cache` を per-shard `Mutex<Shard>` で wrap した並行版。1T と低 skew では c8 より速いが skew=1.2 / 16T で c8 比 1/8.7 に崩壊 | `c9-design`, `c8-vs-c9-thread-sweep` |
| `sieve_c10s`〜`c14s` | 単一 shard testbed 上で reader/writer の天井を順に剥がした系列。c11s で reader visited ping-pong を構造除去、c14s で Path A lock-free + adv-hot HR 修復まで到達。残課題は (i) Path B/C writer Mutex 競合 (T1 uniform read-heavy 16T fail)、(ii) seqlock-via-tag 由来の false-miss と read-only adv-hot -22% 退行 | `single-shard-baseline`, `c10s-vs-c8-baseline`, `c11s-conditional-set`, `c12s-cas-slot-claim`, `c13s-sweep`, `c14s-design`, `c14s-sweep` |

ベースラインは ST が `senba::Cache` (= j8 の API 化版)、並行が `sieve_c8`。
以降の改善は基本的にこの 2 つを起点に評価する。

---

# A. ライブラリ surface (`senba::Cache`)

`docs/api-comparison.md` が moka / lru / quick_cache / stretto との API 横断比較
+ 進捗トラッカーを担う(living document)。本章は「API 拡張の優先度判断」と
「surface 内部の構造的最適化」に限定する。

## A1. 公開 API の残ギャップ

`api-comparison.md` §9 の「欠けているもの一覧」のうち、本 crate のスコープ
(SIEVE 比較研究の延長で薄く配る) で **追加候補**:

- **`drain()`**: consume 系の唯一の埋めるべきギャップ (値所有 `IntoIterator` は
  `drain().collect()` の sugar 以上にならないので別扱い)
- **`pop_oldest()` 相当**: SIEVE は LRU/MRU を持たないが、フラット配列の `tags[0]`
  が `sieve_orig` の "tail (oldest)" と対応するので、構造上 1 件露出は可能
- **`get_or_insert_mut` / `try_get_or_insert*`**: 既存 `get_or_insert_with` の拡張
- **`resize(new_cap)`**: per-shard で trim する形だと SIEVE の hand 不変条件と
  どう接続するか要検討

スコープ外として **意図的に見送り**:

- TTL / TTI、weighted size、eviction listener: thin SIEVE crate の路線から外れる。
  rich 系は将来別 facade で扱う。
- builder パターン: 現状ジェネリック軸 (SlotSize / SHARDS / BuildHasher) で吸収できる
  ため不要。
- `FromIterator<(K, V)>`: senba の `capacity` は eviction policy のパラメータで
  あって「全部入れる箱」のサイズではないため、`size_hint` 起点で容量を推定する
  semantics は誤誘導になりやすい。
- async API: c8 (or その後継) で `&self` 並行 API を入れた後の派生。

## A2. surface 内部の構造的最適化

`senba::Cache` は j8 の throughput 性質を概ね保っているが、shift-on-evict 化で
構造が変わったため再評価が要るもの:

- **A2.1 c-hoist trick の再確認**: `tag & ID_MASK = id × S::SIZE` は SLOT 単位で
  保たれている (`sieve-cache-shift-on-evict` で perf-gate 改善)。が、shift で
  `tags` 配列が `cap` まで縮んだ事による SIMD scan 窓の縮小 (旧 j8 比 半分) が
  inner ループの cycle 配分にどう影響したかは詳細プロファイル未実施。
- **A2.2 tags 配列の prefetch / chunk overlap**: `j8-c-hoist` レポートで示唆した
  「inner 単独最適化は打ち止め、次は load latency hide」の先。`tags` の SIMD
  scan で次 chunk を `_mm_prefetch` で投げる、または 2 chunk を交互に走らせて
  port を分散する案。Path A (見つかった) と Path B (見つからない) の cycle
  バランスが c-hoist で 14:5 になっている現状から、Path A 側を更に縮める余地。
- **A2.3 `hand` の管理**: shift-on-evict 化で `hand = if pos < last { pos } else { 0 }`
  となっているが、この分岐自体の cost と、複数 evict が連続するときの hand
  動線について未検証。samply での再プロファイルが必要。
- **A2.4 large V (`[u8; 256]` 等) での hot/cold split**: `Slot32` default では
  V が大きい型は上のブラケット (`Slot64`) に逃すため、tags scan は L1 残留する。
  ただし `entries` の cache miss が支配的になる帯域は未測定で、E3 系 (旧アイディア
  の hot/cold split) の効きを `senba::Cache` の SLOT 軸で再評価する余地。

## A3. perf-gate の維持

`benches/sieve_cache_perf.rs` は library surface の安定契約。`micro.rs` での実験
変化が perf-gate を踏まないように、新 API 追加時は **必ず 3 シナリオ
(`insert_u64` / `mixed_u64` / `insert_string`) で baseline 比較**を行う。
±2-3% noise、5% を超える regression は調査対象。

---

# B. 並行版 SIEVE (`sieve_c8` 系列)

`c8-design` で **seqlock-via-tag** (j8 の u16 tag を seqlock の sequence 兼
locator として再利用) が成立、`c8-vs-moka-thread-sweep` で moka 0.12 比
15.4× scaling が示せた。次は研究軸を伸ばす方向。

## B1. c8 派生変種

| ID | 内容 | 動機 | 優先度 |
|---|---|---|---:|
| **B1.1 `c8a`** | `V: Clone` 化のため内部 `Arc<V>` wrap、reader は Arc clone (refcount inc) のみで生 V に触れず | `String` 等 owned 型を並行 cache に乗せる。`K, V: Copy` 制約を外す | 高 |
| B1.2 `c8e` | `crossbeam_epoch` で writer の drop を遅延、reader は raw 参照保持 | `V: Clone` 任意で zero-copy read | 中 (複雑度高) |
| B1.3 `c8s` | SIMD scan を Rust メモリモデル上完全合法な形 (`portable-simd` 経由 or `AtomicU16` 配列を 16 回 `load(Relaxed)` 相当) に書き換え | 現状 c8 の「`AtomicU16` 配列を `*const u16` cast で SIMD load する」灰色領域を解消 | 中 |

## B2. 並行ベンチ sweep

`c8-vs-moka-thread-sweep` §拡張 が cap=16384 / SHARDS=256 / T={1,2,4,8,16} の
1 軸 sweep。残る軸:

- **B2.1**: SHARDS ∈ {8, 32, 64, 256} × per-shard contention 飽和点を地図化
- **B2.2**: skew ∈ {0.6, 0.8, 1.0, 1.2, 1.4} で hot key 集中度を変えた scaling
- **B2.3**: read-heavy 比 (95% read / 5% write) — production cache の典型分布
- **B2.4**: 96-core / NUMA 機での scale 上限 (現状は 12-core i5 が上限)
- **B2.5**: `concurrent_invariants_under_zipf` の long-running soak (phantom tag 修正後の
  不変条件を threads ↑ ops ↑ で確認)

## B3. 並行 API の library surface 化

c8 系列が固まったら、`senba::Cache` と二系統のままにせず **`&self` で動く並行 API
を本 crate の library surface に取り込む** 道を別スペックで議論する予定
(`senba-sievecache-design` §scope 外として明記)。`SlotSize` ジェネリックと
seqlock-via-tag の同居が論点。

---

# C. ST 路線で残る最適化

A2 と独立に、`senba::Cache` (= j8 ロジック) の hot path に対する微細最適化:

- **C1. j8 退行帯の解消**: `st-twitter-5cluster` で **Zipf ≥ 1.79 かつ cap ≥ 16384**
  の高 hit ratio 帯で +0.5〜+4.9 ns 退行が出ている。`j8-candidate-loop-analysis`
  の Δ単一式 `5cy × N_cand + 7cy × ΔN_false` の右辺を縮める手は per_shard 縮小
  (16) で実施済 (`sieve-j8-bench` §8) だが、cap=65536 のような実用大 cap では
  per_shard=16 にすると SHARDS=4096 まで膨らむため別軸必要。
- **C2. shift-on-evict のさらなる最適化**: `tags.copy_within(pos+1..len, pos)` は
  per evict で O(shard 内残数) — per_shard ≤ 64 なので絶対量は小さいが、頻度が
  高い steady-state で測ると効く可能性。SIMD memmove / unrolled copy / 「shift
  方向反転で常に短い側を動かす」等。
- **C3. `Borrow<Q>` 経路の余計な hash 呼び**: `get<Q>` では key 比較を `K: Borrow<Q>`
  経由でやっているが、hash 計算は `H<Q>` で 1 回のみ。perf-gate に乗っているので
  退行は無いが、generic 化で codegen が膨らんでいる可能性は要 objdump 確認。

---

# D. 計測軸の拡張

過去レポートで未消化の workload 軸:

- **D1. skew=0.5 (uniform 寄り)**: eviction が支配的で SIMD scan の真価が出る条件。
  `j5-pershard-pareto` で skew ∈ {0.9, 1.0, 1.2} まではある。
- **D2. 大きい V (`[u8; 256]`, `Vec<u8>` 等)**: hot/cold split の効きと、`SlotSize=Slot64`
  ブラケットでの cache footprint 影響を測る。
- **D3. churn-heavy** (req >> capacity × 100): orig の linked-list pointer chasing
  悪化 / array が逆転する条件を狙う。
- **D4. mixed read/write 比率**: 現状 micro bench は insert_only。`get`-only /
  `mixed` の 3 種で hit-path コストの相対重要度がどう動くか。

`bench_concurrent.rs` は read-write 比 を変えられるので B2.3 と D4 は実質統合可能。

---

# E. 兄弟アルゴリズム / 比較対象拡張

- **E1. S3-FIFO 移植**: SIEVE の兄弟 (Yang 2023, NSDI'24 同チーム)、論文では
  多くの workload で SIEVE と同等以上 HR。実装 2-3 日。比較対象として価値高い。
- **E2. moka 0.12 / mini-moka 0.10 の追測**: `j8-vs-mini-moka-twitter` §10 で
  Zipf + Twitter 5 cluster までは比較済。残るは大規模 cap (cap=1M〜) と
  W-TinyLFU が SIEVE を抜く条件の探索。
- **E3. libCacheSim FFI で C 実装を直に benchmark に取り込む**:
  `sieve-c-vs-senba-twitter52` で「C 実装は libCacheSim ハーネス込みで 4-6× 遅い」
  が判明、純 algorithm 比較には FFI 取り込みが要る。

---

# F. 棄却 / 実装済 (履歴)

| ID | 内容 | 結末 | 参照 |
|---|---|---|---|
| A1 | hasher 統一 (FxHash / XXH3) | 実装済: 全 variant XXH3、`senba::Cache` は `with_hasher` で注入可 | `src/hash.rs` |
| C2 / E1 | `Vec<Option<Node>>` → `Vec<MaybeUninit<Node>>` | 実装済 (orig)、bench はノイズ範囲だが構造的正しさで採用 | `sieve-orig-overhead-analysis` |
| J2 | set-associative (Map 廃止 + hash 直接 segment) | 実装済 = `sieve_j4`、以降の j5/j7/j8 全て継承 | `sieve-j4-set-associative` |
| J3 | 全 inline (Map 廃止 + tag SIMD scan) | 実装済 = `sieve_j3` | `sieve-j3-bench` |
| J5 派生 | double-hash 排除 | 実装済 = `sieve_j5` | `sieve-j5-doublehash-ab` |
| M2.1 | visited を tag MSB に同居 (j6) | **棄却**: tag bit 削減で false-match 率倍増、Twitter 全帯域で +2.5〜+11.3 ns 退行 | `sieve-j6-m21-twitter` |
| M2.3 | tag を u16 化 (j7) | 実装済 → j8 に発展継承 | `sieve-j7-m23-twitter` |
| M5.3 | slack を tags 側のみに、+ tag 内 entry_id embed | 実装済 = `sieve_j8` (inline 20 B/cap で orig 抜き) | `j8-twitter-pareto` |
| M1 系 (slack 削減) | `order_cap = 2 × cap` の slack | **構造ごと撤去**: `senba::Cache` で shift-on-evict 化 → `tags` 配列が `cap` に半減 | `sieve-cache-shift-on-evict` |
| B1 (visited inline) | visited bit を Entry に inline | 旧 v3 系列のアイディア。j 系列では tag 内 inline で恒常的に解決済 | (該当 j7/j8 の tag layout) |
| F1 (S3-FIFO) / F2 (W-TinyLFU) | 兄弟アルゴリズム比較 | F2 部分達成: mini-moka / moka 0.12 (= W-TinyLFU 系) との Twitter 5 cluster 比較済。F1 (S3-FIFO 移植) は未実施で **E1 に再掲** | `j8-vs-mini-moka-twitter` |
| compact トリガ緩和 (D1 旧) | 旧 j 系列の compact 頻度 | shift-on-evict で **compact 自体撤去** | `sieve-cache-shift-on-evict` |
| jedisct1/rust-sieve-cache 比較 | 既存 Rust 実装の追評 | 設計調査で oracle 不一致 (CLOCK 寄りに縮退) と判明、詳細ベンチ見送り | `jedi-vs-orig` |
| c12s install-at-evicted-pos | writer Mutex 完全排除案 | **棄却**: 新 entry が hand 直前 visited=0 で入る → 即 evict 候補、SIEVE 保護期間が消滅し HR -10〜-72%、algorithm 等価性破壊 | `c12s-cas-slot-claim` |

---

# G. c15 以降の方向 — CAS state の再設計と writer lock-free 化

`sieve_c10s` 〜 `sieve_c14s` の単一 shard sweep で見えた 2 つの構造的天井
((i) Path B/C writer Mutex 競合、(ii) seqlock-via-tag 由来の false-miss と
それに伴う retry / SIMD overhead) に対する攻め筋を整理する。本章は
entry-level seqlock 案 (`2026-05-08-c14s-design.md` §7) を含む複数の design
lane を並べて比較するための living section。

## G0. 背景 — なぜ c14s で頭打ちになったか

c14s は Path A (= 既存キーの value 更新) のみを lock-free 化し、Path B/C
(= warmup install / evict) は writer Mutex 配下に残した。その結果、§4
acceptance で T1 (uniform read-heavy 16T) が c11s 比 0.323 で fail のまま。
これは **c14s 固有のバグではなく Path B/C の構造的競合**で、c14s スコープ
では解けない。

加えて c14s は false-miss を bounded retry (racing 観測時のみ) で実装内に
隠したが、これは対症療法。seqlock-via-tag 構造を取る限り Path A の VERSION
flip は reader scan に副作用を落としつづける。read-only adv-hot 16T で
c13s 比 -22% の退行が出ているのは、retry 自体ではなく retry 必要性を
判定するために導入した EMPTY-lane 検出 SIMD overhead (`_mm256_cmpeq_epi16
(masked, zero_v)` + movemask + live_lanes mask) の固定コストが pure cost
として乗っているため。

これら 2 つを構造的に解くためには、**(i) tag が抱え込んでいる責務の分離**、
**(ii) writer の lock-free 化と SIEVE 等価性の両立**、の 2 軸の再設計が要る。

## G1. tag が背負っていた 4 つの責務

c8〜c14s 系列で `AtomicU16` tag に詰まっていたもの:

| 責務 | 用途 | 主に触る経路 |
|---|---|---|
| (1) scan ニードル | LIVE bit + 14-bit hash partial key | reader AVX2 SIMD scan |
| (2) id 参照 | entries 配列 index、c-hoist で SIMD と兼用 | reader / writer 両経路 |
| (3) 存在ビット | LIVE / EMPTY 判定、所有権 lock | writer CAS |
| (4) 同期通知 | VERSION bit (c13s/c14s)、seqlock sequence | reader 検証 + writer Path A |

reader scan が欲しいのは (1)(2) のみ、Path A の CAS が触りたいのは (3)(4)
のみだが、すべて同 word に同居しているため以下が起きる:

- **scan が同期 churn に巻き込まれる**: Path A が (4) を flip するたびに
  tag cache line が Modified に遷移し、reader の AVX2 load が cache-line
  ping-pong を踏む。
- **reader の seqlock 検証 (`t1 == t2`) が Path A の VERSION flip に反応
  → API 表面で false-miss**: 意味的に hit だった key を miss と返す。c14s
  の bounded retry はここを実装内で埋め戻す対症療法。
- **scan-clean が崩れる**: c11s で visited を別配列に出して取った reader
  scaling の利得が、Path A の CAS by tag によって部分的に毀損する。

c11s で「visited は別配列に出して正解だった」と確認できた方向の自然な延長は、
**「同期通知 (4) も別 word に出す」**。c15 系列の design lane はこの分離を
どう実現するかで分かれる。

## G2. CAS state の再設計案 (同期通知層の分離)

### G2-α. entry-level seqlock — `c14s-design.md` §7 案

#### 構造

```rust
struct Entry<K, V> {
    version: AtomicU32,  // 偶数 = stable, 奇数 = in_progress
    key: K,
    value: V,
}
```

tag は `LIVE | id | hash` のみで、Path A では **不変**。同期通知は entry 側の
`version` フィールドが担う。

#### state machine

```
Path A (既存キー update):
  pos, id = find by tag scan          ← tag は変化しない
  CAS(entry[id].version, even → odd)  ← 所有権獲得
  entry[id].value = new_value
  store(entry[id].version, odd + 1)   ← 偶数に戻す = release

reader:
  pos, id = tag scan
  v1 = load(entry[id].version)        ← 奇数なら spin
  snapshot = entry[id]                ← key + value を読む
  v2 = load(entry[id].version)
  if v1 == v2 && even: return Some(value)
  else: 内部 retry (上限つき)
```

#### なぜ効くか

- **tag scan が完全に scan-clean** になる。Path A の活動は entry にしか及ば
  ないので、AVX2 scan 路の cache line は writer-vs-reader で取り合わない。
- **false-miss が API 表面から消える**: Path A の VERSION flip が tag に
  存在しないため、`t1 == t2` 検査が Path A に反応しない。reader が見るのは
  entry 側の version で、ここでの不一致は実装内 retry に閉じる。
- **AVX2 scan の改造不要**: scan ニードルの bit layout が c14s と同じなので、
  既存の `find_get_avx2` を流用できる。c14s で導入した EMPTY-lane 検出 SIMD
  overhead を **削除できる** (false-miss が出ないので racing 検出自体が不要)
  → read-only adv-hot 16T の -22% 退行を回収する候補。

#### 何を犠牲にするか

- **entry サイズ +4B**: `Entry<u64, u64>` は 16B → 24B (padding 次第)。
  Slot16 を維持するなら entry 側に padding が乗る、Slot32 ブラケット同居で
  自然吸収するなら Slot 選択が変わる。perf-gate `insert_u32_slot16` への
  影響は要再測定。
- **Path C (shift-on-evict / install) は依然 tag を動かす**: install 時 / shift
  時 / evict 時には tag を書き換えるので、reader scan に対して Path C 用の
  seqlock-via-tag (= 現状の c14s 構造) は依然必要。**2 段 seqlock 構造**
  (tag 側 = Path C 用、entry 側 = Path A 用) になる。
- **Path B/C の Mutex は残る**: G2-α は同期通知層の再設計であって、writer
  state machine の lock-free 化は別途 (G3 系) が要る。T1 fail は α 単独
  では解けない。

#### どの workload で効くか

- **adversarial-hot read-heavy**: c14s が bounded retry で取り戻していた HR
  をゼロコスト (= retry 自体不要) で確保。c14s/c13s = 0.78 の退行も α では
  発生しない。
- **uniform read-only**: false-miss 観測時のみの retry も EMPTY-lane 検出も
  不要なので、c14s で残った SIMD overhead を構造的に外せる。

### G2-β. state を別 atomic に切り出し (multi-word state、SoA 化)

#### 構造

```rust
struct Shard<K, V, S> {
    tags:    AlignedTags,             // [u16] = LIVE | id | hash (scan 専用、Path A で不変)
    slots:   Box<[AtomicU32]>,        // version + lock-bit + epoch (同期/所有権専用)
    visited: Box<[AtomicU64]>,        // c11s 由来 (bit-packed)
    entries: UnsafeCell<Box<[MaybeUninit<Entry<K, V>>]>>,
}
```

α が version を entry の中に置いたのに対し、β は **shard レベルの並列配列**
として slots 配列を維持する。c11s が visited bit を tag から分離したのと
同形の手で、同期通知だけを更に分離する。

#### state machine

```
Path A:
  pos, id = find by tag scan          ← tag 不変
  CAS(slots[pos].lock_bit, 0 → 1)
  entries[pos].value = new_value
  store(slots[pos].version, version + 1)
  store(slots[pos].lock_bit, 0)

reader:
  pos, id = tag scan
  v1 = load(slots[pos])               ← version + lock_bit を 1 word で取得
  snapshot = entries[pos]
  v2 = load(slots[pos])
  if v1 == v2 && lock_bit == 0: return Some
```

#### なぜ効くか

- **α と同じ false-miss 解消** + **entry サイズが膨らまない**: small V (`u64`)
  の cache 利用密度が落ちない。Slot16 を維持できる。
- **`find-avx2-frontier` Tier-B (B1 SoA tag split) と統合可能**: 将来 tag を
  更に scan 専用 byte 列 (1B/lane) と id/aux 列に分けるなら、slots 配列は
  その自然な置き場になる。AVX-512 V5 (`find-avx2-avx512.md`) の path とも
  整合する。
- **prefetch 戦略の自由度**: tags / slots / entries が独立配列なので、reader
  hot path の prefetch を tag → slots → entries の 3 段で打てる。

#### 何を犠牲にするか

- **slots 配列の cache 占有**: cap=64 で `Box<[AtomicU32]>` = 256B = 4 cache
  line。tags (128B) と entries (1〜数 KB) と並ぶ第三の hot 配列が増える。
- **reader load が 1 本増える**: scan 候補 1 件あたり tag (2B) + slots (4B)
  + entry の合計 3 load。α (entry 同居) より cache hit 数が +1。
- **AVX2 scan 自体は変わらない**: scan が見るのは tag のみで、`find_get_avx2`
  は α と同じ。slots はスカラー検証側でのみ load される。

#### どの workload で効くか

- α と同じ workload プロファイル + **Slot16 の維持が要件のとき (small V cache)**
  に α より優位。
- 将来の AVX-512 / SoA tag split / PEXT 経路と統合するなら β が筋。長期
  ポートフォリオ的には α より β の方が他の最適化と組み合わせやすい。

### G2-γ. tag を 2-word 化 (low-half scan + high-half sync)

#### 構造

```rust
tags: Box<[AtomicU32]>,  // [low 16 bit = scan ニードル | high 16 bit = version]
```

scan が見るのは low 16 bit のみ、CAS は full-word でも上半身だけでも書ける。
α (entry 同居) と β (別配列) の中間で、**tag と version の cache locality を
保ちつつ scan 範囲を絞る** 設計。

#### なぜ効くか

- **scan + 同期検証が同 cache line で完結**: candidate 1 件の検証で 2 load
  (α/β) ではなく 1 load (full-word) で済む。
- **prefetch は tags 配列 1 本だけで OK**: locality 損失が α/β より小さい。

#### 何を犠牲にするか

- **AVX2 scan で `vpand` で low 16 bit を抜く工数 1 命令増**。
- **lane 数が半分**: per_shard=64 のとき、現状 4 chunk (16 lane × 4) → 8
  chunk (8 lane × 8) に倍増。outer ループ反復が増え、特に AVX-512 path で
  V1/V2 の zmm 1-shot 比較が崩れる。
- **AVX-512 との相性が悪い**: V1 (kmask 直結) は 16 lane × u16 が前提なので、
  γ は scan 範囲が異なる新 dispatch を要する。

#### γ の位置づけ

α/β に対して **微小な locality 利得** と引き換えに、AVX-512 / PEXT 経路の
dispatch が崩れる。**長期的には α か β に分があり、γ は採用しない方が良い**。
本章では選択肢として並べるが、優先度は最も低い。

## G3. Path B/C lock-free 化 (writer state machine の再設計)

c12s (`2026-05-08-c12s-cas-slot-claim.md`) で **install-at-evicted-pos** を
試したが、SIEVE algorithm 等価性が崩壊して不採用になった。理由を構造として
取り出すと:

```
sieve_orig: 新 entry は LRU list の HEAD に挿入 (= hand から最も遠い側)
            hand は TAIL から HEAD へ進む
            → 新 entry は最低 cap 規模 cycles 保護される

c12s: 新 entry は evict した pos = hand 直前にそのまま install
      visited=0 で入る → 次 hand wrap で即 evict 候補
      → 保護期間が消滅 = 「保護期間が短い CLOCK 亜種」になる
```

要件は **新 entry が hand から構造的に距離 d ≥ K cycles 離れていること**。
G3 系の案はこの距離保証をどう成立させるかで分岐する。

### G3-δ. install-at-(hand + cap/2)

#### 動作

```
hand: AtomicUsize
new_pos = (hand.load() + cap / 2) mod cap
if tags[new_pos] == EMPTY: install
else: 近傍を線形に EMPTY 探索
      なければ FAA(hand) で 1 evict 進める
```

新 entry は常に hand から構造的に **cap/2 距離** 離れた位置に入る。sieve_orig
の HEAD 挿入 (= hand から最も遠い) を array で擬似する形。

#### なぜ効くか

- **install と evict が pos 距離 cap/2 離れる** ので、両者の CAS が同 cache
  line を取り合わない。複数 writer の **空間的並列性** が立つ。
- **保護期間 ≈ cap/2 cycles が構造的に保証される** (hand が cap/2 進むまで
  当該 entry に到達しない)。

#### 何を犠牲にするか

- **sieve_orig との eviction 列 byte-exact 一致は崩れる**: HEAD 挿入の
  semantics は同じだが、hand の進め方や install pos の決定論性が array 側で
  変わる。oracle test の合格基準を「eviction 列ぴったり一致」から「HR の
  有意差なし」に緩める必要がある。
- **EMPTY 探索が線形**: cache full に近い steady-state で、(hand + cap/2)
  近傍に EMPTY が無いと線形探索が走る。worst case で O(cap)。
- **HR の劣化リスク**: cap/2 距離は *統計的に* hand から遠いだけで、特定の
  workload では sieve_orig と HR が乖離する可能性。実 trace cross-check が要。

### G3-ε. 退避 free-list 経由の遅延 install

#### 動作

```
free_list: lock-free MPMC queue (e.g. Vyukov bounded queue)
evict:
  pos = FAA(hand) で hand 進める
  evict tags[pos]
  free_list.enqueue(pos)
install:
  pos = free_list.dequeue()  ← 最も古い free pos を取る
  tags[pos] = new_tag
```

evict した pos を即 install せず、**queue を介して時間的に decouple** する。
queue が深いほど、install と evict が時間的に離れ、保護期間が delays 分だけ
伸びる。

#### なぜ効くか

- **install と evict の時間的競合が消える**: install は queue 先頭、evict は
  hand 進行と各々独立な CAS で進む。
- **保護期間 ∝ queue 深さ**: queue が空に近づかない限り、install pos は
  evict されてから queue 通過時間ぶん前のもの = hand から距離が確保される。

#### 何を犠牲にするか

- **queue 自体が contended**: lock-free MPMC queue (Vyukov / BAQ 等) 自体が
  hot で、ここが新たなスケーリング天井になる可能性。
- **queue 深さの管理**: 浅すぎると保護期間が短い (= c12s 同様の劣化)、
  深すぎると capacity 上の有効 entry 数が減る (= queue に積まれた pos は
  使えない)。動的調整が要る。
- **実装複雑度**: lock-free queue + 全体の memory ordering を含めると、
  c14s までで最も複雑なコードになる。

### G3-ζ. per-shard sub-sharding

#### 動作

1 shard を **K 個の sub-shard × cap/K** に分割。各 sub-shard は独立に SIEVE
state machine (hand / tags / visited / entries) を持ち、独自 Mutex で writer
を排他化する。reader は `(key.hash >> shard_bits) & (K-1)` で sub-shard を
選び、そこに対して通常の reader path を走らせる。

#### なぜ効くか

- **Mutex 競合が 1/K に**: K 個の独立な writer queue になり、確率的に同時
  writer が異なる sub-shard に分散する。
- **構造的に SIEVE 等価が自明**: 各 sub-shard は単独で sieve_orig 等価な
  state machine。sub-shard 横断の eviction 順序は外側で定義しないので問題に
  ならない (HR は sub-shard 単位の SIEVE が決める)。
- **実装が薄い**: c14s をほぼそのまま K 個並べるだけ。新規 algorithm は不要。

#### 何を犠牲にするか

- **HR の劣化リスク**: 1 つの shard を K 個に切ると、ある sub-shard では
  cold key が evict された一方で別 sub-shard では hot key が空席を残す、
  といった「sub-shard 局所最適 vs shard 全体最適」のずれが出る。実 trace
  cross-check 必須。
- **per-sub-shard cap が小さくなりすぎるリスク**: 既に SHARDS=256 で
  per_shard=64 なら、K=4 で per-sub-shard=16。AVX2 scan 1 chunk で完結する
  最小サイズになり、それ以下だと SIMD 利点が消える。
- **sub-shard 選択 hash の追加 cost**: shard 選択 hash の bit を更に切り
  分けて使うか、別 hash を計算するかで実装が分岐。

### G3 案の比較

| 案 | lock-free 度 | SIEVE 等価 | 実装複雑度 | HR 劣化リスク |
|---|---|---|---|---|
| δ install-at-(hand+cap/2) | full | 統計的等価 (要検証) | 中 | 中 |
| ε free-list 経由 | full | 構造的に保護期間担保 | 高 | 低〜中 |
| ζ sub-shard | per-sub-shard Mutex 残 | sub-shard 単位で自明 | 低 | 中 |

writer の Mutex を完全消去するなら δ または ε、簡素な構造で 1/K 化するだけで
十分なら ζ。c14s から地続きで進めるなら ζ が圧倒的に薄い。

## G4. 推奨着手順と組み合わせ戦略

優先度は (a) 効きが大きい × (b) 実装複雑度が低い × (c) 既存 c14s 資産を
壊さない、の 3 軸で評価:

| 順位 | 案 | 効く課題 | 着手難度 |
|---:|---|---|---|
| 1 | α entry-level seqlock | adv-hot HR (構造除去) + read-only adv-hot -22% 退行回収 | 低〜中 |
| 2 | ζ sub-shard | T1 uniform read-heavy 16T fail を 1/K 化 | 低 |
| 3 | β SoA state | α の long-term 上位互換、AVX-512 / PEXT 統合の前提 | 中 |
| 4 | δ install-at-(hand+cap/2) | T1 を更に押し下げる (Mutex 完全消去) | 中〜高 |
| 5 | ε free-list 経由 | δ で HR が崩れた場合の代替 | 高 |
| - | γ 2-word tag | 採用しない (AVX-512 dispatch が崩れる) | - |

### 組み合わせ戦略

α と ζ は **直交** (α は同期通知層の分離、ζ は state machine 多重化) なので、
**両方乗せられる**。c14s に対して:

1. まず α を入れて false-miss を構造除去 + adv-hot read-only -22% を回収
2. 次に ζ を入れて T1 を 1/K 化
3. (1)+(2) で頭打ちが見えたら β に進化、または δ で writer Mutex 完全消去

c12s の教訓 — **「lock-free と SIEVE 等価は trade-off ではなく両立が条件」**
— を踏まえると、δ/ε の install 位置を動的に決める手は oracle 検証が hairy。
**まず α + ζ で 2/3 を取り、残りは AVX-512 V1 (`find-avx2-avx512.md`) のような
直交手で削る** のが堅い順序。

## G5. open questions

議論の起点として残しておく未解決問題:

- α の `version` 増分は `AcqRel` ordering で十分か、それとも fence が要るか
  (Loom 検証が要件)。
- ζ の sub-shard 数 K は静的ジェネリックか動的か。SHARDS と同様にジェネリック
  パラメータで吸収するなら `Cache<K, V, S, SHARDS, SUB_SHARDS, H>` まで膨らむ。
  デフォルトをどう取るかが API 設計の論点。
- β の slots 配列の word サイズは `AtomicU32` か `AtomicU64` か。`AtomicU64`
  なら version (32 bit) + lock_bit (1 bit) + epoch (31 bit) で ABA 防止 epoch
  を抱えられる。`AtomicU32` ではコンパクトだが ABA リスクが残る。
- δ の保護期間 cap/2 が NSDI'24 の "1 cycle 保護" に必要十分か、論文の証明を
  array semantics に当てはめて再検証が要る。
- α と c14s の bounded retry を **共存させる** 価値はあるか。Path C (shift-on-
  evict) の seqlock-via-tag は α でも残るので、Path C 経由の false-miss は
  依然出る。retry の保険として残すか、α に全乗っかりして取り除くか。
