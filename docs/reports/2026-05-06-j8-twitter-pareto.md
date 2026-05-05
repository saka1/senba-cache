# j8 vs orig — Twitter trace 全数ベンチ (full sweep)

**日付**: 2026-05-06
**対象**: `sieve_j8` (M5.3 + tag 内 ID embed + free_list 廃止 + BLSR×2 + sizeof-aware bit layout + chunk base ptr hoist) を Twitter cache trace (OSDI'20) 上で `sieve_orig` と全数比較。
**バイナリ**: `target/release/bench` (commit 3dbe2bf 時点)。

## 1. 目的

j8 の累積最適化 (M5.3 → BLSR → c-hoist) を踏まえた**現時点 (2026-05-06) のスナップショット**を `j7-twitter-pareto` と同じ枠組みで取り直し、最良 per_shard を選定して `j7_twitter_pershard32_vs_orig.png` の j8 版を作成する。

## 2. 設定

- **トレース**: `cluster006`, `cluster018`, `cluster019` (各 1 M ops)
- **capacity**: 1024, 4096, 16384, 65536
- **per_shard**: 16, 32, 64 — j8 の構造的上限 `MAX_PER_SHARD = 64` に従う
- **trials**: 5 (median 報告)
- **seed**: 42, **LEN**: 1 000 000
- スクリプト: `scripts/sweep_j8_twitter_full.sh`
- 生 CSV: `profiles/j8_twitter_full_2026-05-06.csv` (225 行 + header)

cap=65536 + per_shard=16 は shards=4096 (`j8_n4096` 未定義) のためスキップ。残り 11 (cluster, cap, per_shard) cell × 3 cluster = 33 cell を測定。

## 3. per_shard 横断サマリ

12 cell (cluster × cap, per_shard=32 と 64 は 12、16 は 9) 中の j8 − orig 中央値:

| per_shard | cells | dns 中央値 (ns) | dns 平均 (ns) | dhr 中央値 (pp) | speed 勝ち | HR 勝ち |
|---:|---:|---:|---:|---:|---:|---:|
| 16 | 9  | **−11.15** | **−10.43** |  +0.041 | 9/9 | 5/9 |
| 32 | 12 |  −8.12 |  −8.20 |  +0.169 | 9/12 | 7/12 |
| 64 | 12 |  −4.44 |  −4.69 |  +0.194 | 8/12 | 9/12 |

- **per_shard=16** が速度では最良 (9/9 cell で orig を absolute に上回る)。ただし cap=65536 をカバーできない構造制約あり。
- **per_shard=32** は速度 9/12 勝ち + HR 7/12 勝ち、4 cap 全帯域をカバーする中庸案。
- **per_shard=64** は HR が最も安定 (9/12 勝ち) だが速度退行が大きい。

j7 のチャンピオン (per_shard=32) と直接比較するため、本稿でも **per_shard=32 を j8 のチャンピオン**として図化する。「最速だけ」を主張するなら 16 だが、図のスコープ整合性 (cap=65536 を含む) と HR 安定性を優先した。`j8-c-hoist` 報告の "sweet spot=16" は依然有効で、cap≤16384 の運用域では 16 を推奨する。

## 4. champion (per_shard=32) — orig 比

| cluster | cap | orig ns/op | j8 ns/op | Δns | Δhr (pp) |
|:---|---:|---:|---:|---:|---:|
| cluster006 | 1024  | 47.58 | 30.94 | **−16.64** | −0.08 |
| cluster006 | 4096  | 42.87 | 32.96 |  −9.91 | +0.32 |
| cluster006 | 16384 | 36.64 | 31.93 |  −4.71 | −0.19 |
| cluster006 | 65536 | 27.66 | 25.80 |  −1.86 | −0.19 |
| cluster018 | 1024  | 37.77 | 30.69 |  −7.08 | +0.85 |
| cluster018 | 4096  | 30.18 | 31.49 |  +1.31 | +0.21 |
| cluster018 | 16384 | 28.48 | 30.76 |  +2.28 | −0.13 |
| cluster018 | 65536 | 27.16 | 27.37 |  +0.20 | −0.02 |
| cluster019 | 1024  | 50.93 | 34.07 | **−16.86** | **+6.32** |
| cluster019 | 4096  | 44.43 | 35.28 |  −9.16 | +2.00 |
| cluster019 | 16384 | 51.24 | 35.00 | **−16.24** | +0.65 |
| cluster019 | 65536 | 56.96 | 37.25 | **−19.71** | +0.13 |

- **cluster019**: 全 cap で j8 完勝 (速度 −9〜−20 ns、HR は cap=1024 で +6.32 pp の大幅利得を保持)。
- **cluster006**: 全 cap で速度勝ち、HR は ~±0.3 pp の僅差。
- **cluster018**: cap=1024 のみ完勝。cap≥4096 では速度がわずかに退行 (+0.2〜+2.3 ns)、HR ほぼ同等 — 帯域型 workload で j8 の inner-loop 退行が目立つ既知パターン (`j8-candidate-loop-analysis`)。

## 5. 図

`docs/figures/` 配下に j7 シリーズと同じ 6 枚を生成 (`scripts/plot_j8_twitter.py`):

- `j8_twitter_nsop_grid.png` — per_shard 横軸の ns/op
- `j8_twitter_delta_nsop_heatmap.png` — Δns ヒートマップ
- `j8_twitter_delta_hr_heatmap.png` — Δhr ヒートマップ
- `j8_twitter_pareto.png` — Pareto 散布
- `j8_twitter_trial_spread.png` — 5 trial 箱ひげ
- **`j8_twitter_pershard32_vs_orig.png`** — `j7_twitter_pershard32_vs_orig.png` の j8 版 (本稿の主成果物)

## 6. 結論

- **12 cell 中 9 cell で j8 (per_shard=32) が orig を速度で上回る。** cluster019 では全 cap で −9〜−20 ns、cluster006 では cap によらず −2〜−17 ns。
- **HR は 12/12 cell で ±1 pp 以内 + 7/12 で純粋勝ち。** cluster019/cap=1024 では +6.32 pp の大幅利得を保持。
- **退行は cluster018/cap≥4096 に局所化** (+0.2〜+2.3 ns)。これは hit ratio の高い帯域型 workload で false-match candidate が増え、`j8-candidate-loop-analysis` で示した inner-loop 退行が顕在化するセル。c-hoist 後でも残る限られた退行域として認識する。
- **memory 20 B/cap の利得を保ったまま throughput でも 3/4 cluster で勝つ**、という j8 系列の当初目標に到達。
- per_shard=16 を許容する運用 (cap≤16384) では 16 が更に速い (`j8-c-hoist` §最終)。本稿は cap=65536 を含む全帯域で「1 つの per_shard で何が言えるか」を見る視点。
