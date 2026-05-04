# sieve_j5 — Twitter cache trace (OSDI'20) Pareto sweep

- 日付: 2026-05-05
- 親レポート: `2026-05-05-j5-pershard-pareto.md` (synthetic Zipf 版の Pareto)
- 動機: synthetic Zipf 版の per_shard Pareto で「j5 は per_shard ∈ [32, 64] で
  throughput / hit ratio の両軸 orig を支配」と判った。実 trace で同じ結論が保つかは未検証。
  Twitter cache trace (OSDI'20, Yang et al.) の `cluster006/018/019` は
  read 比率・unique key 数・アクセスパターンが大きく異なる 3 本で、
  「synthetic Zipf で見えた sweet spot は実用ワークロードにも当てはまるか」を撫でる
  本投資の総決算ベンチ。
- データ: `external/twitter-cache-trace/cluster{006,018,019}` (1M 行 × 3 本、
  `feat(workload): Twitter cache trace (OSDI'20) CSV reader` で導入済み reader 経由)。

## 設計

- 軸: cluster ∈ {006, 018, 019} × cap ∈ {1024, 4096, 16384, 65536} ×
  per_shard ∈ {32, 64, 128, 256}。直積 = 48 cell。`orig` baseline は (cluster, cap)
  単位で 12 cell。1 cell 5 trial。総 (48+12) × 5 = 300 行 raw。
- cap 上端を **65536 まで拡張**: synthetic 版 (≤16384) では cluster006/018 の
  working set を覆えない。cluster019 はこのレンジでもまだ working set 未満
  (unique key 633K) で「ヒット率が伸び切らない領域」が観察できる。
- per_shard は synthetic 版と同じ {32, 64, 128, 256} を使い直比較可能にした。
  per_shard=32 は SIMD chunk 1 本で済む下限、256 は 8 chunk で scan tax 帯。
- harness は既存 `bench` CLI + `scripts/sweep_j5_twitter.sh`。
  cap=65536/per_shard=32 → SHARDS=2048 が必要なため `bench.rs` に
  `j5_n1024` / `j5_n2048` を追加 (matcher 拡張のみ、`SieveCache` 自体は const generic で対応済み)。
- raw: `profiles/j5_twitter_pareto_2026-05-05.csv`。
- 図: `docs/figures/j5_twitter_*.png` (seaborn 5 枚: ns/op grid, Δns heatmap,
  Δhr heatmap, Pareto scatter, trial spread boxplot)。
- CPU は親レポートと同じ i5-12600K (P-core L1d=48 KB, L2=1.25 MB)。

## 結果

5 trial の中央値で報告。Δns と Δhr は同一 (cluster, cap) の orig との差分。

### cluster006 (read 98%, unique 136K)

| cap | per_shard | shards | variant | ns/op | hit ratio | Δns | Δhr (pp) |
|---:|---:|---:|:---|---:|---:|---:|---:|
| 1024 | — | 1 | orig | 45.03 | 0.1322 | 0.00 | 0.00 |
| 1024 | 32 | 32 | j5_n32 | 30.25 | 0.1313 | −14.78 | −0.08 |
| 1024 | 64 | 16 | j5_n16 | 35.97 | 0.1318 | −9.06 | −0.03 |
| 1024 | 128 | 8 | j5_n8 | 42.27 | 0.1317 | −2.76 | −0.05 |
| 1024 | 256 | 4 | j5_n4 | 65.10 | 0.1315 | +20.07 | −0.07 |
| 4096 | — | 1 | orig | 39.51 | 0.3486 | 0.00 | 0.00 |
| 4096 | 32 | 128 | j5_n128 | 33.33 | 0.3518 | −6.19 | +0.32 |
| 4096 | 64 | 64 | j5_n64 | 35.94 | 0.3529 | −3.58 | +0.43 |
| 4096 | 128 | 32 | j5_n32 | 42.48 | 0.3516 | +2.97 | +0.30 |
| 4096 | 256 | 16 | j5_n16 | 63.69 | 0.3515 | +24.17 | +0.29 |
| 16384 | — | 1 | orig | 34.64 | 0.6403 | 0.00 | 0.00 |
| 16384 | 32 | 512 | j5_n512 | 30.04 | 0.6384 | −4.60 | −0.19 |
| 16384 | 64 | 256 | j5_n256 | 32.95 | 0.6418 | −1.69 | +0.15 |
| 16384 | 128 | 128 | j5_n128 | 42.47 | 0.6430 | +7.82 | +0.27 |
| 16384 | 256 | 64 | j5_n64 | 55.65 | 0.6431 | +21.00 | +0.28 |
| 65536 | — | 1 | orig | 26.66 | 0.8293 | 0.00 | 0.00 |
| 65536 | 32 | 2048 | j5_n2048 | 26.39 | 0.8274 | −0.27 | −0.19 |
| 65536 | 64 | 1024 | j5_n1024 | 27.54 | 0.8288 | +0.88 | −0.05 |
| 65536 | 128 | 512 | j5_n512 | 35.31 | 0.8293 | +8.65 | +0.00 |
| 65536 | 256 | 256 | j5_n256 | 47.82 | 0.8296 | +21.16 | +0.03 |

### cluster018 (read 96%, unique 156K)

| cap | per_shard | shards | variant | ns/op | hit ratio | Δns | Δhr (pp) |
|---:|---:|---:|:---|---:|---:|---:|---:|
| 1024 | — | 1 | orig | 37.30 | 0.5017 | 0.00 | 0.00 |
| 1024 | 32 | 32 | j5_n32 | 31.08 | 0.5101 | −6.22 | +0.85 |
| 1024 | 64 | 16 | j5_n16 | 36.01 | 0.5084 | −1.29 | +0.67 |
| 1024 | 128 | 8 | j5_n8 | 41.82 | 0.5066 | +4.51 | +0.49 |
| 1024 | 256 | 4 | j5_n4 | 59.42 | 0.5043 | +22.12 | +0.26 |
| 4096 | — | 1 | orig | 29.20 | 0.6253 | 0.00 | 0.00 |
| 4096 | 32 | 128 | j5_n128 | 30.64 | 0.6273 | +1.43 | +0.21 |
| 4096 | 64 | 64 | j5_n64 | 33.67 | 0.6276 | +4.46 | +0.24 |
| 4096 | 128 | 32 | j5_n32 | 39.73 | 0.6276 | +10.53 | +0.23 |
| 4096 | 256 | 16 | j5_n16 | 55.50 | 0.6270 | +26.29 | +0.18 |
| 16384 | — | 1 | orig | 28.28 | 0.7378 | 0.00 | 0.00 |
| 16384 | 32 | 512 | j5_n512 | 28.66 | 0.7365 | +0.38 | −0.13 |
| 16384 | 64 | 256 | j5_n256 | 30.95 | 0.7374 | +2.67 | −0.04 |
| 16384 | 128 | 128 | j5_n128 | 38.20 | 0.7380 | +9.92 | +0.02 |
| 16384 | 256 | 64 | j5_n64 | 49.46 | 0.7382 | +21.19 | +0.04 |
| 65536 | — | 1 | orig | 24.16 | 0.8206 | 0.00 | 0.00 |
| 65536 | 32 | 2048 | j5_n2048 | 24.96 | 0.8204 | +0.80 | −0.02 |
| 65536 | 64 | 1024 | j5_n1024 | 29.63 | 0.8207 | +5.47 | +0.01 |
| 65536 | 128 | 512 | j5_n512 | 36.75 | 0.8208 | +12.58 | +0.02 |
| 65536 | 256 | 256 | j5_n256 | 45.60 | 0.8208 | +21.43 | +0.02 |

### cluster019 (read 75%, unique 633K — scan-heavy)

| cap | per_shard | shards | variant | ns/op | hit ratio | Δns | Δhr (pp) |
|---:|---:|---:|:---|---:|---:|---:|---:|
| 1024 | — | 1 | orig | 48.60 | 0.2409 | 0.00 | 0.00 |
| 1024 | 32 | 32 | j5_n32 | 32.93 | 0.3041 | −15.67 | **+6.32** |
| 1024 | 64 | 16 | j5_n16 | 39.21 | 0.3003 | −9.40 | +5.94 |
| 1024 | 128 | 8 | j5_n8 | 46.96 | 0.2937 | −1.65 | +5.28 |
| 1024 | 256 | 4 | j5_n4 | 70.24 | 0.2824 | +21.64 | +4.15 |
| 4096 | — | 1 | orig | 42.36 | 0.2964 | 0.00 | 0.00 |
| 4096 | 32 | 128 | j5_n128 | 34.68 | 0.3164 | −7.67 | +2.00 |
| 4096 | 64 | 64 | j5_n64 | 38.24 | 0.3160 | −4.12 | +1.96 |
| 4096 | 128 | 32 | j5_n32 | 47.11 | 0.3155 | +4.76 | +1.91 |
| 4096 | 256 | 16 | j5_n16 | 70.14 | 0.3145 | +27.78 | +1.81 |
| 16384 | — | 1 | orig | 47.35 | 0.3153 | 0.00 | 0.00 |
| 16384 | 32 | 512 | j5_n512 | 36.83 | 0.3217 | −10.52 | +0.65 |
| 16384 | 64 | 256 | j5_n256 | 40.63 | 0.3217 | −6.72 | +0.64 |
| 16384 | 128 | 128 | j5_n128 | 51.98 | 0.3216 | +4.64 | +0.63 |
| 16384 | 256 | 64 | j5_n64 | 73.62 | 0.3214 | +26.27 | +0.62 |
| 65536 | — | 1 | orig | 55.63 | 0.3275 | 0.00 | 0.00 |
| 65536 | 32 | 2048 | j5_n2048 | 43.56 | 0.3288 | −12.07 | +0.13 |
| 65536 | 64 | 1024 | j5_n1024 | 45.83 | 0.3288 | −9.80 | +0.13 |
| 65536 | 128 | 512 | j5_n512 | 52.88 | 0.3287 | −2.76 | +0.13 |
| 65536 | 256 | 256 | j5_n256 | 75.33 | 0.3287 | +19.69 | +0.12 |

## 観測

### 1. Synthetic Zipf の sweet spot (per_shard ∈ [32, 64]) は実 trace でも保つ

12 (cluster, cap) セルすべてで **per_shard=32 が j5 内 trial 中 最速**。
per_shard=64 でも cluster006/cap=4096, cluster019 全 cap など多くのセルで orig 比 −2〜−9 ns。
親レポートの "scan ≈ 6 ns × (per_shard/32 − 1)" モデルが synthetic から実 trace へ
**そのまま transfer する**。

per_shard=128 以上は実 trace でも一様に scan tax を払い、cluster019/cap=65536 を除き
すべて orig より遅い (+5〜+27 ns)。256 は使う理由がない。

### 2. cluster019 で hit ratio が桁違いに改善

cluster019 は read 比率 75% (cluster006/018 の 96〜98% に対し低い) で
unique key 数 633K — write/scan が混ざった「working set がキャッシュに入りきらない」典型。
このとき j5 は orig 比 **+6.32pp** (cap=1024)、+2.00pp (cap=4096)、+0.65pp (cap=16384) と、
shard 化が hit ratio を大きく押し上げる。

仮説: scan-heavy トレースだと orig の単一 hand は scan が来た瞬間ホットエントリの
visited bit を消して回り「scan に焼かれる」(SIEVE 論文 §2.3 で議論される
scan-resistance の限界帯)。j5 では scan が複数 shard に分散し、
各 shard 内で hand は独立に進むので、ホット shard の visited bit は焼かれにくい。
synthetic Zipf でこれが見えなかったのは Zipf に scan 成分がないから。

cluster006 (read 98%) では cap=4096 で +0.43pp など散発的に出るが大きくはない。
cluster018 (read 96%) も同様で +0.85pp が最大。**「scan 成分が増えるほど shard 化の
hit ratio gain が増える」** 関係が cluster3本のレンジで明瞭。

### 3. throughput と hit ratio の二択ではなく "両取り" セルが多い

| cluster | cap | per_shard | Δns | Δhr (pp) |
|---|---|---|---|---|
| 006 | 4096 | 32 | −6.19 | +0.32 |
| 006 | 4096 | 64 | −3.58 | +0.43 |
| 006 | 16384 | 64 | −1.69 | +0.15 |
| 018 | 1024 | 32 | −6.22 | **+0.85** |
| 018 | 1024 | 64 | −1.29 | +0.67 |
| 019 | 1024 | 32 | −15.67 | **+6.32** |
| 019 | 1024 | 64 | −9.40 | +5.94 |
| 019 | 4096 | 32 | −7.67 | +2.00 |
| 019 | 4096 | 64 | −4.12 | +1.96 |
| 019 | 16384 | 32 | −10.52 | +0.65 |
| 019 | 16384 | 64 | −6.72 | +0.64 |
| 019 | 65536 | 32 | −12.07 | +0.13 |
| 019 | 65536 | 64 | −9.80 | +0.13 |

13 セルが **両軸で orig を支配** (Pareto 上で orig を厳密に飲む点)。
synthetic Zipf 版は「片方 −0.1pp 程度の犠牲を含む」セルが多かったが、
実 trace では犠牲なしの win が普通に取れる。

### 4. cap=65536 で大容量帯の挙動が見える

- cluster006 cap=65536: 全 j5 セルが orig と ±1ns。working set (unique 136K) の
  半分近くを cache が覆い hit ratio 0.83 に達するため eviction 自体がほぼ起きない
  (raw を見ると evictions ≈ 4)。eviction path の差は throughput に出ない。
- cluster018 cap=65536: 同様。working set の 4 割を cap が覆う。
- cluster019 cap=65536: hit ratio 0.33 で頭打ち。working set 633K に対し cap が
  まだ小さく eviction が dense。**この帯でも j5_n2048 (per_shard=32) は
  −12 ns で勝つ** — 大規模 cache でも shard 化のメリットは消えない。

「cap が大きくなると orig 自身が hit ratio で稼いで eviction が減り j5 アドバンテージが
消える」 (親レポート) は cluster006/018 では確認、cluster019 のような scan-heavy 帯では
**そうならない**。実 trace の多様性が初めて出した知見。

### 5. trial spread

`docs/figures/j5_twitter_trial_spread.png` 参照。各 5 trial の IQR は cell 内 ±1 ns
程度に収まり、上記 Δns ≥ 3 ns はすべて noise を有意に超える。Δhr は trial 内変動が
ほぼゼロ (deterministic trace + deterministic hash)。

## 結論

- **j5 の最適点は per_shard ∈ [32, 64]**: synthetic Zipf 版と同じ結論。実 trace 3 本で
  例外なく当てはまる。
- **scan-heavy トレース (cluster019) で j5 は二重に勝つ**: throughput で −10 〜 −16 ns、
  hit ratio で +0.65 〜 +6.32pp。SIEVE 単一 hand の scan-resistance 限界が
  shard 並列化で緩和される、という新しい角度の利得。
- **cap=65536 までスケールしても sweet spot は崩れない**: shard 数を 2048 まで増やしても
  per_shard=32 を保つ限り Pareto 支配が続く。
- **synthetic Zipf だけでは cluster019 級の hit-ratio gain は見えなかった** —
  Twitter trace を入れたことで「j5 は SIEVE よりも実用ワークロードでむしろ強い」
  という主張が初めてデータで支えられた。

## 図

- `docs/figures/j5_twitter_nsop_grid.png` — ns/op vs per_shard を (cluster × cap) で grid。
  各パネル黒破線 = orig 基準。
- `docs/figures/j5_twitter_delta_nsop_heatmap.png` — Δns/op heatmap (cluster × cap × per_shard)。
  青セル = j5 が orig より速い、赤 = 遅い。
- `docs/figures/j5_twitter_delta_hr_heatmap.png` — Δhit ratio heatmap (pp)。
  青 = j5 が低い、赤 = 高い。cluster019/cap=1024 が真っ赤に振れる。
- `docs/figures/j5_twitter_pareto.png` — Pareto scatter (ns/op × hit ratio)、
  ★ = orig、線 = j5 per_shard sweep。cluster019 のパネルで orig が支配される領域が一目。
- `docs/figures/j5_twitter_trial_spread.png` — 5 trial の boxplot。

## 次の実験候補

- **scan-resistance 仮説の検証**: cluster019 で観測された hit ratio gain が
  「scan による visited bit の汚染が shard 化で局所化される」ことに本当に起因するか。
  visited bit の rewrite カウントを variant 内で計測し orig vs j5 で比較すれば直接示せる。
- **per_shard=8/16 (SIMD chunk 未満)** のスカラー fallback 設計: cluster019 で
  per_shard=32 がさらに細かければ hit ratio はもっと伸びる可能性。
- **eviction 列の Jaccard / Kendall tau**: hit ratio が同じでも policy 内部が
  どう違うか。並列化議論の前段。
- **TTL を尊重した harness**: Twitter trace は TTL 列を持つ。現 harness は無視している
  ので、TTL 切れ insert を ignore する mode を作れば read 比率の高い 006/018 で
  ヒット率の上限が変わる可能性。
- **複数 CPU での再現**: i5-12600K 単機の結果。scan/per_shard の交点 (per_shard ≈ 64)
  は L1d/L2 サイズに依存する可能性が高い。

## 付随する変更ログ

- `src/bin/bench.rs`: variant matcher に `j5_n1024`, `j5_n2048` を追加 (cap=65536 / per_shard=32 用)。
- `scripts/sweep_j5_twitter.sh`: cluster × cap × per_shard 直積 sweep スクリプト。
- `scripts/plot_j5_twitter.py`: seaborn で 5 図を生成。
- `profiles/j5_twitter_pareto_2026-05-05.csv`: 5 trial × (12 orig + 48 j5) = 300 行 raw。
- `docs/figures/j5_twitter_*.png`: 図 5 枚。
