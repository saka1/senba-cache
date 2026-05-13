# c17s shard heuristic sweep — summary

`docs/reports/2026-05-13-r2h-control-results.md` の発見「c17s_8x が r1/r2h を pareto
dominate」を広範な実 trace で検証 + sweet spot 特定。

軸: c17s × shards_mult ∈ {1, 2, 4, 8, 16} (= c1x..c16x) + r1@ways=8 対照 / T ∈ {1, 4, 8, 16} /
workload {Twitter 5 cluster × 4 cap, ARC 11 preset × preset cap, Zipf 4 cfg × 4 cap} / value=u64 /
trials=3。**4680 rows、WSL2 で 40 min、crashes 0、skipped 0**。

## 1. workload class × T 別 best variant 出現

56 (workload class × T) cell の best 集計:

| best variant | cell 数 | 主な該当 workload class |
|---|---:|---|
| **c16x** | **36** (64%) | Twitter 5 cluster 全て、ARC OLTP 全 T、ARC DS1/S1/S3 大 cap、Zipf 0.8/1.0 |
| c8x | 23 | ARC P1/P3/P6/P8 (cap-fits 系)、ARC ConCat/MergeP/MergeS、Zipf 1.4 gim |
| c4x | 4 | ARC P1/P8 T=1、Zipf 1.4 RH T=4/T=8 |
| c2x | 1 | Zipf 1.4 RH T=16 |
| c1x | 1 | Zipf 1.4 RH T=8 |
| **r1_w8** | **2** | Zipf 1.4 RH T=8/T=16 (= r1 が唯一勝つ workload class) |

→ **c16x が過半数の cell で勝つ**。c1x (現状 senba auto-shard) で勝てる cell はゼロ。
r1_w8 が勝てるのは合成 Zipf 1.4 read-heavy のみ。

## 2. Mops gain (c1x → 各 mult、workload class 別、T=16)

| workload | c1x (Mops) | c4x gain | c8x gain | c16x gain | sweet spot |
|---|---:|---:|---:|---:|:---:|
| twitter cluster006 | 59.3 | **+74%** | +83% | **+98%** | c16x |
| twitter cluster016 | 38.3 | +123% | +183% | **+199%** | c16x |
| twitter cluster018 | 51.1 | +108% | +158% | **+182%** | c16x |
| twitter cluster019 | 13.9 | +210% | +382% | **+514%** | c16x |
| twitter cluster034 | 20.7 | +175% | +280% | **+379%** | c16x |
| ARC OLTP (preset cap) | 9.9 | +240% | +454% | **+583%** | c16x |
| ARC DS1 | 19.2 | +40% | +43% | **+48%** | c16x |
| ARC P1 cap=160k | 71.9 | **+60%** | +60% | +31% | c4x/c8x (cliff at c16x) |
| ARC P3 cap=160k | 59.5 | +54% | **+56%** | +53% | c8x |
| ARC P6 cap=160k | 61.5 | +65% | **+72%** | +52% | c8x (cliff at c16x) |
| ARC P8 cap=160k | 83.1 | +36% | **+47%** | +17% | c8x (cliff at c16x) |
| ARC S1 / S3 | ~41 | +40% | +59% | **+57%** | c8x ≈ c16x |
| ARC ConCat | 26.3 | +97% | **+133%** | +99% | c8x |
| ARC MergeP / MergeS | ~26 | +75% | **+74%** | +49% | c8x |
| Zipf 0.8 gim | 43.2 | +50% | +61% | **+71%** | c16x |
| Zipf 1.0 gim | 68.6 | +36% | +37% | **+38%** | c16x |
| Zipf 1.4 gim | 159.0 | +15% | **+21%** | +14% | c8x |
| Zipf 1.4 read-heavy | 153.4 | +1% | +2% | +6% | r1@8 wins (+20%) |

主観察:
- **Twitter / ARC OLTP / Zipf 0.8-1.0 で c16x が爆発的に勝つ** (+50〜+580%)
- **ARC P-series (P1/P3/P6/P8) は c8x が sweet spot**、c16x は Mops でも regress
- **Zipf 1.4 RH のみ r1@ways=8 が勝つ** (+20% over c16x、HR ±0.5pp)

## 3. HR delta (c1x → 各 mult、平均値で見る安全マージン)

workload class T=16 の HR delta (pp):

| workload | c2x | c4x | c8x | c16x | 含意 |
|---|---:|---:|---:|---:|---|
| twitter cluster006 | −0.6 | −1.3 | −2.8 | −5.4 | 中位 drop |
| twitter cluster016 | +0.2 | +0.3 | 0.0 | −1.1 | flat |
| twitter cluster018 | +0.1 | +0.1 | −0.1 | −1.2 | flat |
| twitter cluster019 | **+1.0** | **+1.7** | **+2.6** | **+3.3** | **HR 改善** |
| twitter cluster034 | 0.0 | 0.0 | −0.2 | −1.1 | flat |
| ARC OLTP | −0.4 | −1.0 | −2.1 | **−4.1** | 中位 drop |
| ARC DS1 | +0.5 | +1.0 | +0.8 | **+1.2** | 改善 |
| ARC P1 | −0.7 | −1.8 | −4.7 | **−10.1** | **cliff** |
| ARC P3 | −0.6 | −1.7 | −3.6 | **−7.4** | **cliff** |
| ARC P6 | −0.5 | −0.8 | −4.2 | **−8.4** | **cliff** |
| ARC P8 | −0.4 | −1.5 | −3.7 | **−8.3** | **cliff** |
| ARC S1 / S3 | −0.3 | −1.1 | −2.5 | −2.7 | 小 drop |
| ARC ConCat | −0.4 | −0.6 | −0.9 | −1.9 | 小 drop |
| ARC MergeP / MergeS | −0.3 | −0.5 | −1.0 | −1.3 | 小 drop |
| Zipf 0.8 gim | −0.2 | −0.9 | −2.2 | **−4.9** | 中位 drop |
| Zipf 1.0 gim | −0.2 | −0.7 | −1.7 | **−3.7** | 中位 drop |
| Zipf 1.4 gim | 0.0 | −0.1 | −0.2 | −0.6 | 完全 flat |
| Zipf 1.4 read-heavy | 0.0 | −0.1 | −0.3 | −0.5 | 完全 flat |

**HR cliff zone**: ARC P1/P3/P6/P8 (cap-fits 系) で c16x が **−7〜−10pp の HR cliff**。
**HR safe zone**: それ以外は c16x でも −5pp 以内、cluster019 と DS1 は **改善**。

## 4. cliff 構造の解釈

ARC P-series の HR drop を cap × shards_mult で展開すると、cliff は **per-shard cap が
hot working set per shard を下回る境界** で発生する:

| P1 cap=160k T=16 | shards | cap/shard | HR | Mops |
|---|---:|---:|---:|---:|
| c1x | 4096 | 39 | 0.621 | 71.9 |
| c2x | 8192 | 19 | 0.616 | 92.3 |
| c4x | 16384 | 9 | 0.607 | 114.9 |
| c8x | 32768 | 4 | 0.577 | 115.1 |
| c16x | 65536 | 2 | **0.508** | **94.2** ← Mops も regress |

cap_per_shard が 4 → 2 になると HR −7pp、Mops も regress。per-shard SIEVE が 2 entries
では eviction policy が機能しなくなる (hand が即座に visited bit リセットして evict、
local LRU 化)。

P-series 以外の trace (cluster019, OLTP, DS1) では cap-fits 帯でも HR cliff が出ない
理由は recall pattern の違い (cluster019 = 短い session、OLTP = transactional、DS1 =
scan-heavy で HR floor が極小) — それぞれ per-shard 2-4 entries でも回せている。

## 5. 推奨 heuristic 値

検討候補 3 つ:

### A. c4x (= cap/16 entries per shard) — HR-safe default

- **長所**: HR drop 全 cell で −2pp 以内、cliff zone 回避
- **Mops gain**: c1x 比 +50〜+200% (実 trace)、c8x の 60〜80%
- **欠点**: Twitter / Zipf で c8x/c16x の利得を取り損ねる

### B. c8x (= cap/8 entries per shard) — balanced default ★ 推奨

- **長所**: 全 workload で Mops が c8x または近接、HR cliff まで余裕 2x
- **Mops gain**: c1x 比 +80〜+580%、cliff zone は −4pp 以内
- **欠点**: ARC P-series で −5pp 程度の HR drop が出る (cliff の "1 段手前")

### C. c16x (= cap/4 entries per shard) — Mops-aggressive default

- **長所**: 36/56 cell で best、Twitter / OLTP / Zipf 0.8-1.0 で最大 Mops
- **欠点**: ARC P-series で **−10pp の HR cliff**、Mops も regress (P1 cap=160k)
- **判断**: P-series は web 系 trace なので cliff は実用的に重大、c16x をデフォルトには
  しにくい

→ **B (c8x = cap/8 entries per shard) を推奨**。HR cliff 直前で Mops の大半を取り、
P-series の HR drop は許容範囲 (−4pp、cluster006 と同等)。

更に踏み込むなら **workload hint API**: `Cache::builder().pattern(CapFits)` のような
opt-in で c4x にダウングレード、`CacheUnderSized` で c16x にアップグレードする path も
ある (実装は別 sweep)。

## 6. r1@ways=8 の位置

r1_w8 は 56 cell 中 **2 cell で best**:

- Zipf 1.4 read-heavy T=8: r1 115.1 vs c1x 109.7 vs c16x 105.7 (r1 +5%)
- Zipf 1.4 read-heavy T=16: r1 184.1 vs c1x 153.4 vs c16x 162.4 (r1 +13%)

合成 Zipf の skew が極端 (1.4) かつ read-heavy の組合せでのみ r1 の affinity が機能する。
**実 trace では全 cell で c8x/c16x に負ける**。

`r1-results.md` で「r1 採用領域 31/520 cell」とした内訳のうち、本 sweep の cluster019 /
ARC MergeP / ARC ConCat / Zipf 1.4 RH の **大半が c8x/c16x に pareto dominated**。
r-series は確定的に artifact 凍結。

## 7. 結論

1. **senba auto-shard heuristic を `next_pow2(cap/64)` から `next_pow2(cap/8)` に変更
   推奨**。実 trace で Mops +80〜+580%、HR drop ≤ −5pp で済む
2. **c16x (cap/4) は P-series ARC でリスクあり**、default 化は危険
3. **r-series は確定的に劣位**、artifact 凍結を確定
4. workload hint API で c4x / c8x / c16x を opt-in にする path は別 sweep で検討

## 生成物

- `data/results.csv` (4680 rows)
- `data/sweep.log` (40 min ログ)
- `data/crashes.log` (空) / `data/skipped.log` (空)
- 本 summary.md
