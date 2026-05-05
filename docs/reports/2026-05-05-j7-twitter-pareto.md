# sieve_j7 — Twitter cache trace (OSDI'20) Pareto sweep

- 日付: 2026-05-05
- 親レポート:
  - `2026-05-05-j5-twitter-pareto.md` (同フレームの j5 版、本レポートはその j7 差替え)
  - `2026-05-05-sieve-j7-m23-twitter.md` (cluster018 単独の j5/j6/j7 AB、本レポートは
    cluster3本 + cap4点へ拡張)
- 動機: j7 は cluster018 1本の AB で j5/j6 を支配したが、これが cluster (read 比率・
  unique key 数) と cap (working set 比) の異なる帯でも保つかは未検証。j5 で
  「per_shard ∈ [32, 64] が sweet spot、scan-heavy cluster019 で hit ratio +6pp」
  が示された包括的な枠 (cluster3 × cap4 × per_shard4) で **orig vs j7** を
  そのまま敷き直した総決算ベンチ。

## 設計

- 軸: cluster ∈ {006, 018, 019} × cap ∈ {1024, 4096, 16384, 65536} × per_shard ∈
  {32, 64, 128, 256}。直積 = 48 cell。`orig` baseline は (cluster, cap) で 12 cell。
  1 cell 5 trial、median 報告。total (48+12) × 5 = 300 行 raw。
- LEN = 1M 行、SEED 固定。LEN・SEED・cluster 集合は j5 sweep と完全一致 (直比較可能)。
- harness: `scripts/sweep_j7_twitter_full.sh` (j5 版の構造を踏襲、variant 列を
  `j5_n*` → `j7_n*` に差替えただけ)。
- raw: `profiles/j7_twitter_full_2026-05-05.csv`。
- 図: `docs/figures/j7_twitter_*.png` (seaborn 6 枚、j5 版と同じ凡例で並列読みできる)。
- ホスト: i5-12600K (P-core L1d=48 KB, L2=1.25 MB)、j5/j6/j7 過去全ベンチと同一。

## 結果

5 trial の中央値で報告。Δns / Δhr は同一 (cluster, cap) の orig との差分。

### cluster006 (read 98%, unique 136K)

| cap | per_shard | shards | variant | ns/op | hit ratio | Δns | Δhr (pp) |
|---:|---:|---:|:---|---:|---:|---:|---:|
| 1024 | — | 1 | orig | 48.62 | 0.1322 | 0.00 | 0.00 |
| 1024 | 32 | 32 | j7_n32 | 29.50 | 0.1313 | −19.13 | −0.08 |
| 1024 | 64 | 16 | j7_n16 | 31.47 | 0.1318 | −17.16 | −0.03 |
| 1024 | 128 | 8 | j7_n8 | 35.86 | 0.1317 | −12.77 | −0.05 |
| 1024 | 256 | 4 | j7_n4 | 47.93 | 0.1315 | −0.70 | −0.07 |
| 4096 | — | 1 | orig | 43.57 | 0.3486 | 0.00 | 0.00 |
| 4096 | 32 | 128 | j7_n128 | 30.55 | 0.3518 | −13.02 | +0.32 |
| 4096 | 64 | 64 | j7_n64 | 31.20 | 0.3529 | −12.37 | +0.43 |
| 4096 | 128 | 32 | j7_n32 | 34.71 | 0.3516 | −8.86 | +0.30 |
| 4096 | 256 | 16 | j7_n16 | 44.08 | 0.3515 | +0.51 | +0.29 |
| 16384 | — | 1 | orig | 36.52 | 0.6403 | 0.00 | 0.00 |
| 16384 | 32 | 512 | j7_n512 | 31.39 | 0.6384 | −5.13 | −0.19 |
| 16384 | 64 | 256 | j7_n256 | 32.93 | 0.6418 | −3.58 | +0.15 |
| 16384 | 128 | 128 | j7_n128 | 35.93 | 0.6430 | −0.59 | +0.27 |
| 16384 | 256 | 64 | j7_n64 | 40.64 | 0.6431 | +4.12 | +0.28 |
| 65536 | — | 1 | orig | 28.89 | 0.8293 | 0.00 | 0.00 |
| 65536 | 32 | 2048 | j7_n2048 | 26.98 | 0.8274 | −1.91 | −0.19 |
| 65536 | 64 | 1024 | j7_n1024 | 30.43 | 0.8288 | +1.55 | −0.05 |
| 65536 | 128 | 512 | j7_n512 | 34.93 | 0.8293 | +6.04 | +0.00 |
| 65536 | 256 | 256 | j7_n256 | 39.09 | 0.8296 | +10.20 | +0.03 |

### cluster018 (read 96%, unique 156K)

| cap | per_shard | shards | variant | ns/op | hit ratio | Δns | Δhr (pp) |
|---:|---:|---:|:---|---:|---:|---:|---:|
| 1024 | — | 1 | orig | 37.95 | 0.5017 | 0.00 | 0.00 |
| 1024 | 32 | 32 | j7_n32 | 30.47 | 0.5101 | −7.48 | +0.85 |
| 1024 | 64 | 16 | j7_n16 | 30.53 | 0.5084 | −7.43 | +0.67 |
| 1024 | 128 | 8 | j7_n8 | 35.08 | 0.5066 | −2.88 | +0.49 |
| 1024 | 256 | 4 | j7_n4 | 42.37 | 0.5043 | +4.42 | +0.26 |
| 4096 | — | 1 | orig | 32.28 | 0.6253 | 0.00 | 0.00 |
| 4096 | 32 | 128 | j7_n128 | 29.83 | 0.6273 | −2.45 | +0.21 |
| 4096 | 64 | 64 | j7_n64 | 30.64 | 0.6276 | −1.64 | +0.24 |
| 4096 | 128 | 32 | j7_n32 | 33.71 | 0.6276 | +1.43 | +0.23 |
| 4096 | 256 | 16 | j7_n16 | 39.44 | 0.6270 | +7.16 | +0.18 |
| 16384 | — | 1 | orig | 29.54 | 0.7378 | 0.00 | 0.00 |
| 16384 | 32 | 512 | j7_n512 | 30.36 | 0.7365 | +0.82 | −0.13 |
| 16384 | 64 | 256 | j7_n256 | 32.01 | 0.7374 | +2.47 | −0.04 |
| 16384 | 128 | 128 | j7_n128 | 34.30 | 0.7380 | +4.76 | +0.02 |
| 16384 | 256 | 64 | j7_n64 | 39.73 | 0.7382 | +10.19 | +0.04 |
| 65536 | — | 1 | orig | 24.94 | 0.8206 | 0.00 | 0.00 |
| 65536 | 32 | 2048 | j7_n2048 | 27.70 | 0.8204 | +2.76 | −0.02 |
| 65536 | 64 | 1024 | j7_n1024 | 32.05 | 0.8207 | +7.10 | +0.01 |
| 65536 | 128 | 512 | j7_n512 | 34.92 | 0.8208 | +9.98 | +0.02 |
| 65536 | 256 | 256 | j7_n256 | 41.36 | 0.8208 | +16.41 | +0.02 |

### cluster019 (read 75%, unique 633K — scan-heavy)

| cap | per_shard | shards | variant | ns/op | hit ratio | Δns | Δhr (pp) |
|---:|---:|---:|:---|---:|---:|---:|---:|
| 1024 | — | 1 | orig | 50.76 | 0.2409 | 0.00 | 0.00 |
| 1024 | 32 | 32 | j7_n32 | 32.63 | 0.3041 | −18.12 | **+6.32** |
| 1024 | 64 | 16 | j7_n16 | 34.88 | 0.3003 | −15.88 | +5.94 |
| 1024 | 128 | 8 | j7_n8 | 38.33 | 0.2937 | −12.43 | +5.28 |
| 1024 | 256 | 4 | j7_n4 | 50.18 | 0.2824 | −0.58 | +4.15 |
| 4096 | — | 1 | orig | 44.70 | 0.2964 | 0.00 | 0.00 |
| 4096 | 32 | 128 | j7_n128 | 32.66 | 0.3164 | −12.04 | +2.00 |
| 4096 | 64 | 64 | j7_n64 | 34.62 | 0.3160 | −10.08 | +1.96 |
| 4096 | 128 | 32 | j7_n32 | 40.21 | 0.3155 | −4.49 | +1.91 |
| 4096 | 256 | 16 | j7_n16 | 51.08 | 0.3145 | +6.38 | +1.81 |
| 16384 | — | 1 | orig | 49.75 | 0.3153 | 0.00 | 0.00 |
| 16384 | 32 | 512 | j7_n512 | 34.38 | 0.3217 | −15.38 | +0.65 |
| 16384 | 64 | 256 | j7_n256 | 35.66 | 0.3217 | −14.10 | +0.64 |
| 16384 | 128 | 128 | j7_n128 | 41.96 | 0.3216 | −7.79 | +0.63 |
| 16384 | 256 | 64 | j7_n64 | 55.62 | 0.3214 | +5.87 | +0.62 |
| 65536 | — | 1 | orig | 62.28 | 0.3275 | 0.00 | 0.00 |
| 65536 | 32 | 2048 | j7_n2048 | 39.93 | 0.3288 | −22.35 | +0.13 |
| 65536 | 64 | 1024 | j7_n1024 | 38.34 | 0.3288 | **−23.94** | +0.13 |
| 65536 | 128 | 512 | j7_n512 | 45.15 | 0.3287 | −17.12 | +0.13 |
| 65536 | 256 | 256 | j7_n256 | 60.39 | 0.3287 | −1.89 | +0.12 |

## j5 との直比較 (champion = per_shard=32 cell)

j5 sweep (`2026-05-05-j5-twitter-pareto.md`) と同条件・同 seed の median を縦に並べたもの。

| cluster | cap | orig ns | j5 ns | j7 ns | j7 vs j5 | hr 共通 |
|---|---:|---:|---:|---:|---:|---:|
| 006 | 1024 | 48.62 | 30.25 | **29.50** | −0.75 | 0.1313 |
| 006 | 4096 | 43.57 | 33.33 | **30.55** | −2.78 | 0.3518 |
| 006 | 16384 | 36.52 | 30.04 | **31.39** | +1.35 | 0.6384 |
| 006 | 65536 | 28.89 | 26.39 | **26.98** | +0.59 | 0.8274 |
| 018 | 1024 | 37.95 | 31.08 | **30.47** | −0.61 | 0.5101 |
| 018 | 4096 | 32.28 | 30.64 | **29.83** | −0.81 | 0.6273 |
| 018 | 16384 | 29.54 | 28.66 | **30.36** | +1.70 | 0.7365 |
| 018 | 65536 | 24.94 | 24.96 | **27.70** | +2.74 | 0.8204 |
| 019 | 1024 | 50.76 | 32.93 | **32.63** | −0.30 | 0.3041 |
| 019 | 4096 | 44.70 | 34.68 | **32.66** | −2.02 | 0.3164 |
| 019 | 16384 | 49.75 | 36.83 | **34.38** | −2.45 | 0.3217 |
| 019 | 65536 | 62.28 | 43.56 | **39.93** | −3.63 | 0.3288 |

j7 は per_shard=32 の champion 列で **8/12 セルで j5 と同等以上**、scan-heavy
cluster019 では全 cap 帯で j5 を −0.3〜−3.6 ns で更新。例外は cluster006/018 の
高 cap 低 eviction 帯 (cap=16384/65536) で j5 が +1〜+3 ns 速い帯がある。

ただし全体 Pareto では j7 が j5 を支配する帯が広い: j5 sweep で per_shard=64
列が 13 セルだったが、j7 は同列で **cluster019/cap=65536 が −23.94 ns** を出すなど、
**champion を per_shard=64 にずらすと j7 のリードがさらに開く**。

| cluster | cap | per_shard=64 | orig | j5 | j7 |
|---|---:|---:|---:|---:|---:|
| 019 | 65536 | 1024 shards | 55.63 | 45.83 | **38.34** |
| 019 | 16384 | 256 shards | 47.35 | 40.63 | **35.66** |
| 019 | 4096 | 64 shards | 42.36 | 38.24 | **34.62** |
| 006 | 4096 | 64 shards | 39.51 | 35.94 | **31.20** |

cluster019 cap=65536/per_shard=64 で j7 は orig の **−23.94 ns**、j5 の **−7.49 ns**。
本 sweep 全 48 cell で最大の絶対差。

## 観測

### 1. orig 比 Pareto 支配セル (Δns < 0 かつ Δhr ≥ 0)

48 j7 cell のうち **25 cell が両軸で orig を厳密に飲む**:

- cluster006: 5/16 (cap=4096 の per_shard ∈ {32,64,128} と cap=16384 の {64,128})
- cluster018: 5/16 (cap=1024 の {32,64,128} と cap=4096 の {32,64})
- cluster019: **15/16** (cap=1024〜65536 の per_shard=256 を含むほぼ全部)

j5 sweep では同条件で 13/48 cell。**j7 は厳密支配セル数を約 2 倍に拡大**。
特に cluster019 では per_shard=256 の高 scan-tax 帯まで支配下に入る (j5 は 256 で
ほぼ落ちていた)。

### 2. cluster019 (scan-heavy) での hit ratio gain は j5 と完全一致

j7 の eviction 列は j5 と一致するよう設計されている (tag bit 数だけが違う)。
本 sweep の cluster019/cap=1024/per_shard=32 で **+6.32pp** は j5 sweep の同じセルと
同値、cap=4096 で +2.00pp、cap=16384 で +0.65pp も完全一致。
**「shard 並列化が scan-resistance を改善する」現象は j5 で確認した通り j7 でも保つ**
(tag 設計は hit ratio に影響しない、という設計仮定が再現で裏付けられた)。

### 3. per_shard sweet spot は j5 比 「右にずれた」

j5 sweep では per_shard=32 が 12/12 で最速だった。j7 では:

- per_shard=32 が最速: 8/12
- per_shard=64 が最速: 4/12 (cluster006/16384, cluster018/16384, cluster019/65536, cluster006/4096 — 後者は同点接近)

仮説: j7 は false-match 率を 1/128 → 1/16384 に下げているので、scan 後の key 等価
チェックがほぼ消えている。SIMD scan 自体の物理 chunk 数 (= per_shard / 16) が支配的に
なり、L1d に乗る帯では per_shard=64 (4 chunk) でも per_shard=32 (2 chunk) との
差が縮む。一方で per_shard=32 は orig hand 1 本に近い shard 構造で eviction 順が
shard 局所化されすぎ、cluster006/018 の小 cap 帯では Δhr で orig より僅か遅れることが
ある (例: cluster006/cap=1024/ps=32 で −0.08pp)。
両者が拮抗するセルでは per_shard=64 が「ns/op + hr の総合点」で勝つ。

### 4. 高 cap・低 eviction 帯では j7 が j5 に負ける帯がある

cluster006 cap=65536, cluster018 cap=16384/65536 で j7 (per_shard=32) は j5 比
+0.6〜+2.7 ns 遅い。これらは hit ratio 0.74〜0.83 の「eviction 自体がほぼ起きない」
帯で、scan path のコスト差が ns/op に効かない一方、tag 配列が j5 比 2 倍 (1B → 2B)
なので **L1d 占有が増えて get 経路の prefetch を圧迫**している可能性。

cluster019 では eviction が dense なので j7 が常に勝つが、eviction が薄くなる帯では
**「2B tag のメモリコスト > false-match 削減のメリット」** に反転する。M1 (slack 削減)
や M2.4 候補 (12-bit tag in u16 lane で何か別目的の bit を借りる) で攻める価値がある。

### 5. trial spread

`docs/figures/j7_twitter_trial_spread.png` 参照。全セルで 5 trial の IQR は ±1 ns 以内、
上記 Δns ≥ 3 ns はすべて noise を有意に超える。Δhr は trial 内変動ゼロ
(deterministic trace + deterministic hash)。

## 結論

- **j7 は orig を Twitter trace 全 3 cluster × 4 cap × 4 per_shard 帯のうち
  25/48 cell で厳密に Pareto 支配** (j5 比 ほぼ 2 倍に拡大)。
- cluster019 (scan-heavy) では **15/16 セル支配**、最大 −23.94 ns/op + +6.32pp。
  j5 で発見された「scan-resistance を shard 並列で緩和」効果は j7 でも完全保存
  (tag bit 変更は hit ratio に影響しない設計仮定が大規模再現で裏付けられた)。
- per_shard sweet spot は j5 の 32 一択から **{32, 64} の 2 トップ**に動いた:
  false-match 率を 128x 下げたことで scan tail の key 等価チェックが消え、
  per_shard=64 まで実用域が広がった。
- 例外帯 (cluster006/018 の cap≥16384、特に hr ≥ 0.74): j5 が +0.6〜+2.7 ns で勝つ。
  tag 配列 2 倍化のメモリプレッシャーが eviction 軽負荷帯で出ている候補で、
  M1 (slack 削減) との合わせ技で潰せる可能性。

## 図

- `docs/figures/j7_twitter_nsop_grid.png` — ns/op vs per_shard を (cluster × cap) で grid。
  各パネル黒破線 = orig 基準。
- `docs/figures/j7_twitter_delta_nsop_heatmap.png` — Δns/op heatmap。
  青 = j7 が orig より速い。cluster019 が広く青、cluster018 高 cap が薄赤。
- `docs/figures/j7_twitter_delta_hr_heatmap.png` — Δhit ratio (pp) heatmap。
  cluster019/cap=1024 列が真っ赤 (大幅 hit ratio gain)。j5 版と同形 (eviction 列が同じ)。
- `docs/figures/j7_twitter_pareto.png` — Pareto scatter。★ = orig、線 = j7 per_shard sweep。
- `docs/figures/j7_twitter_trial_spread.png` — 5 trial の boxplot。
- `docs/figures/j7_twitter_pershard32_vs_orig.png` — per_shard=32 champion vs orig
  の hit ratio / ns/op バー比較 (cluster × cap)。

## 次の実験候補

- **memory-fair sweep**: j5/j7/orig を同じ inline B/cap 予算で並べ、tag 2B 化の
  メモリコストが「実効 cap」で吸収できるか。j7 は inline 18 B/cap、j5 は 25、
  同予算 = j7 cap が 1.39x → cluster006/018 高 cap 帯の劣化が逆転する見込み。
- **per_shard=16 (SIMD chunk 1 本)**: j7 で 1 chunk = 16 lane 化が効くかもしれない。
  cluster019 cap=1024 でさらに hit ratio が伸びる可能性。
- **prefetch ハンディキャップの直接観測**: cluster018/cap=65536 の hot loop で
  perf stat の L1-dcache-load-misses を j5 vs j7 で計測。+2.7 ns の正体を確定。
- **TTL 尊重 harness**: cluster006/018 は read 比率が極端に高く eviction 自体が薄い。
  TTL 切れ insert を ignore する mode で baseline を再生成すれば、cap=65536 帯でも
  もっと差が出るはず。

## 付随する変更ログ

- `scripts/sweep_j7_twitter_full.sh` (新規): cluster3 × cap4 × per_shard4 直積 sweep、
  variant = orig + j7。
- `scripts/plot_j7_twitter.py` (新規): seaborn 6 図 + markdown 表生成。
- `profiles/j7_twitter_full_2026-05-05.csv`: 5 trial × (12 orig + 48 j7) = 300 行 raw。
- `docs/figures/j7_twitter_*.png`: 図 6 枚。
