# docs/reports インデックス

実験・調査ごとのレポート一覧。新しいレポートを追加したらこのファイルも更新する。

## テーマ別マップ

| テーマ | 関連レポート |
|---|---|
| ベンチ条件の整備 | realistic-workload-bench |
| v0〜v3 系列 (linked-list + array) | v0-divergence, realistic-workload-bench, v3-bench, v3-profile |
| orig 改善 (MaybeUninit) | sieve-orig-overhead-analysis |
| 外部実装調査 | jedi-vs-orig |
| 設計アイディア集 (living doc) | `../improvement-ideas.md` (旧 `2026-05-04-improvement-ideas.md`、`docs/` 直下に移動) |
| j3 系列 (Map なし SIMD) | sieve-j3-bench |
| j4 系列 (set-associative j3) | sieve-j4-set-associative, sieve-j4-crossover-and-shard-sweep, sieve-j4-pershard-vs-footprint |
| j5 系列 (j4 の double-hash 排除) | sieve-j5-doublehash-ab, j5-pershard-pareto, j5-twitter-pareto, j5-vs-orig-2x-memfair |

---

## 一覧 (日付昇順)

### 2026-05-03-sieve-v0-divergence.md
`sieve_v0` が `sieve_orig` と eviction 列で分岐する条件を発見し原因を解明したレポート。
`evict_one` で victim が tail-1 だったとき hand が wrap せずに新規挿入 entry を指してしまうバグ。
compaction が偶発的に hand をリセットして隠蔽していたため unit test をすり抜けた。
対症修正 (`hand >= tail` でラップ) で oracle 全件 green 確認。

### 2026-05-03-realistic-workload-bench.md
ベンチ条件を NSDI'24 論文準拠に変更 (skew ∈ {0.6, 0.8, 1.0, 1.2}、cap ∈ {100, 1000, 10000}、
trace 1M req) して orig/v0/v1/v2 を再評価。orig が全条件で最速 (差 8〜27%)。
v1 (bit-parallel scan) はむしろ遅い (11〜27% 劣化)、v2 (Option 剥がし) は微改善 (0〜5%)。
skew=0.6 で eviction が飽和し orig リードが最も顕著。v1 の third-pass バグも同時に発見・修正。

### 2026-05-03-sieve-v3-bench.md
v1 (bit-parallel) + v2 (Option 剥がし) を合流し 2-pass 化した v3 をベンチ。
**v3 は v1 とほぼ tie、orig 比 1.11〜1.22× 負け**。2-pass 化は Zipf steady state では
fast-path で 1-pass が支配的なため効かない。bit-parallel も hand から数 slot で victim
が見つかる条件では word ロード 2 本のオーバーヘッドが勝つ。

### 2026-05-03-sieve-v3-profile.md
samply + addr2line で v3 の時間内訳を phase 別に分解したプロファイルレポート。
v3 が orig に負ける原因の定量内訳:
(1) hit path の `visited.set` RMW が余分な cache line を毎回踏む (+0.8 ms)、
(2) eviction bookkeeping が orig より多い (+0.7 ms)、
(3) compact が orig に存在しないコスト (+0.8 ms)。
scan ブロック自体は全体の 7〜8% しかなく、改善余地が元々小さいことを実証。

### improvement-ideas.md (living doc, 旧 `2026-05-04-improvement-ideas.md`)
日付つきレポートではなく `docs/improvement-ideas.md` に移動・改名された改善案の倉庫。
旧 A〜J 章のうち J3 / J2 / J5 派生は実装済 (sieve_j3 / j4 / j5)、A1 (hasher 統一) /
C2-E1 (MaybeUninit) も適用済。現在の関心は新規 **M 章「j5 メモリフットプリント削減」** —
`order_cap = 2*cap` の slack と Entry の visited padding を削るルートを並べる。

### 2026-05-04-jedi-vs-orig.md
既存 Rust 実装 `jedisct1/rust-sieve-cache` の設計調査。
`swap_remove` による立ち退きで Vec 内の相対順序が破壊され、連続 miss 時に新規 entry が
即 evict される CLOCK 寄りの挙動に縮退することを解析。
oracle (`sieve_orig` と eviction 列一致) の基準を満たさないと判定。
Rust 実装テクニックとしての参照価値も低く、詳細ベンチは後回しに決定。

### 2026-05-04-sieve-orig-overhead-analysis.md
C リファレンスと Rust ポートの差を機械語レベルで分析。
`Vec<Option<Node>>` の discriminant が hit path と eviction loop で毎ステップ 2 命令余分に走る
ことを objdump で確認。`Vec<MaybeUninit<Node>>` に置き換えを実装・実測。
asm は期待通り改善するが **bench はノイズ範囲内** — HashMap が 80% を占める orig では
ノード操作の局所最適は埋もれる。構造的正しさ理由で採用。

### 2026-05-04-sieve-j3-bench.md
外部 HashMap を廃止し tag 配列 AVX2 SIMD scan で lookup する `sieve_j3` の初回ベンチ。
初期実装の 2 バグ (scalar 末尾が SIMD を支配、tag hash に SipHash 過剰) を objdump で発見・修正。
XXH3 で orig と公平比較後: **cap=100 / skew∈[0.6, 0.8] で orig の 0.70〜0.73×**、
skew=1.2 では僅差で負け。cap≥1000 では O(N) scan で構造的に大敗 (1.7〜19×)。
MaybeUninit refactor でさらに数% 改善、skew=1.2/cap=100 も orig 比 0.98× に。

### 2026-05-05-sieve-j4-set-associative.md
j3 を 8 shards 並べた set-associative 変種 `sieve_j4` の初回ベンチ。
hit ratio: per-shard ≥ 64 (cap ≥ 512) で tax 消滅、per-shard=125 (cap=1000) では
orig より **+0.09〜+0.60 pp 上回る**。throughput: cap=1000 で j3 単独より 1.3〜2× 速く、
cap=100/skew≤1.0 で orig 比 0.62〜0.77× の勝ち帯。NSDI'24 論文外の独自拡張。

### 2026-05-05-sieve-j4-crossover-and-shard-sweep.md
j4 の cap 軸 sweep (N=8 固定) と SHARDS sweep (cap=1024 固定) で crossover と最適 shard 数を地図化。
hit ratio crossover は per-shard ≈ 64 (cap=512)。throughput crossover は skew 依存 (低 skew ほど
j4 が長く勝てる)。**skew=0.6 / N=32 で j4 は orig より 13% 速く hit ratio も +0.50 pp**。
throughput と hit ratio は逆方向のトレードオフ (throughput 最大 → N 大、hit ratio 最大 → N≈8)。

### 2026-05-05-sieve-j4-pershard-vs-footprint.md
「per_shard と total footprint のどちらが支配変数か」を 3 sweep で切り分け。
total footprint を 15× にしても per_shard が同じなら ns/op はほぼ不変 → **H2 (L1d 境界仮説) を棄却**。
`ns/op ≈ const_overhead(~30-35ns) + scan(per_shard, hit_ratio)` モデルを確立。
orig との 3〜5 ns 差は double hash 固定費に集中。実用帯は per_shard ∈ [32, 128]。

### 2026-05-05-sieve-j5-doublehash-ab.md
j4 の double-hash (shard 選択 + j3 内 tag 計算で hash を 2 回計算) を排除した `sieve_j5` の AB。
`sieve_j3` に `*_with_hash` API を追加し、外で計算した hash をそのまま渡す設計。
**Δ(j5−j4) = −7 ± 1 ns/op が cell・cap・total footprint に依らず安定** — double-hash の定常コストを
直接定量。per_shard ≤ 32 では j5 が orig を逆転 (例: 29 ns vs 33 ns)。
j4 の「34 ns 床から動けない」現象の正体が SIMD scan ではなく double-hash であることを証明。
以降の比較基準は j4 → j5 に移行。

### 2026-05-05-j5-pershard-pareto.md
per_shard ∈ {32, 64, 128, 256} × total cap ∈ {1024, 4096, 16384} × skew ∈ {0.9, 1.0, 1.2} を
直積で sweep し ns/op と hit ratio の 2 軸で Pareto を取る。
**「shard 細分化で SIEVE faithfulness が崩れる」直観は反証** — Δhit_ratio は全 36 セルで
±0.43pp 以内、半数のセルで j5 のほうが高い。throughput では per_shard=32 が常に最速 (orig 比
−1〜−7 ns)、per_shard ≥ 128 は scan tax で +5 ns 以上負ける。**sweet spot は per_shard ∈ [32, 64]**
で hit ratio も throughput も Pareto 支配。

### 2026-05-05-j5-twitter-pareto.md
Twitter cache trace (OSDI'20) cluster006/018/019 × cap ∈ {1024, 4096, 16384, 65536} ×
per_shard ∈ {32, 64, 128, 256} の総決算 sweep。synthetic Zipf 版の sweet spot
(per_shard ∈ [32, 64]) は実 trace 12 (cluster, cap) セル全てで保つ。さらに
**scan-heavy な cluster019 で j5 は orig 比 throughput −10〜−16 ns / hit ratio +0.6〜+6.3pp の二重勝ち** —
SIEVE 単一 hand の scan-resistance 限界が shard 並列化で緩和される、という新しい利得。
synthetic Zipf だけでは見えなかった「実用ワークロードでむしろ強い」根拠。図 5 枚 (seaborn)。

### 2026-05-05-j5-vs-orig-2x-memfair.md
公平性の検証: j3/j5 の `order_cap = 2 * capacity` (tombstone 用ヘッドルーム、ghost cache 的補助 slot)
が「j5 の速度優位は memory hand-out のおかげ」という疑念を生むため、**orig に 2x cap を渡した
worst-case ハンデ** で再測定。3 レジームに分離 — **per_shard ≤ 12.5 (cap=100)**: orig_2x ハンデでも
j5 が 2-3x 圧勝 (SIMD scan + shard 並列が pointer chase を支配、memory advantage 由来でないと確定)。
**per_shard ≈ 125 (cap=1000)**: 拮抗、±数 ns。**per_shard = 1250 (cap=10000)**: j5 完敗 (低 skew で 3x 遅い、
線形 scan が L1d を踏み外す)。large-cap で j5 を使うなら per_shard を SIMD chunk に保て、
という Pareto 結論をメモリ公平条件で再確認。
