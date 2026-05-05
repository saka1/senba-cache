# SIEVE 実装 改善アイディア (living doc)

このドキュメントは date-stamped レポートではなく、現時点で活きている改善案の倉庫。
日付つきの実験レポートは `docs/reports/` 側を参照する。
旧名 `2026-05-04-improvement-ideas.md` を起源とし、以降の進捗をその場で反映する。
関心の中心は **M 章「j5 メモリフットプリント削減」**。下方の A〜J は
orig/v3 ベースライン時代のブレスト原文を履歴として温存している。

## 状況サマリ (〜2026-05-05)

| 系列 | 結論 | 主レポート |
|---|---|---|
| `sieve_orig` | 著者参照 (NSDI'24) を `Vec<MaybeUninit<Node>>` 化した忠実ポート。oracle と速度ベースラインの両用 | `sieve-orig-overhead-analysis` |
| `sieve_v0`〜`v3` | 連結リスト → array 化 + bit-parallel scan + 2-pass 等。**いずれも orig に劣る or 同等**で系列終了 | `v0-divergence`, `v3-bench`, `v3-profile` |
| `sieve_j3` (旧 J3) | 外部 HashMap 廃止、tag SIMD scan 全 inline。**cap≤1000 で orig 比 0.7×** | `sieve-j3-bench` |
| `sieve_j4` (旧 J2) | j3 を 8 shards 並べた set-associative。per_shard sweet spot ∈ [32, 64] | `sieve-j4-*` (3 本) |
| `sieve_j5` | j4 から double-hash 排除 (`j3::*_with_hash`)。Δ ≈ −7 ns/op 安定。**Twitter trace で orig を二重勝ち**するセル発見 | `sieve-j5-doublehash-ab`, `j5-pershard-pareto`, `j5-twitter-pareto` |
| memfair | j5 inline 34 B/cap vs orig 25 B/cap。「j5 の優位は memory hand-out 由来でない」を確定 | `j5-vs-orig-2x-memfair` |
| `sieve_j6` (M2.1) | visited を u8 tag の bit6 に同居 (= 6-bit hash)。Twitter cluster018 全 9 cell で j5 比 +2.5〜+11.3 ns 劣化 | `sieve-j6-m21-twitter` |
| `sieve_j7` (M2.3) | tag を u16 化 (live+visited+14-bit hash)。**j5/j6 を Twitter で全帯域支配** (j5 比 −1〜−9 ns)、inline B/cap は j5 −14・j6 +2 | `sieve-j7-m23-twitter` |

速度ベースラインは `sieve_orig` から `sieve_j5` に移行済み。
次の関心は **「j5 の inline footprint を削って orig と memory-fair に勝つ」**。

---

# M. j5 メモリフットプリント削減 (現在の関心)

## M0. 動機

memfair レポート (`2026-05-05-j5-vs-orig-2x-memfair.md`) で確定:

- j5 inline bytes/cap ≈ 34、orig ≈ 25
- 肥大要因 2 つ:
  1. `order_cap = 2 * capacity` (`sieve_j3.rs:72-78`) — tombstone 用 slack で物理 slot が論理 cap の 2 倍
  2. `Entry { key, value, visited: bool }` の bool padding (Rust の 8 B align で 7 B 死ぬ、K=V=u64 のとき)

memfair は「j5 の速度優位が memory hand-out 由来でない」を確定したが、
**同じ inline footprint 同士で勝つ** のが次のゴール。

## M1. `order_cap` 2x slack を削る

| 案 | 概要 | 期待節約 | 副作用 | リスク |
|---|---|---:|---|---|
| M1.1 | 倍率を 1.25x / 1.5x に | -25〜37% slack | compaction 頻度 ↑ (各回は memcpy のみ、rehash 無し) | 低 |
| M1.2 | tombstone なし、`do_evict` 時に左シフト (1.0x) | -100% slack | `O(tail-pos)` shift / evict | 中 (small-cap では scan に埋もれる想定) |
| M1.3 | hand 通過時の lazy 整地 | tombstone 比率を低位に維持 | hand pass のロジック追加 | 中 |
| M1.4 | shard 跨ぎで slack をプール共有 | 8 shards で slack 1 本 | 設計重い、ROI 不明 | 高 |

## M2. Entry padding を取る (visited bit の置き場)

`Entry` を 24 B → 16 B にできれば -7 B/slot × 2 = **-14 B/cap (~28%)**、j5 34 → 20 B/cap で orig (25) を抜く水準。

| 案 | 概要 | 期待 | 備考 |
|---|---|---|---|
| M2.1 (= `sieve_j6`、敗) | visited を `tags[i]` の MSB に同居 (tag = 1-bit live + 1-bit visited + 6-bit hash) | -28% memory、しかし throughput は **j5 比 +2.5〜+11.3 ns** で劣化 (`sieve-j6-m21-twitter`) | tag bit を削った (false-match 1/128→1/64) のが主因と判明 |
| M2.2 | visited を別 bit-vector (`Vec<u64>`) に SoA 化 | -28% memory | hit 時に別 cache line 1 本踏む。M2.1 の劣化版 |
| **M2.3 ★ (= `sieve_j7`、勝)** | tag を u16 化、live + visited + 14-bit hash。false-match 1/16384 | inline B/cap は j5 −14・j6 +2、throughput は **j5/j6 を Twitter 全帯域で支配** (`sieve-j7-m23-twitter`) | M2.1 の方針 (visited を tag に) は正しく、bit を削るのではなく増やすのが正解 |

## M3. SoA 化

`keys: Vec<K>`, `values: Vec<V>`, `tags: Vec<u8>`, (`visited: Vec<u64>` if M2.2)。

- scan 中は tags のみ触れて L1 滞留が改善
- memfair Regime 3 (per_shard=1250 で j5 完敗、線形 scan が L1d 48 KB を踏み外す) の救済策
- M2.1 採用なら visited は tags 側、values は key match 後にだけ参照

## M4. 「2x slack を ghost cache として活用」

memfair の指摘どおり、dead slot は機能的に ghost。捨てずに使う別軸。
**メモリを削るのではなく単位 memory 当たり hit ratio を上げる方向**。

- **M4.1**: tombstone に key だけ残す (value は drop)。次の miss で ghost ヒットなら priority insert (visited=true で投入)。NSDI 論文外、ARC / 2Q 寄り
- **M4.2**: ghost 領域を 0.25x に縮小 (M1 と組合せ)

## M5. 根本再設計

| 案 | 概要 | 評価 |
|---|---|---|
| M5.1 | open addressing + 別配列で SIEVE 順序 | 「現状の良い構造を保ったまま」から逸脱、保留 |
| M5.2 | metadata を 32 B に圧縮 | 実効ゼロ、却下 |
| **M5.3** | slack を tags 側だけに限定 (entries は cap+ε のみ確保、tail 同期) | M1 と直交。`order_cap` 拡張の純コストが **2 B/slot** (tags + visited bitmap) まで縮む |

## M6. ROI 推奨 (主観、2026-05-05 更新)

1. ~~M2.1 (j6)~~ → 棄却。tag bit 削減で throughput 劣化。
2. **M2.3 (j7、採用済)** — tag を u16 化、live + visited + 14-bit hash。Twitter 全帯域で j5/j6 を支配。新ベースライン。
3. **memory-fair sweep on j7** — j7 の inline B/cap (実効 18) で揃えた cap で orig (25) / j5 (25) と head-to-head。
4. **M1.1 or M1.2 (slack 削減)** — j7 と直交。small-cap 帯ほぼ無痛。倍率 vs ns/op vs memory の Pareto sweep。
5. **M5.3 (slack を tags 側のみに)** — M1 と組合せで slack 拡張コスト最小化。j7 では tags 4 B/cap になり相対コストが上がるため検討価値あり。
6. **M3 (SoA)** — memfair Regime 3 救済。j7 の tags は既に並列配列なので半分は実装済。
7. **M4.1 (ghost 活用)** — 論文外拡張、研究的興味。

短期ロードマップ:
- ~~M2.1 単体ベンチ~~ → 完了 (`sieve-j6-m21-twitter`、棄却)
- ~~M2.3 単体ベンチ~~ → 完了 (`sieve-j7-m23-twitter`、採用)
- **次**: j7 の memory-fair sweep + cluster019 (scan-heavy) 追加検証
- M1 系の Pareto sweep を j7 ベースで
- 必要に応じて M5.3 / M3 を重ねる

---

# Phase 1〜3 アーカイブ (orig/v3 時代のブレスト)

以下は 2026-05-04 起源の原文。**いくつかは実装済**で、現状から見ると古い見立ても多い。
研究的な書き残しとして温存する。

## 完了済み / 実装済みアイディア

| 旧 ID | 内容 | 実体 | 主レポート |
|---|---|---|---|
| A1 | hasher 統一 | `src/hash.rs` で全 variant XXH3 統一 (FxHash ではないが思想は同じ) | `sieve-j3-bench` 他 |
| C2 | `Option<Entry>` → `MaybeUninit` | `sieve_orig` で適用、bench はノイズ範囲だが構造的正しさで採用 | `sieve-orig-overhead-analysis` |
| E1 | orig + MaybeUninit (旧 v4 構想) | C2 と同上 | 同上 |
| J3 | 全 inline (Map 廃止 + tag SIMD scan) | `sieve_j3.rs` | `sieve-j3-bench` |
| J2 | set-associative | `sieve_j4.rs` (`DEFAULT_SHARDS = 8`) | `sieve-j4-*` (3 本) |
| (J 派生) | double-hash 排除 | `sieve_j5.rs` | `sieve-j5-doublehash-ab` |

## 未着手 / 保留

- **B1 (visited inline)**: v3 系列としては未実装。j3/j4/j5 では結果的に inline 済みだが、それが今度はメモリ問題になり **M2 に引き継ぎ**
- **E2 (Node packing)**: orig の Node を 32→24 B に。orig 系列追加磨きとして残置
- **A3 (raw_entry で hash 1 回)**: HashMap 経路を持つ orig/v3 系専用、j 系列では無関係
- **F1 (S3-FIFO) / F2 (W-TinyLFU) / F3 (2-queue SIEVE)**: 比較対象拡張、優先度低
- **D1 (compact トリガ緩和)**: j3 では `order_cap=2x` 固定のため **M1 に統合**
- **G (計測軸)**: skew=0.5 / 大 V / churn-heavy 等は未実装、優先度中
- **J4 (共通 inner segment trait)**: 未実装 (j4 は j3 を直接 wrap)。比較効率化は得られているので緊急性低
- **J6 (順序保存版 set-associative)**: 未着手、設計コスト大

## 旧本文 (アイディアブレスト原文)

以下は 2026-05-04 時点の文章をそのまま残す。出発点となった
profile 内訳 (HashMap 80% / hit path 1.5-4.7% / evict 7-9% / compact 0-3.4%) は
v3 系列に対するもので、j3/j4/j5 では Map 自体が消えているので解釈には注意。

## A. HashMap 層 (一番太い、最も orthogonal)

**A1. FxHashMap / ahash 化**
- siphash leaf (`sip.rs:78,79,256` 等) は合計 7-10%。`rustc-hash` で半減期待。
- 全 variant に同じ hasher を入れて比較するのが筋。
  `SieveCache<K, V, S = RandomState>` に generic 化。
- **期待: 15-25% (全 variant 一律)。実装半日、リスク小。**

**A2. u64 専用 identity hash**
- bench は u64 key。`BuildHasherDefault<IdentityU64Hasher>` で衝突は Zipf
  に賭ける。
- 学術的興味: 「SIEVE 純粋なオーバーヘッドはいくらか」を測る基準点になる。
- 期待: A1 に対して更に +5-10%、ただし production 不可。

**A3. entry()/raw_entry で hash 1 回化**
- miss path: `index.get` → None → `index.insert` で同じ key を 2 回 hashing。
  `hashbrown::HashMap::raw_entry_mut().from_key()` なら 1 回。
- evict 時の `index.remove` は別 key (victim) なので畳めない。
- 期待: 1-3% (miss が 22% × hash 半減)。実装 1 日。

**A4. HashMap の中身を SIEVE 内部 array に直接置く**
- HashMap の value が `EntryId` なのは 1 段の indirection。値も中に入れて
  1 段減らす。
- でも `HashMap<K, Entry<K,V>>` にすると iteration 順序がランダムで
  SIEVE order が崩れる。組み合わせ難。基本ボツだが頭の体操として。

## B. hit path (req の 78% にかかる、v3 の最大の弱点)

**B1. visited bit を Entry に inline 化** ★
- `struct Entry { key, value, qpos, visited: bool }`。bitmap BitSet を撤去。
- `entry.value = value; entry.visited = true;` が**同じ cache line 内**で
  完結 → orig と同じ hit-path コスト。
- 代償: bit-parallel scan は失う (visited が散らばるので word load できない)。
- でも profile が「scan は 4.6% しかない」と言ってるので失っても痛くない。
- **期待: hit-path 余分 0.8ms / total 3.15ms ギャップの 25% を回収。
  実装 1 日。**

**B2. visited を `Vec<u8>` の並列配列に**
- entries とは別配列だが byte 単位 → SIMD `pcmpeqb` でゼロ走査ができる。
- hit 時はバイト 1 個書き込み。ただし**別 cache line を踏むので B1 ほど
  安くならない**。
- B1 の劣化版。boundary case 用に頭に入れておく程度。

**B3. visited が既に立っていたら write を skip**
- `if !entry.visited { entry.visited = true; }`。Zipf top-keys は 2 回目
  以降の hit で write を省く。
- branch コスト > 節約 store コストなのでマイナーだが、cache line dirty を
  防ぐ意味で coherence 重要な並列下では効くかも。

**B4. SIEVE-LRU 雑種: 一定確率でしか visited を立てない**
- `if (key.hash() & 0xff) == 0 { entry.visited = true }` 等で 1/256
  確率にする。
- hit rate は若干劣化、hit-path コストはほぼ消える。
- 論文準拠を捨てるので別 algorithm 系列扱い。

## C. miss path 構造の刈り込み

**C1. `find_victim` と `do_evict` をマージ**
- 関数境界 + r1/r2 の `ScanResult` 構造体経由で Option 検査が増えている。
  インライン後の codegen は良いはずだが、`first_live` 追跡の分岐が
  steady-state を太らせている (v3 bench レポートで言及)。
- single function で early-return / `unreachable_unchecked()` を使って
  分岐を圧縮。
- 期待: 0.5-1%。コード可読性とトレードオフ。

**C2. `Option<Entry>` を `MaybeUninit<Entry>` に剥がす** ★
- `entries: Vec<Option<Entry<K,V>>>` の Option を取る。tombstone bitmap
  が「dead/live」の情報を既に持っている。
- profile leaf hot 1 位が `option.rs:767` (Option::unwrap) で 9.5% — ここ
  を直撃する。
- 必要: unsafe block + Drop の手動管理 (key/value の drop は eviction 時
  に take で実行)。
- **期待: 2-4%。実装 1 日、unsafe があるので test 厚め。**

**C3. プリフェッチ**
- scan で `pos` が決まった瞬間に `_mm_prefetch(&entries[order[pos]])` を
  投げる。
- LLVM が自動でやってくれている可能性もあるが、明示で 1-2%。

**C4. order を `Vec<u32>` で u32 化**
- `EntryId = usize` (= 8 bytes/slot) → `u32` で 2 倍密度。cache line 1 本
  で 16 entry 分の order が見える。
- 1.6M slots まで対応 (capacity=800k 規模) — 現状の bench 範囲では足りる。
- 期待: 1-3%。

## D. compaction (orig には無い純粋オーバーヘッド)

**D1. トリガを緩める**
- 現状 `tail == order_cap || dead >= len` → `tail == order_cap || dead >= 4*len`
  等に。
- メモリ使用量は増えるが時間は減る (compact 頻度 1/4)。
- `order_cap = 4 * capacity` にすれば cap=10000 では 40k slot に。
- **期待: cap=100 で 3-5% (compact 6.8% を半減程度)。実装 1 時間。**

**D2. 増分 compaction**
- insert 毎に 8 slot ずつ「動かす」。1 回の insert が compact 全体を
  背負わない。
- レイテンシは平準化、合計仕事は減らない。**bench は wall time なので
  この bench では効かない**が、レイテンシ分布的には嬉しい。
- 別レポートで p99 を出すなら有意。

**D3. ring buffer 化 (compaction-free)**
- order を環状にして tombstone を hand 通過時に即時回収。
- ただし SIEVE は「insertion 順を保つ FIFO」なので、tombstone 跡地に
  新規 entry を入れると順序が壊れる。
- 解決: 「insertion 順 = 実時間 tail 増加」を別カウンタで管理し、ring の
  物理位置と論理 qpos を分離。
- 設計コスト大。**結局 orig の連結リスト構造に近づく。**

**D4. そもそも array-based を諦める**
- profile が「array-based の overhead は構造的」と言っている。E に進む。

## E. orig を磨く (= 勝者の最適化、profile 的に最高 ROI)

**E1. v4: `Vec<Option<Node>>` → `Vec<MaybeUninit<Node>>` + free_list** ★★★
- orig leaf hot 1 位 `option.rs:767` 13.94% を直撃。
- `node_mut` / `node` の `as_mut().expect("live node")` を全部
  `unwrap_unchecked` 等価に。
- **期待: 5-10%。実装 1 日、unsafe + miri test。**

**E2. Node packing**
- 現状: `key: u64, value: u64, freq: u32, prev: NodeId(u32), next: NodeId(u32)`
  = 28 bytes、padded 32。
- `freq: u8` + 3 bytes pad、または freq を NodeId の MSB に押し込む
  (capacity < 2^31)。
- Node を 24 bytes に。連結リスト walk が 1 cache line で 2.6 entry →
  5.3 entry 入る。
- 期待: hand walk が遅い workload (低 hit rate) では 5-10%、高 hit rate
  では 1-3%。

**E3. hot/cold split**
- `Vec<NodeMeta>` (key, prev, next, freq) と `Vec<V>` (value) に分割。
- evict_one walk は NodeMeta 配列だけ触れば良い。value は cache に load
  しない。
- bench では V=u64 で半分の効果しかないが、V が大きい (e.g. struct)
  ワークロードで決定的に効く。
- 期待: bench 1-3%、現実的 V で 10-30%。

**E4. arena chunk allocator**
- `Vec` の amortized growth を捨て、固定 chunk size でアロケート。
  allocation latency を平準化。
- bench では効かない (warm-up でアロケ済み)。

**E5. orig + A1 (FxHashMap) + E1 + E2 を全部入れた "orig_pro"**
- 期待累積: 25-40% over orig baseline。

## F. アルゴリズムレベル再設計

**F1. S3-FIFO 移植**
- SIEVE の兄弟分。Small-FIFO + Main-FIFO + Ghost-FIFO の 3 段。hand walk
  なし、全 O(1)。
- hit rate は SIEVE と同等以上 (論文では多くの workload で勝つ)。
- 比較対象として価値高い。実装 2-3 日。

**F2. W-TinyLFU**
- frequency sketch + window LRU。hit rate top tier。
- 実装 5-7 日 (count-min sketch + admission policy)。
- "SIEVE family" を超える比較。

**F3. 2-queue SIEVE (筆者ヒューリスティック)**
- "fresh" FIFO (visited なし) + "examined" FIFO (visited 1 度だけクリア済み)
  の 2 段。
- 新規 → fresh tail。eviction → examined tail から取る、空なら fresh から
  examined に migrate しながら drain。
- hand walk 完全消去 (常に O(1) operation のみ)。
- ただし semantic は変わる。SIEVE 論文と一致するか要検証。

**F4. SIEVE-O(1) via "visited 別 queue"**
- visited を立てた瞬間に entry を別 queue に移動する eager 設計。
- eviction 時には visited queue を見ず、unvisited queue の tail を取るだけ。
- hand 不要。ただし hit path に「entry を queue 間移動」コストが乗る。
  trade-off。

## G. 計測軸の拡張 (アイディアではないが、上の効きを正しく測るため必要)

- **skew=0.5 を試す**: 分布が flatter で N が大きくなる。array-based の
  bit-parallel 真価が出る条件。今回 0.6 が最低だが、scan ブロックが
  そもそも全体の 5% しかなく見えない。
- **大きい V (例: `[u8; 256]`)**: hot/cold split (E3) の効きを測る。
- **大量 churn workload** (req >> capacity × 100): orig の linked-list
  pointer chasing 悪化を狙う。array が逆転するかも。
- **`get`-only / `insert`-only / mixed の 3 種**: 現状 micro bench は
  insert_only。SIEVE の `get` は `&mut self` 必要なので比率変わると
  hit-path コストの相対重要度が変わる。

## H. 期待 ROI ランキング (profile 根拠付き)

| 順位 | アイディア | 期待速度 | 工数 | リスク | profile 根拠 |
|---:|---|---:|---:|:---:|---|
| 1 | E1: orig + MaybeUninit (v4) | +5-10% over orig | 1d | 中 (unsafe) | option.rs:767 leaf 13.94% |
| 2 | A1: FxHashMap (全 variant) | +15-25% 一律 | 0.5d | 低 | sip.rs leaves 合計 ~10% |
| 3 | B1+C2 + tombstone 簡素化 (v5) | +5-10% over v3 | 1.5d | 中 | hit-path 0.8ms + Option 0.3ms |
| 4 | E2: Node packing | +2-5% over orig | 1d | 低 | hand walk cache miss |
| 5 | A3: entry() | +1-3% | 1d | 低 | miss path × 2 hash |
| 6 | D1: compact トリガ緩和 | cap=100 で +3-5% | 1h | 低 | compact 6.8% (cap=100) |
| 7 | F1: S3-FIFO 比較 | (比較軸) | 3d | 中 | — |
| 8 | F3: 2-queue SIEVE | +5-15%? | 3d | 高 | hand walk 消去 |

## I. 提案: 短期 → 中期 のロードマップ案

**Phase 1 — "winner を磨く" (1-2 週)**
- v4 = orig + MaybeUninit + Node packing
- 全 variant に generic hasher 軸を追加 (`<K, V, S>`)、FxHashMap 比較を
  bench に組み込む
- 期待: orig_pro = orig × 0.65-0.75 (= 25-35% 速い)

**Phase 2 — "array 路線の評価をやり直す" (1-2 週)**
- v5 = v3 + visited inline + Option<Entry> 剥がし + compact トリガ緩和
- Phase 1 の winner と head-to-head。array が orig_pro に追いつくかで
  この路線の継続判断
- workload を skew=0.5 / 大 V / churn-heavy に拡張して array が勝つ条件
  を探す

**Phase 3 — "兄弟アルゴリズムとの比較" (2-3 週)**
- S3-FIFO 移植 (F1)
- 2-queue SIEVE (F3) を実験的に
- "SIEVE 論文準拠" を緩めた variant で hit rate / latency / wall time
  全部測る

**Phase 4 — "memory layout の本気"**
- hot/cold split (E3)
- chunk allocator
- production 想定 (concurrent / Send / Drop) を入れた時の劣化測定

## 直感のまとめ

- **E1 (orig + MaybeUninit) が一番手堅い** — profile leaf hot 1 位を直撃。
- **A1 (FxHashMap) が一番大きい** — 全 variant 一律 +20%、SIEVE と直交。
- **B1+C2 (v5) が一番面白い** — 「v3 の hit-path 弱点を消したら orig に
  追いつくか」という研究的な問い。

まずは **E1 か A1 を 1 つ入れてベースラインを更新する** のが筋が良さそう。
そうすると以降の variant 比較が新しい baseline 上で測り直せる。

どれもプロファイル取り直し前提で、`scripts/samply_phases.py` のマーカー
セットを各 variant 用に追加すれば同じ枠組みで evaluate できる。

## J. 「Map と array の結合」を切る設計群 (本質的な再設計)

A〜I は暗黙に「`Map<K, EntryId>` + 補助構造 (linked list / array)」という
**二層構造を維持したまま各層を磨く**前提に立っている。しかし profile が
告げているのは、その「Map 層」自体が 80% のコストを占めるという事実
である。であれば「層を磨く」ではなく **層自体の結合を再設計する** 道が
ありうる。これは A1 (FxHashMap) や A4 (Map に Entry 直接格納) より一段
深く、Map と array の **結合の仕方そのもの** を問う。

中心 insight:

> Map の value が「array 内の正確な index」である必要は、本来は無い。
> ハッシュテーブルの lookup は **「hash → 候補絞り込み → 等価確認」**
> の 3 段で動いており、最終確認が等価比較なら、中間の「位置」は coarse
> でも (極端には bogus でも) 当たり判定は壊れない。「位置を正確に保つ」
> ためにかけている compaction や Map 書き換えは、そもそも回避可能な仕事
> ではないか。

### J0. 設計空間の整理 — 「Map[K] が指すものの粗さ」軸

| 設計 | Map[K] の value | lookup の最終解決 | 移動 (compact/merge) コスト |
|---|---|---|---|
| **既存 (v0/v3)** | `usize` (array 内の正確な index) | 即 dereference | 全 alive entry の Map 書き換え |
| **J1: 時系列 segment** | `seg_id` (segment 単位、stable) | seg 内 SIMD tag scan + 等価 | seg 内 compact: ゼロ / seg 間 merge: 動かした entry の Map.value 上書きのみ |
| **J2: hash 分割 segment** | (Map 自体不要) `hash(K) >> shift = seg_id` | seg 内 SIMD tag scan + 等価 | 構造的にゼロ |
| **J3: 全 inline** | (Map 自体不要) tag を entry に張り付け | 全配列 SIMD broadcast scan + 等価 | 物理 move のみ、rehash 皆無 |

3 案は実は **同じ "inner segment" コードの outer index 切り替え** で実装
できる (→ J4)。

### J1. 時系列 segment 型 (insertion-order partitioned, no-merge)

**構造**:
- `Vec<Segment>`、segments を時系列に並べる (古い順)
- `Segment = { entries: [(K,V); K_size], tags: [u8; K_size], visited: u64, alive_mask: u64 }` (K_size=64 想定、AVX2 で 2 op / segment)
- `Map<K, SegId>` のみ保持。SegId は **stable** — segment が完全に空に
  なったら free list に戻すが、再利用された ID は意味的に新規

**SIEVE 意味論**: 完全準拠
- 挿入順 = segment 順、segment 内も挿入順
- hand walk: SegId 昇順 × segment 内 slot 順
- 同じ trace で `sieve_orig` と evict 列が一致するはず (oracle test 可能)

**operations**:
- `insert`: tail segment が満杯なら新 segment を確保、tag = `(hash(k) as u8)`、空き slot に書く
- `get(k)`: `Map[k] → seg_id → segment.tags` を SIMD broadcast 比較 → 候補で full key 等価 → visited 立てる
- `evict`: hand を seg → slot 順に進めながら visited を倒す、unvisited を返す
- **intra-segment compact**: 不要 (slot dead でも tag scan は 1 op、Map は不変)
- **inter-segment merge**: 採用しない (= 「sparse 受容」)

**Map 更新コスト**:
- insert / evict 時: 通常の 1 hash + 1 write のみ
- compaction 起因の Map 書き換え: **ゼロ**
- v0/v3 が抱えた「compaction が走る瞬間に O(alive) の Map 書き換え」が
  構造的に消える

**メモリ overhead 上限**:
- 最悪 50% dead で capacity の 2x
- ただし Zipf workload では cold tail が segment 単位で死ぬので、実測
  fragmentation は 10-30% に収まる可能性が高い (要計測)
- segment 数 = capacity / K_size、cap=10000 / K_size=64 → 156 segments

**期待効果** (profile 根拠付き):
- HashMap layer の cache footprint 削減: value が `usize` (8B) →
  `u32 SegId` (4B) で half。slots/cache-line 倍増
- compaction 起因の Map 書き換え消滅 (v3 の compact 0-3.4% を直撃)
- segment 内 SIMD tag scan で hit-path も orig 並み (B1 の hit-path 改善
  と同等の効果)

**実装難度**: 中。AVX2 intrinsic と tag 計算が要るが unsafe は最小限。

### J2. hash 分割 segment 型 (set-associative SIEVE)

**構造**:
- `Vec<Segment>` を **hash で indexing**: `seg_id = hash(k) >> (64 - log2(N_seg))`
- N_seg と K_way は固定 (N_seg × K_way = capacity)
- **外部 Map 完全に廃止**

**SIEVE 意味論**: 弱化 (= 別 algorithm 系列扱い)
- 各 segment が独立な mini-SIEVE
- グローバル挿入順は失う、per-segment 挿入順のみ
- hit rate 影響は associativity (K_way) と hash 品質に依存
- → CPU L1/L2 (8〜16-way set-associative + LRU/PLRU) と同じ理論枠組み

**operations**:
- `insert(k,v)`: `hash → seg_id` → segment 内 SIEVE で eviction + 挿入
- `get(k)`: `hash → seg_id → tags SIMD scan` → 等価 → visited
- evict は per-segment hand
- compaction 概念が存在しない

**期待効果**:
- HashMap 80% を **完全消滅** (profile の OTHER を直撃する一番大きい賭け)
- per-segment lock で trivially concurrent (Phase 4 への布石)
- メモリは set-associative なので fixed (J1 のような fragmentation 無し)

**リスク**:
- Zipf hot keys が同じ segment に集中する確率が hash 分布に依存
- cap=10000、K_way=16 → N_seg=625、Zipf top 100 keys が同じ 1 segment
  に落ちる確率は実測しないと分からない (FxHash や ahash の rand 性に依存)
- hit rate degradation の見積もり: CPU cache 文献では K=16 で
  fully-associative の 95-98% に到達する例が多い

**実装難度**: 低 (3 案の中で最小)。

**前例調査**:
- CPU L1/L2/L3 cache: 8〜16-way set-associative + LRU/PLRU/Random
- Twitter Segcache (NSDI'21): segment-structured KV cache、ただし TTL 駆動
- 「**SIEVE を set-associative 化**」した実装は文献上見当たらず、
  研究的に未踏の可能性が高い (要文献調査)

### J3. 全 inline 型 (SwissTable as cache)

**構造**:
- `Vec<(tag: u8, K, V)>` ただ 1 本 (tag 配列と KV 配列を別 Vec にして
  SoA にする方が SIMD は楽 — 後述)
- tag = `(hash(k) >> shift) as u8`、entry に inline
- **Map 廃止 + segment 分割も廃止**
- visited bit は別 bitmap または entry inline

**SIEVE 意味論**: 完全準拠
- 配列順 = 挿入順、hand は配列を 1 周
- intra-array compaction (tombstone 詰め) は採用 / 非採用どちらでも構わない
  — tag は entry に張り付くので **rehash 不要**

**operations**:
- `get(k)`: `tag = hash(k) >> shift`、broadcast(tag) を AVX2/AVX-512 で
  全 tag 配列に SIMD 比較 → matching positions → 各候補で key 等価
  → visited
- `insert/evict`: 配列 tail で循環、SIEVE 通常通り
- 「移動」は全て物理 memcpy のみ、Map 整合性は **そもそも存在しない**

**性能スケーリング** (AVX2 vpcmpeqb + vpmovmskb ≈ 3 cycles / 32-byte chunk):

| capacity | scan cycles | scan ns @3GHz | 候補数 (1/256) | 等価コスト | 合計 lookup |
|---:|---:|---:|---:|---:|---:|
| 100 | ~10 | 3 | ~0.4 | ~2 ns | **~5 ns** (HashMap 圧勝) |
| 1000 | ~96 | 32 | ~4 | ~20 ns | **~50 ns** (互角) |
| 10000 | ~940 | 313 | ~40 | ~200 ns | **~500 ns** (HashMap 100 ns に負け) |
| 100000 | ~9400 | 3 us | ~390 | ~2 us | **~5 us** (HashMap 圧勝) |

つまり **「中規模 (~1000 entries) までは Map 不要が成立する」** という
明確な crossover がある。SIEVE ベンチの cap=100 と cap=10000 の中間が
ちょうど境界。

**実装難度**: 中。AVX2 intrinsic を直接、scalar fallback も書く必要
(ARM64 NEON / WASM SIMD)。

**研究的価値**:
- 「HashMap が支配的というのは本当に必然か?」の反証実験
- 小〜中規模 cache における「Map ゼロ設計」のベースライン
- L1/L2 内に収まる cache 規模では SIMD scan が pointer-chasing に勝つ

### J4. ハイブリッド設計: 「inner segment」を共通基盤にする

J1, J2, J3 は実装上 **ほぼ同じ機構** を使う。違いは「inner segment が
何個あって、どう選ぶか」だけ:

| 案 | inner segment 数 | outer index | 順序保存 |
|---|---|---|---|
| J3 | **1** | (不要) | グローバル挿入順 ✓ |
| J2 | 多数 | hash で直接決定 | per-seg のみ |
| J1 | 多数 | `Map<K, SegId>` | グローバル挿入順 ✓ |

つまり **inner segment** (tag SIMD scan + key 等価 + per-seg SIEVE) を
1 つきれいに書けば、outer indexing を差し替えるだけで 3 案を低コストで
比較できる。

```rust
trait Index<K> {
    fn locate(&self, key: &K) -> SegId;        // J2: hash 直接 / J1: Map lookup
    fn record(&mut self, key: K, seg: SegId);  // J1 のみ実装、他は no-op
    fn forget(&mut self, key: &K);
}

struct Cache<K, V, I: Index<K>> {
    segments: Vec<Segment<K, V>>,  // 共通
    index: I,                       // 切り替え点
}
```

これは **研究実験の効率を劇的に上げる**: 3 案を 1 つの bench harness で
比較でき、profile も同じ計測軸で解釈できる。

### J5. 「Map 更新は rehash ではない」の精緻化

J1 で merge を採用した場合 (or 採用しなくても、整理として):

| 操作 | per-entry コスト | 理由 |
|---|---|---|
| v0/v3 compact | hash(k) + Map probe + write | k から index を引き直す必要 |
| **J1 merge** | (hash 不要) + Map probe + write | k は entry に inline、tag も pre-computed |
| J1 intra-seg compact | **ゼロ** | Map 不変 |

含意:
- merge 1 回コスト = (動かした entry 数) × (Map probe + write)
- hash 再計算は不要
- amortize すると merge 頻度が低い限り無視可能
- → **J1 で merge を「禁止」する必要はない**。「sparse になりすぎたら
  merge」を許す柔軟設計でも、コストは v0/v3 とは桁違いに小さい

これは前回の議論で私が「merge は破綻」と過大評価したのを訂正した結論。
正確には「merge は **可能** だが、no-merge の方がより構造単純で
メモリ overhead と引き換えに Map 完全不変が得られる」。

### J6. 順序保存版 set-associative (野心的、要検討)

J1 (順序 ✓ Map 必要) と J2 (順序 ✗ Map 不要) は両極。両取りはできないか?

**スケッチ**: lane 内は時系列、lane 間は hash で分割
- `Segment[N_lane][K_slot]` 行列状
- `insert(k,v)`: `lane = hash(k)` → 該当 lane の最新 slot に追加
- `get(k)`: `lane = hash(k)` → lane 内 SIMD scan
- `evict`: 全 lane の最古 timestamp を比較 → **時刻が一番古い lane** の
  最古 slot から evict (= グローバル時刻順を維持)

**問題点**:
- evict 時に全 lane の最古を見るので O(N_lane) per eviction
- lane 内 hand 位置の管理が複雑、SIEVE の hand 概念が破綻気味
- min-heap で最古 lane を持てば O(log N_lane) だが、insert/evict ごとに
  heap 更新コストが乗る

**判定**: 設計コスト大、SIEVE 意味論との整合に時間がかかる。
**J1, J2 を先に評価してから戻ってくる順序が ROI が高い**。

### J7. ROI 表への追記と Phase ロードマップ反映

H 表に追記 (相対順位は Phase 1 完了後の orig_pro baseline 想定):

| 順位 | アイディア | 期待速度 | 工数 | リスク | profile 根拠 |
|---:|---|---:|---:|:---:|---|
| **新★** | **J2: set-associative** | +40-80% over orig | 4d | 中 (hit rate ↓ を要実測) | OTHER 80% **完全消滅** |
| **新★** | **J3: 全 inline (中規模)** | cap≤1000 で +30-60% | 3d | 中 (大規模で逆転) | OTHER 80% 直撃、crossover 明確 |
| **新** | J1: 時系列 segment | +20-40% over orig | 5d | 中 | OTHER 50% 削減 + compact 消滅 |
| **新** | J4: 共通 inner segment 基盤 | (上の前提条件) | +1d | 低 | 3 案を低コスト比較可能 |

I 章に **Phase 2.5** を挟む (Phase 1 = orig 磨き、Phase 2 = array 路線
再評価 の間):

> **Phase 2.5 — 「Map を捨てる」実験 (2-3 週)**
>
> 1. **inner segment 共通基盤** (J4) を実装: tag SIMD scan + per-seg SIEVE
> 2. outer index を 3 通り差し替えて評価:
>    - (a) `IndexNone` (J3): segment 1 つ、capacity ≤ 1024 想定
>    - (b) `IndexHash` (J2): hash 直接、SIEVE 意味論弱化
>    - (c) `IndexMap` (J1): `Map<K, SegId>`、no-merge / 順序保存
> 3. cap=100 / 1000 / 10000 で head-to-head、orig_pro と比較
> 4. **hit rate を必ず orig 比較計測** (J2 の弱化が許容範囲か判定)
> 5. capacity を増やしながら J3 → J2 への crossover 点を地図化
> 6. 期待: 中規模で HashMap 層の構造的削減を実証、研究的には
>    「SIEVE を Map 非依存に再設計した初の実装」として記録に値する

### J8. 直感のまとめ (J 群)

- **J2 (set-associative) が一番大きい賭け** — HashMap 80% を完全消滅
  させる。hit rate 影響は実測勝負だが、CPU cache の蓄積から見て
  associativity 16 で fully-associative 比 95%+ は期待できる
- **J3 (全 inline) が一番面白い** — 「Map は本当に必要か?」を反証する
  研究的価値、中規模で勝てる地形が profile から計算可能
- **J1 (時系列 segment) が一番手堅い** — SIEVE 論文準拠を維持しつつ
  HashMap 層を縮減、no-merge という構造的単純さが効く
- **J4 を共通基盤にする** — 1 つの inner segment 実装で 3 案比較、
  研究実験の総コストが半分以下になる
- **J5/J6 は理論的整理** — 直接実装 priority は低いが、議論の精度を
  上げる土台

H 章の E1/A1 を 1〜2 週で消化したあと、J 章を Phase 2.5 として走らせる
のが、profile 根拠 + 研究的興味 + 実装効率 の 3 軸で最大 ROI と思う。

### 補足: なぜこれが「本質的に深い」か

A〜I の改善案はすべて **「Map<K, EntryId> + 補助構造」という二層構造を
所与とする** 範疇に閉じている。FxHashMap も MaybeUninit も Node packing
も、その枠の中で各層を磨く改善である。

J 群はその枠自体を疑う:

1. **「Map[K] は正確な位置を返す必要があるのか?」** — 否。SwissTable 的に
   候補 filter + 等価確認なら、位置は coarse でよい (J1)
2. **「そもそも Map は必要か?」** — 否。hash で直接 segment を決めれば
   外部 Map 不要 (J2)
3. **「entry 自体に hash を inline すれば移動コストはゼロにできるか?」**
   — 是。tag が entry に張り付けば rehash 概念自体が消える (J3)

これは **「キャッシュとは Map<K, V> なのか、それとも tag-indexed array
なのか」** という data structure の根本選択の問題で、A〜I の射程の外に
ある。SIEVE algorithm そのものを変えずに、その実装基盤を入れ替える。

profile が 80% を HashMap が占めると告げているとき、Map 層を「使う前提
で磨く」のではなく **「使うかどうかから問い直す」** のが、研究的にも
工学的にも筋が良い。
