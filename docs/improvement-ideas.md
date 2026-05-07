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
