# orig vs j8 vs moka 0.12 vs mini-moka — Twitter 5 cluster シングルスレッド再測定

**日付**: 2026-05-06
**対象**: 既存の `cluster006/018/019` に **`cluster016` (cat 3, Zipf 1.79, miss 7.28%)** と
**`cluster034` (cat 2, Zipf 1.14, miss 8.19%)** を追加した 5 cluster で、
`sieve_orig` / `sieve_j8` (per_shard=32 champion) / `moka 0.12` / `mini-moka 0.10` を
シングルスレッドで再測定。
**バイナリ**: `target/release/bench` (commit 85f1a7d 時点)。

## 1. 目的

j8-twitter-pareto (3 cluster) 以降の課題:

- 退行帯 (cluster018/cap≥4096) と利得帯 (cluster019) の **Zipf 軸での crossover** を地図化。
- **category 3** (cluster016) で SIEVE / W-TinyLFU の挙動が変わるか確認。
- **medium Zipf** (cluster034, α=1.14) が j8 利得 / 退行のどちらに振れるか。

## 2. 設定

- **トレース**: `cluster006` (Zipf 1.88, miss 2.7%, cat 2), `cluster016` (1.79, 7.3%, cat 3),
  `cluster018` (2.10, 0.2%, cat 1), `cluster019` (0.74, 45.5%, cat 2),
  `cluster034` (1.14, 8.2%, cat 2)
- **capacity**: 1024, 4096, 16384, 65536
- **per_shard**: 32 (j8 champion)
- **trials**: 5 (median 報告)
- **seed**: 42, **LEN**: 1 000 000
- スクリプト: `scripts/sweep_st_twitter_5cluster.sh`
- 生 CSV: `profiles/st_twitter_5cluster_2026-05-06.csv` (400 行 + header)

## 3. cluster 別サマリ — j8 vs orig

中央値 (5 trial):

| cluster | cap | orig ns | j8 ns | Δns | orig HR | j8 HR | Δhr (pp) |
|:---|---:|---:|---:|---:|---:|---:|---:|
| cluster006 | 1024  | 46.93 | 31.10 | **−15.83** | 13.22 | 13.13 | −0.08 |
| cluster006 | 4096  | 41.59 | 31.34 | **−10.25** | 34.86 | 35.18 | +0.32 |
| cluster006 | 16384 | 34.74 | 31.90 |  −2.84 | 64.03 | 63.84 | −0.19 |
| cluster006 | 65536 | 26.79 | 25.85 |  −0.94 | 82.93 | 82.74 | −0.19 |
| **cluster016** | 1024  | 43.04 | 32.25 | **−10.79** | 36.27 | 37.06 | **+0.79** |
| **cluster016** | 4096  | 36.54 | 32.48 |  −4.07 | 49.37 | 49.72 | +0.36 |
| **cluster016** | 16384 | 33.17 | 32.33 |  −0.84 | 67.52 | 67.52 | +0.00 |
| **cluster016** | 65536 | 27.45 | 32.34 |  +4.89 | 77.66 | 77.59 | −0.07 |
| cluster018 | 1024  | 36.14 | 30.67 |  −5.47 | 50.17 | 51.01 | +0.85 |
| cluster018 | 4096  | 29.67 | 30.19 |  +0.53 | 62.53 | 62.73 | +0.21 |
| cluster018 | 16384 | 28.59 | 29.09 |  +0.50 | 73.78 | 73.65 | −0.13 |
| cluster018 | 65536 | 22.98 | 26.66 |  +3.68 | 82.06 | 82.04 | −0.02 |
| cluster019 | 1024  | 49.21 | 32.51 | **−16.70** | 24.09 | 30.41 | **+6.32** |
| cluster019 | 4096  | 43.86 | 33.25 | **−10.61** | 29.64 | 31.64 | +2.00 |
| cluster019 | 16384 | 49.71 | 34.50 | **−15.21** | 31.53 | 32.17 | +0.65 |
| cluster019 | 65536 | 58.61 | 36.19 | **−22.42** | 32.75 | 32.88 | +0.13 |
| **cluster034** | 1024  | 44.99 | 30.06 | **−14.93** | 30.46 | 30.51 | +0.05 |
| **cluster034** | 4096  | 40.99 | 29.98 | **−11.01** | 35.52 | 35.50 | −0.02 |
| **cluster034** | 16384 | 44.12 | 32.55 | **−11.58** | 39.02 | 39.06 | +0.04 |
| **cluster034** | 65536 | 47.68 | 36.81 | **−10.87** | 41.12 | 41.16 | +0.04 |

新規 cluster の所見:

- **cluster034 (medium Zipf 1.14, miss 8%)** は **全 4 cap で j8 が −10〜−15 ns 圧勝**、
  HR は ±0.05 pp の同点。cluster019 (低 Zipf) と同じ「j8 完勝」パターンに分類される。
  これで「j8 利得帯」の境界は **Zipf ≲ 1.5** 付近にあることが示唆される
  (cluster006/018 の Zipf ≥ 1.88 が退行帯、cluster019/034 の ≤ 1.14 が利得帯)。
- **cluster016 (cat 3, Zipf 1.79, miss 7%)** は cap=1024/4096 で j8 利得、cap=16384 で同点、
  **cap=65536 で +4.89 ns 退行** — cluster018 (Zipf 2.10) と同じ pattern が cap シフト
  気味に出現。category 3 でも挙動は category 2/1 から推定通り (Zipf 軸で説明可能)。

## 4. j8 退行帯の整理

退行 (Δns > 0) が出るのは **Zipf ≥ 1.79 かつ cap ≥ 16384**:

| cluster | cap | Zipf | Δns | 解釈 |
|:---|---:|---:|---:|:---|
| cluster016 | 65536 | 1.79 | +4.89 | hit ratio 78%, false-match 増 |
| cluster018 | 4096  | 2.10 | +0.53 | (既知) |
| cluster018 | 16384 | 2.10 | +0.50 | (既知) |
| cluster018 | 65536 | 2.10 | +3.68 | (既知, hit ratio 82%) |

`j8-candidate-loop-analysis` の予測通り、**hit ratio が高く candidate ループに入る頻度が
高い帯域型** で退行。Zipf 軸では 1.79 から既に発現するため、退行域は
「cluster018 の特殊事情」ではなく **構造的な j8 の特性**。memory 20 B/cap の利得を
保ったまま運用するなら、Zipf ≥ 1.8 + cap ≥ 16384 では per_shard=16 を推奨
(`j8-c-hoist` 結論と整合)。

## 5. W-TinyLFU (moka 0.12 / mini-moka 0.10) との比較

### 5.1 hit ratio

| cluster | cap | orig HR | j8 HR | moka HR | mini_moka HR | Δhr (j8−moka) |
|:---|---:|---:|---:|---:|---:|---:|
| cluster016 | 1024  | 36.27 | 37.06 | 21.14 | 21.17 | **+15.92** |
| cluster016 | 16384 | 67.52 | 67.52 | 51.24 | 51.24 | **+16.28** |
| cluster019 | 1024  | 24.09 | 30.41 |  3.38 |  3.37 | **+27.03** |
| cluster019 | 65536 | 32.75 | 32.88 |  7.47 |  7.52 | **+25.41** |
| cluster034 | 1024  | 30.46 | 30.51 | 25.39 | 25.39 | +5.12 |
| cluster034 | 65536 | 41.12 | 41.16 | 35.50 | 35.51 | +5.65 |

**SIEVE (orig/j8) は 5 cluster × 4 cap = 20 全 cell で W-TinyLFU 以上の HR**。
新規 **cluster016** は **W-TinyLFU が −16 pp 崩壊** — `j8-vs-mini-moka-twitter` で示した
「Twitter 実 trace で W-TinyLFU が脆い」傾向が cat 3 でも再現。
**cluster034 (medium Zipf)** では崩壊幅が緩む (Δhr 5〜6 pp) — W-TinyLFU は
medium Zipf では competitive、scan-heavy (019) と high Zipf 帯域型 (016, 018) で崩れる、
という二極構造の中間。

### 5.2 throughput

moka 0.12 は **全 cell で 600〜1300 ns/op**、mini-moka 0.10 は **380〜530 ns/op**
(j8 比 12〜35 倍)。シングルスレッドでは concurrent primitive の overhead が支配的で
比較対象として妥当ではない (並列スケールは `c8-vs-moka-thread-sweep` 参照)。
HR 軸でのみ意味のある比較として §5.1 を採用。

## 6. 図

`docs/figures/` に 4 枚生成 (`scripts/plot_st_twitter_5cluster.py`):

- `st_twitter_5cluster_hr.png` — HR バー (cluster × cap × variant)
- `st_twitter_5cluster_nsop.png` — ns/op バー (log y)
- **`st_twitter_5cluster_pareto.png`** — Pareto 散布 4 family、cap 軸を線で結ぶ
- **`st_twitter_5cluster_pareto_sieve.png`** — orig vs j8 のみ (linear x, より細かく読める)

## 7. 結論

- **cluster034 (medium Zipf 1.14)** で j8 は全 cap −10〜−15 ns 圧勝、HR ±0.05 pp。
  「j8 利得帯」を **Zipf ≤ 1.5** に拡張できることを示唆。
- **cluster016 (cat 3, Zipf 1.79)** で cap=65536 のみ +4.89 ns 退行。退行は cluster018 固有
  ではなく **Zipf ≥ 1.79 + 高 hit ratio** の構造的特性。
- W-TinyLFU の HR 崩壊は cat 3 でも再現 (cluster016 で −16 pp)。SIEVE は **5 cluster ×
  4 cap = 20 全 cell で moka/mini-moka 以上の HR**。
- 5 cluster sweep が新たな測定基盤として整備された (`scripts/sweep_st_twitter_5cluster.sh`)。
  今後 j 系列の比較は cluster016/034 を含めた 5 cluster で行うのが推奨。
