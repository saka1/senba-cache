# docs/reports インデックス

実験・調査ごとのレポート一覧。新しいレポートを追加したらこのファイルも更新する。

## テーマ別マップ

| テーマ | 関連レポート |
|---|---|
| ベンチ条件の整備 | realistic-workload-bench |
| v0〜v3 系列 (linked-list + array) | v0-divergence, realistic-workload-bench, v3-bench, v3-profile |
| orig 改善 (MaybeUninit) | sieve-orig-overhead-analysis |
| 外部実装調査 | jedi-vs-orig |
| 外部実装比較 (W-TinyLFU) | j8-vs-mini-moka-twitter |
| 設計アイディア集 (living doc) | `../improvement-ideas.md` (旧 `2026-05-04-improvement-ideas.md`、`docs/` 直下に移動) |
| j3 系列 (Map なし SIMD) | sieve-j3-bench |
| j4 系列 (set-associative j3) | sieve-j4-set-associative, sieve-j4-crossover-and-shard-sweep, sieve-j4-pershard-vs-footprint |
| j5 系列 (j4 の double-hash 排除) | sieve-j5-doublehash-ab, j5-pershard-pareto, j5-twitter-pareto, j5-vs-orig-2x-memfair |
| j6 系列 (M2.1: visited を tag に同居) | sieve-j6-m21-twitter |
| j7 系列 (M2.3: tag を u16 化、visited + 14-bit hash) | sieve-j7-m23-twitter, j7-twitter-pareto |
| j8 系列 (M5.3 + tag 内 ID embed + free_list 廃止) | sieve-j8-bench, j8-candidate-loop-analysis, j8-c-hoist, j8-twitter-pareto |
| c8 系列 (j8 並行版: read lock-free + write per-shard Mutex) | c8-design, c8-vs-moka-thread-sweep |
| 5 cluster ベース sweep (cluster006/016/018/019/034) | st-twitter-5cluster |
| ライブラリ化 (`senba::Cache` 公開 API) | senba-sievecache-design, twitter-string-keys, senba-twitter-string-sweep, sieve-cache-shift-on-evict |

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

### 2026-05-05-sieve-j6-m21-twitter.md
M2.1 (visited を tag バイトの bit6 に同居させ Entry padding を消す) の単独実装 `sieve_j6` を
Twitter cluster018 × cap ∈ {1024, 4096, 16384} × per_shard ∈ {32, 64, 128} で j5 と AB。
**全 9 cell で j5 より +2.5〜+11.3 ns/op 遅化**、improvement-ideas の「hit-path 改善」予想は棄却。
劣化幅は per_shard (= scan 長) に比例し、AVX2 経路の `vpand` 1 命令増 + visited クリア RMW の
port 競合疑惑が候補。correctness は確定 (j5 と外部完全一致 — hits/misses/evictions が同一)。
inline footprint -28% は構造的に達成。次は memory-fair sweep で「同じメモリ予算で j6 が j5 を抜くか」を測る。

### 2026-05-05-sieve-j7-m23-twitter.md
M2.3 (tag を u16 化、live + visited + 14-bit hash) の単独実装 `sieve_j7` を Twitter cluster018 ×
cap ∈ {1024, 4096, 16384} × per_shard ∈ {32, 64, 128} で orig / j5 / j6 と AB。
**j7 は 9 cell 中 8 で j5 を支配** (Δ −1.4〜−9.2 ns/op)、j6 比は全 cell で −1.1〜−19.4 ns/op。
唯一の例外 (cap=16384, per_shard=32) でも +0.94 ns の誤差レベル。
per_shard=128 帯で利得が最大化 (j6 と真逆の傾き) → j6 の劣化主因は false-match 率倍増 (1/128→1/64)
であり、tag bit を増やす方向が正解という解釈。inline B/cap は j5 比 −14、j6 比 +2 で、
j7 は **memory も throughput も j5/j6 の良いとこ取り**。M2.1 (j6) の方針自体は正しく、tag bit を
削りすぎた点だけが失敗だったと確定。

### 2026-05-05-j7-twitter-pareto.md
j5 twitter sweep と同フレーム (cluster ∈ {006, 018, 019} × cap ∈ {1024, 4096, 16384, 65536} ×
per_shard ∈ {32, 64, 128, 256}、48 j7 cell + 12 orig cell × 5 trial) で **orig vs j7** の総決算。
**j7 は 25/48 cell で orig を厳密 Pareto 支配** (j5 sweep の 13/48 から ほぼ 2 倍に拡大)。
cluster019 (scan-heavy) では **15/16 cell 支配**、cap=65536/per_shard=64 で −23.94 ns/op が
本 sweep 最大差。hit ratio gain は j5 と完全一致 (eviction 列は tag bit 数に依存しない)、
+6.32pp at cluster019/cap=1024 を再現。per_shard sweet spot は j5 の 32 一択から **{32, 64} の 2 トップ**
に動き、false-match 率 128x 低下で scan tail コストが消えたことで per_shard=64 まで実用域が広がった。
例外帯は cluster006/018 の cap≥16384 高 hit ratio 帯で、j5 が +0.6〜+2.7 ns 速い (tag 配列 2 倍化の
L1d 圧迫が候補)。次は memory-fair sweep で 2B tag のメモリコストを cap で吸収できるか確認する。

### 2026-05-05-sieve-j8-bench.md
`2026-05-05-sieve-j8-design.md` 設計 (= §M5.3 + tag 内 entry_id embed + free_list 廃止、inline 20 B/cap) の初回検証。
Twitter cluster018 × cap ∈ {1024, 4096, 16384} × **per_shard=64 固定** で orig / j7 / j8 を 5 trial AB。
**eviction 列は j7 と bit-exact 一致** (oracle test + bench カウンタで二重確認、SIEVE 意味論 OK)。
throughput は **j7 比 +1.9〜+4.1 ns/op 退行**、机上検討の +0.5〜+1 ns を 2〜8 倍上回る。
cap=1024 では j8 は orig を 2.76 ns 引き離すが、cap=4096/16384 では orig に負ける (+4.4 ns)。
2026-05-06 改訂: samply で命令レベル profile を取り、初稿の「entries[id] が scattered で
L1 prefetch 不発」仮説は **棄却** (per_shard=64 では 1 shard が L1 内に収まり access pattern
が効かない、cmp[entries] の skid サンプルが j7=553 vs j8=561 で同一)。退行の真因は
**(a) dep chain 延長 (movzbl tags[pos] が L1-hit 1 回追加 → +~1.2 ns/op) + (b) hash bit
14→8 で false-match 率 64x 増 (1/16384→1/256、+~0.9 ns/op)** の 2 つで、両方 tag bit 配分の
構造的コスト。(b) は per_shard を下げれば消える成分なので per_shard ∈ {16, 32, 64} sweep
で (a)+(b) 分解を実測する D' が次手の最有力。memfair sweep A は別軸で並走可。
2026-05-06 §8 追加: D' 完了。**per_shard=16 で Δ(j8−j7) は平均 +0.14 ns まで縮み、
cap=16384 では j8 が j7 を 1.55 ns 上回る**。(b) 成分が per_shard で消えることを実測で
確認、§4.5 の予測を裏付け。**per_shard=16 では 3 cap 全てで j8 が orig を absolute に
上回る** (−7.88, −1.13, −2.08 ns)。j8 の真の sweet spot は per_shard=16 (設計時想定の
32〜64 とは逆向き)。残る検証は memfair sweep A。

### 2026-05-06-j8-candidate-loop-analysis.md
`sieve-j8-bench` §4.4 の (a)/(b) 分解を再構成した解析ノート (新規ベンチなし、既存 profile + asm 再読)。
退行は独立 2 項ではなく **inner candidate ループの per-candidate コスト × candidate 数** の
単一構造であることを示す。j8 で追加した id 抽出 4 命令 (movzbl/and/shl/cmp) は inner
ループ本体に居て、true match と false match の **どちらでも発火する**。よって false-match
率が上がると id 抽出のコストも比例で積み増される (j8 で 433/409 sample が ≈ 同数だった
ことから samply 経由でも確認可能)。Δ単一式 = `5cy × N_cand_j8 + 7cy × ΔN_false` で per_shard
依存を再導出 — per_shard=16 では candidate 数自体が j7 と差が無くなるので退行は実質ゼロ
になる。Rust ↔ Intel asm の対応、samply 数値の再解釈、設計上の逃げ道がない理由 (id 計算
は cmp[entries[id]] の前提なので inner ループ外に出せない)、sweet spot=per_shard=16 への
収斂までを集中議論。**§8 で命令レベル最適化 2 案 (BLSR ×2 + sizeof(Entry)-aware bit
レイアウト) を提案、§10 で `src/sieve_j8.rs` に両方適用して実測**: cap=4096/per_shard=64
の最退行 cell で −11.9% (−4.69 ns/op)、cap=16384/per_shard=16 では新 j8 が orig を 2.5%
上回り memory 20 B/cap の利得を保ったまま throughput 並走を達成。inner asm は
`movzwl + and 0x3f0 + cmp+load + blsr ×2` (Path A 17→16 cy、Path B 7→2 cy) に短縮。

### 2026-05-06-j8-c-hoist.md
`j8-candidate-loop-analysis` §8.4(c) (chunk 先頭 byte pointer を outer に hoist し
`bit = tzcnt(mask)` をそのまま byte offset として使う最適化) を `src/sieve_j8.rs`
に適用し cluster018 sweep (5 trials × 3 cap × 3 per_shard) で実測。
inner ループから `mov+shr (lane=bit>>1) + or (pos=i+lane)` の 3 ops を追い出して
success path 限定にし、Path A は 16→14 cy、inner ops は 7→5。
**9 cell 中 8 cell で改善**、最大 −5.74% (cap=1024/ps=64)。
運用 sweet spot (per_shard=16) では cap=1024/4096/16384 の **3 cap 全てで orig を
absolute に上回る** (−20.31% / −3.76% / −1.55%)。memory 20 B/cap の利得を保ったまま
throughput でも勝つ、という当初目標に到達。inner ループ単独最適化は本稿で打ち止め、
次は load latency hide (prefetch / chunk overlap) が打ち手候補。

### 2026-05-06-j8-twitter-pareto.md
`sieve_j8` の累積最適化 (M5.3 + tag-id embed + free_list 廃止 + BLSR×2 + sizeof-aware
layout + c-hoist) を 2026-05-06 時点で `j7-twitter-pareto` と同じ枠組み
(cluster {006,018,019} × cap {1024,4096,16384,65536} × per_shard {16,32,64}, 5 trials)
で全数測定し、per_shard 横断サマリで champion を選定したスナップショット。
**per_shard 中央値**: 16 で −11.15 ns (9/9 cell で orig 超え)、32 で −8.12 ns
(9/12 速度勝ち + 7/12 HR 勝ち)、64 で −4.44 ns。cap=65536 を含めるため
**champion=per_shard=32** を採用し `docs/figures/j8_twitter_pershard32_vs_orig.png`
を作成 (`j7_twitter_pershard32_vs_orig.png` の j8 版)。退行は cluster018/cap≥4096 に
局所化 (+0.2〜+2.3 ns) — `j8-candidate-loop-analysis` で示した false-match 退行域と一致。
cluster019 では全 cap で −9〜−20 ns、cap=1024 で +6.32 pp の HR 利得を保持。
memory 20 B/cap の利得を保ったまま 3/4 cluster で throughput 勝ちという j8 系列の当初目標に到達。

### 2026-05-06-j8-vs-mini-moka-twitter.md
`sieve_j8` (per_shard=32) と外部 W-TinyLFU 実装 `mini-moka 0.10.3` の Twitter trace 上 HR + ns/op screening。
default 設定の mini-moka に対し **j8 が 12/12 cell で HR を支配** (Δ +1.92〜+28.09pp、中央値 +10pp 帯)、
ns/op は **12〜16× の差で j8 が速い** (mini_moka は concurrent primitive + 毎 op sync 込み)。
cluster019/cap=1024 では mini_moka の HR が 3.4% に崩壊する一方 j8 は 30.4% を維持、scan-resistance も
SIEVE 側が優位。Zipf 1.0 sanity (mini_moka が orig と HR ±1pp で並ぶ) で adapter 機能正常を確認、
Twitter の崩壊は default-tuned W-TinyLFU 実挙動と確定。初版で `ConcurrentCacheExt::sync()` 呼び忘れ
が露見し、修正再測定 (HR ±0.5pp で結論不変、ns/op +10〜15%) — 教訓を §9 に post-mortem。
公平性 caveat 全考慮でも HR ±1pp 同等性すら成立しないため、API 整理に投資する妥当性を確認。
図 3 枚 (HR bar / ns/op log bar / Pareto)。
**§10 拡張**: moka 0.12.15 + Zipf skew {0.6,0.8,1.0,1.2} sweep を追加し、(a) moka 0.12 と
mini-moka 0.10 の HR は 28/28 cell で Δ ≤ 0.1pp に収束 (adaptive window 効果は 1M op で
非観測、moka 0.12 は mini-moka より 1.5〜2× 遅い)、(b) Zipfian では W-TinyLFU が SIEVE と
HR competitive (skew=1.2 で完全並び、skew=0.6/0.8 の cap=16384 では W-TinyLFU が +0.75pp 微勝ち)、
(c) Twitter cluster018/019 でだけ W-TinyLFU が −10〜−28pp 崩壊、を確認。結論「W-TinyLFU は
Zipf-like で強く、non-Zipf 実 trace で脆い、SIEVE は両軸で robust」に更新。図 3 枚追加。


### 2026-05-06-c8-design.md
`sieve_c8` = `sieve_j8` の並行版 (read lock-free + write per-shard Mutex) 第一手。設計のキモは
**seqlock-via-tag** — j8 が tag (u16) 内に `LIVE | VISITED | id (6bit) | hash (8bit)` を
詰めている性質をそのまま seqlock の sequence number 兼 locator として再利用する。
reader は tag 1 回読み → entries[id] raw read → tag 再 load (re-validate)、t1 == t2 && LIVE
のみ採用。`K, V: Copy` 制約で torn read の Drop / Clone 伝播を断つ。`parking_lot::Mutex` を
shard ごとに置き writer 直列化。AVX2 path も搭載 (SIMD scan は best-effort filter として
位置付け、候補は scalar 用 seqlock dance で再検証)。実装中に **phantom non-empty tag**
バグ発見・修正 — reader の `fetch_or(VISITED)` が evict 直後の EMPTY tag に発火すると
`0x4000` (LIVE 無し・VISITED のみ) が残り writer の `t != EMPTY` 判定が崩れるレース。
判定を `t & LIVE != 0` に統一して解消、20/20 連続 invariants test pass。生成 ASM 目視で
`find_get_avx2` が合法 x86_64 (`vpand ymmword ptr / vpcmpeqw / vpmovmskb / blsr×2 /
tzcnt / lock or` 系) のみで構成されていることを確認。第一手 smoke (cap=512, ops=4M):
**1T 10.3 → 4T 19.2 Mops/s (skew=1.0)**, **1T 15.6 → 4T 30.8 Mops/s (skew=1.2)**, thread CV
≤ 0.04 で Mutex 競合ほぼ観測なし。1T overhead vs j8 = +42 ns/op (見積もり +10〜20 を上回る)、
内訳推定は parking_lot ~12 ns + fetch_or ~10 ns + seqlock dance ~15 ns。次手は SHARDS=32/64
sweep と memfair 比較。


### 2026-05-06-st-twitter-5cluster.md
既存 cluster006/018/019 に **cluster016 (cat 3, Zipf 1.79, miss 7.3%)** と
**cluster034 (cat 2, Zipf 1.14, miss 8.2%)** を追加した 5 cluster で orig / j8 (per_shard=32) /
moka 0.12 / mini-moka 0.10 をシングルスレッド再測定。**cluster034**: 全 4 cap で j8 が
−10〜−15 ns 圧勝、HR ±0.05pp → 「j8 利得帯」を **Zipf ≤ 1.5** に拡張できることを示唆。
**cluster016**: cap=1024/4096 で利得、cap=65536 で +4.89 ns 退行 — 退行は cluster018 固有
ではなく **Zipf ≥ 1.79 + 高 hit ratio** の構造的特性と確定。W-TinyLFU の HR 崩壊は cat 3 でも
再現 (cluster016 で −16 pp)、SIEVE は 20 全 cell で moka/mini-moka 以上の HR。
今後の比較ベースは 5 cluster (`scripts/sweep_st_twitter_5cluster.sh`)。図 4 枚 (HR / ns/op /
Pareto 4-family / Pareto SIEVE-only)。

### 2026-05-06-c8-vs-moka-thread-sweep.md
`bench_concurrent.rs` を generic 化して c8 / moka 0.12 / mini-moka 0.10 を同一 harness で並列比較。
**§1** (初版, cap=512 / SHARDS=8 / keys=10k): c8 は harness 側の `SHARDS=8` 固定で hot shard contention
が強く露出、scaling 1.87x at T=8 で頭打ち。**§拡張 (メイン結論)**: harness を `--shards` 受けに改修、
**cap=16384 / SHARDS=256 / keys=1M / T={1,2,4,8,16}** で再測定、HR を 3 者 0.688〜0.690 に揃えた純粋な
並列性比較を実施。Mops/s (T=1→T=16): **c8 8.5 → 50.9 (5.99x、near-linear)**, **moka 1.96 → 3.30
(1.68x、T=4 で天井、T=8/16 で逆に regress)**, **mini-moka 2.19 → 6.05 (2.76x、T=8 ピーク後微減)**。
T=16 で **c8/moka = 15.4x**。p99 chunk latency: c8 195→680 ns、moka 995→**10162 ns**、mini-moka
829→4888 ns。並列モデル考察: c8 の lock-free read + per-shard Mutex writer は SHARDS=256 で hot key
が 1/256 shard に集中するだけ、残り 255 shard 無競合で near-linear。moka 0.12 は 4 thread 以上で
writer 経路が contended になり pending tasks queue が膨らむため thread 増で throughput が悪化する
(単に flat ではなく負の効果)。adaptive window は HR ベネフィットが出る代わりに内部 state contention
で並列性は mini-moka 0.10 より悪化。後続候補: skew sweep, read-heavy 比 (95% read), 96-core 機での
スケール上限。

### 2026-05-06-senba-sievecache-design.md
j8 系列 / c8 系列の研究結果をベースに **publishable な Rust crate API** としての ST 版
`senba::Cache` を確定する設計ドキュメント (実装着手前)。crate identity が SIEVE で
固定されている前提で top-level type 名を簡潔に `Cache` に取り、既存 placeholder trait
`Cache` は `CacheImpl` に rename して bench/oracle 共通基盤として残す (12 sieve_*
modules の impl 行を一括書き換え)。設計の核は **`SlotSize` sealed trait** (`Slot16` /
`Slot32` (default) / `Slot64`) で entries arena の stride を SLOT::SIZE 固定にする
padding 自動化と、それによる `Entry<K, V>` の sizeof 制約 (j8 では 2 冪 + ≤256) の解消。
`(u64, String)`=32, `(String, String)`=48 の典型 string-cache ケースをサポート。
j8 の c-hoist trick (`tag & ID_MASK = id × S::SIZE`) は SLOT 単位で同型に維持して
throughput を保つ。`remove` API は per-shard swap-to-fill-gap (O(per_shard) ≤ O(64))
で warm-up 不変条件 I8 を回復させ free_list を持たない構造を維持。memory は default
Slot32 で `(u64, u64)` の場合 36 B/cap (j8 fast-path 20 → +80%) の意識的な tax を
払う代わりに、任意の K, V (sizeof ≤ 64) で動く API と `remove` を獲得する。並行版・
`generic_const_exprs` 化・crate rename (`senba_cache` → `senba`) は scope 外、
別スペック予定。

### 2026-05-06-twitter-string-keys.md
`senba::Cache<K, V, S, SHARDS>` が任意 K を取れるようになったので Twitter trace の
anonymized_key を **u64 に pre-hash せず** `Cache<String, u64>` にそのまま流す経路を
bench に追加 (`drive_str` + `--source twitter-string`)。`twitter_csv_from_path_string`
追加、`senba::Cache` に `CacheImpl` 実装追加。HR は pre-hash 版と完全一致 (500K 行で
DefaultHasher 衝突ゼロ)。orig は String 化で ns/op が 2.1-2.3x 重く、senba::Cache は
String キーでも orig-string の 1.4-1.8x 高速。`senba::Cache` の per-shard ≤ 64 制約に
より SHARDS=8 default では capacity ≤ 512 のみベンチ可能。j7/j8 への String 経路拡張は
今後。

### 2026-05-06-senba-twitter-string-sweep.md
`senba::Cache` (新公開 API) を **生 String キー**のまま Twitter trace 5 cluster ×
cap{1024..65536} × {orig, senba ps32, senba ps64} × 3 trials で sweep。**20 cell 全てで
senba_ps32 が orig を ns/op で支配** (−15〜−101 ns、cap・scan-heavy 度に応じ拡大)。
HR は ±0.9 pp 以内で実質一致、ただし scan-heavy な **cluster019 で +6.32 pp / +2.00 pp**
と senba が大勝 (j5/j8 で観測した「shard 並列化が SIEVE の hand scan-resistance を補強」
現象が String キーでも再現)。per_shard=32 が 64 に微差で勝ち sweet spot を踏襲。
SHARDS は cap/per_shard で 16〜2048 まで動的選択 (per-shard ≤ 64 制約) — `bench.rs` に
`senba_n{16..2048}` variant を String 経路に追加して実現。

### 2026-05-06-sieve-c-vs-senba-twitter52.md
libCacheSim の C リファレンス SIEVE (`cachesim` + `Sieve.c`) と senba の Rust 実装
(`sieve_orig`, `senba::Cache`) を **同一トレース・同一 cap で別々に** 走らせ wall-clock
で比較したシステム比較。`twitter_cluster52.csv` 100 万 req、footprint 143542、cap
{144, 1435, 14354}、taskset -c 0、5 run median。HR は cachesim Sieve と senba::sieve_orig
で全 cap 完全一致 (0.4881 / 0.3216 / 0.1973)。**初回 (CSV 直読み) MQPS: cachesim ~2.5、
senba::sieve_orig 27.8〜42.9 → 11〜17× gap**。follow-up で `traceConv` により
oracleGeneral binary 形式に変換して再測すると **cachesim は 7.5 MQPS まで 3× 跳ね、
gap は 4〜6× に縮小**。CSV パースが cachesim wall-clock の ~⅔ を占めていたと判明。
残り 4〜6× の差は libCacheSim の `cache_t` vtable / glib hashtable / `request_t`
plumbing などのハーネス本体に由来する見込み。アルゴリズム単体比較は依然 FFI で C
実装を senba bench に取り込む follow-up (option b) が必要。`workload::file::libcachesim_csv_from_path`
と `--source libcachesim-csv` を bench に追加して両ツールが同じ key を見ることを保証。

### 2026-05-06-sieve-cache-shift-on-evict.md
`senba::Cache` (`src/sieve_cache.rs`) から **`compact` 経路と `tail` フィールドを撤去**し
**shift-on-evict** モデルに書き換えた refactor 記録。append-at-tail だった `Inner::insert`
を「evict 位置で `tags[pos+1..len]` を memmove で1つ前にシフト → 末尾 `tags[len-1]` に
新 tag 書き込み」に変更。フラット配列が常に `sieve_orig` の "tail (oldest) → head
(newest)" 順と対応するため oracle 等価性 (eviction 列の byte-for-byte 一致) を維持。
途中試した単純な in-place reuse (新 tag を evict 位置にそのまま書く) は、visited
bit が絡むケースで `sieve_orig` と発散することを発見し却下。`tags` 配列が
`2*capacity` から `capacity` に半減し、SIMD scan 窓もそれに合わせて縮小。
**perf-gate 3 シナリオ全てで改善** (insert_u64 −2.9%, mixed_u64 −3.7%, insert_string
−1.0%)。`for` ループ shift だと mixed_u64 が +4.8% リグレッションだったが
`Vec::copy_within` (memmove) で改善に転じた点は要記憶。新不変量 I4'
(`tags[0..len]` は穴なし全 LIVE) を導入。
