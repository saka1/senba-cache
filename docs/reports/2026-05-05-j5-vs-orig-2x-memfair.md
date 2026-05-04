# sieve_j5 vs sieve_orig — メモリ公平性 (orig×2 ハンデ) の検証

- 日付: 2026-05-05
- 親レポート: `2026-05-05-j5-twitter-pareto.md`、`2026-05-05-j5-pershard-pareto.md`
- 動機: これまでの j5 系ベンチは **論理 capacity を揃えて** orig と比較してきた。
  しかし j5 (= shard 化された j3) の内部レイアウト `sieve_j3.rs:72-78` は
  `order_cap = 2 * capacity` で **物理 slot 数を 2 倍** 確保している
  (tombstone を貯めて compaction 頻度を抑えるためのヘッドルーム)。
  link 8B/entry の orig より j5 の inline footprint は重い側になることがあり、
  「j5 が速いのは memory hand-out のおかげではないか」という公平性の疑念が残る。
  本レポートは **orig に 2x cap を渡した worst-case ハンデ** で j5 の優位が崩れるかを実測する。

## なぜ orig×2 が j5 にとって worst case か

j3 の `order_cap = 2 * capacity` は **dead slot (tombstone) 用ヘッドルーム** で、
live entry は最大 `capacity` 個まで。つまり j5 の slot は機能的に 2 種:

- **live**: ヒット可能な真の cache entry (最大 cap 個)
- **tombstone**: hand 走査時に skip される dead slot、compaction 待ち
  (最大 cap 個、ghost cache のような補助エントリに近い)

一方 orig×2 は `cap*2` 個の live を持つので、**ヒット率でも footprint でも j5 を上回る**。

| 指標 (u64,u64, cap=1000) | orig | j5 | orig_2x (cap=2000) |
|---|---|---|---|
| live 数 (上限) | 1000 | 1000 | 2000 |
| inline bytes/cap (実効) | ~25 | ~34 | ~50 |
| inline 物理 (KB) | 24.4 | 33.2 | 97.6 |

orig_2x は j5 より物理 ~3x 重く、live も 2x ある。**j5 が「memory advantage で勝っている」**
**仮説が真なら、orig_2x には負けるはず**。

## 設計

- harness: `benches/micro.rs` の `bench_mem_fair` (新規)。
  `bench_insert_only` と同じ Zipf trace (N=100k, len=1M, seed=42) を使う。
- 軸: skew ∈ {0.6, 0.8, 1.0, 1.2} × cap ∈ {100, 1000, 10000} ×
  variant ∈ {orig (cap), orig_2x (cap×2), j5 (cap)}。直積 = 36 cell。
- j5 は `DEFAULT_SHARDS = 8`。per_shard = cap/8 → {12.5, 125, 1250}。
- criterion 既定 (sample=20, warmup=500ms, measurement=3s)。CPU は他 j5 系列と同じ
  i5-12600K (P-core L1d=48 KB, L2=1.25 MB)。trace 長 1M op で除算して ns/op に換算。

## 結果

ns/op (criterion mean、trace=1_000_000 ops で除算)。太字は **orig_2x vs j5** の勝者。

| skew | cap | orig | **orig_2x** | **j5** |
|---:|---:|---:|---:|---:|
| 0.6 | 100   | 41.62 | 44.02 | **16.77** |
| 0.6 | 1000  | 40.78 | **37.89** | 29.97 |
| 0.6 | 10000 | 36.11 | **36.79** | 102.32 |
| 0.8 | 100   | 35.02 | 38.38 | **17.99** |
| 0.8 | 1000  | 32.99 | **32.30** | 30.99 |
| 0.8 | 10000 | 30.79 | **29.22** | 80.06 |
| 1.0 | 100   | 32.87 | 33.91 | **20.74** |
| 1.0 | 1000  | 25.24 | **23.56** | 26.34 |
| 1.0 | 10000 | 21.83 | **21.62** | 55.25 |
| 1.2 | 100   | 21.55 | 20.66 | **16.51** |
| 1.2 | 1000  | 16.61 | **16.83** | 17.70 |
| 1.2 | 10000 | 15.09 | **14.57** | 29.96 |

raw: `target/criterion/mem_fair_u64/**/new/estimates.json`。

### 3 つのレジーム

per_shard で整理すると境界が綺麗に出る:

1. **per_shard ≤ 12.5 (cap=100)**: **j5 圧勝**。orig_2x ハンデでも勝てない。
   j5 16-21 ns vs orig_2x 21-44 ns。AVX2 1 chunk (32B tags = 32 slot) で
   shard 全体を scan できる領域、SIMD 走査 + 並列 shard scan が link-list の
   pointer chase を 2-3x 圧倒。memory advantage は j5 に **無い** (= live は等量、
   orig_2x のほうがむしろ多い) のに勝つ → 速度優位は SIMD レイアウト由来と確定。

2. **per_shard ≈ 125 (cap=1000)**: **ほぼ tie**。
   skew ≥ 1.0 では j5 が 1-3 ns 微敗、skew ≤ 0.8 では orig_2x が 2-7 ns 勝つ。
   AVX2 で `2 * 125 = 250` slot を 8 chunk scan する帯。link-list と
   linear scan のコストがちょうど拮抗する遷移帯。

3. **per_shard = 1250 (cap=10000)**: **j5 完敗**。
   特に低 skew (scan-heavy) で j5 80-102 ns vs orig_2x 29-37 ns、約 3x 遅い。
   per_shard ≫ SIMD chunk になり、線形 scan が L1d (48 KB) を踏み外して
   `2 * 1250 * (1+17) = 45 KB/shard × 8 = 360 KB` で L2 領域に滑る。
   pointer chase の orig はワーキングセット局所性が出るので逆転。

## 結論

- **j5 の優位は memory hand-out 由来ではない**。orig に 2x cap (j5 より物理 3x 重い)
  を渡しても、small-cap 帯 (per_shard ≤ ~32) では j5 が依然 2-3x 速い。
  この regime では j5 のスピードは SIMD scan + shard 並列性が稼いでおり、
  「2x slack」は本質的なアドバンテージではない。
- 一方 large-cap (per_shard ≫ SIMD chunk) では、orig_2x ハンデなしの orig にすら j5 は負ける。
  これは memory ではなく **per_shard 線形 scan のコスト**が原因。j5 を large-cap で
  使うなら shard 数を増やす (= per_shard を 32-64 に保つ) のが必須、という親レポートの
  Pareto 結論を**メモリ公平条件下でも再確認**した形。
- 「j3 の 2x slack」は機能的には ghost cache / tombstone 用補助スロットであり、
  live capacity ではない。今後 footprint を議論するレポートは
  **inline bytes/cap = 34 (j5) vs 25 (orig)** の比較で記述するのが誠実。
  heap-backed K,V (String/Vec) ならこの差は %数で薄まる、というのは
  別途値サイズ可変の bench で検証する余地あり (本レポート射程外)。

## 残タスク

- 値サイズ可変 (`u64` → `[u8; 64]` 等) で同じ memfair bench を回し、
  「heap が支配する実用域では 2x slack が無視できる」を実測する。
- per_shard を固定 (例: 32) にして cap を伸ばすベンチ — j5 の large-cap 崩壊が
  per_shard 起因であることを単離する (親レポートに既出だが mem_fair 文脈で再録する価値あり)。
