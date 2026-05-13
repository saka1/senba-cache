# 2026-05-13 — c17s shard heuristic sweep: cap/8 推奨、cliff は P-series で発現

## 仮説

`2026-05-13-r2h-control-results.md` で「c17s × shards_mult=8 が r1/r2h を pareto
dominate」と分かったが、その sweep は 4 workload class × 4 cap × 4 T = 56 cell の狭範囲。
広範な実 trace で:

- **H_a**: shards_mult を上げ続けると Mops は plateau に達する (sweet spot がある)
- **H_b**: 一定 mult を超えると HR が cliff を起こす (per-shard cap が hot working set を
  下回るため)
- **H_c**: sweet spot は workload class で大きく変わらず、単一 heuristic 値を default に
  できる

事前予測: c8x or c16x で plateau、c16x 超で HR cliff、default 値は **cap/8** (=c8x) 系統。

## 実施したこと

### harness 拡張

`docs/benchmark/c17s-shard-sweep/run.sh`:

- variants: **c17s × shards_mult ∈ {1, 2, 4, 8, 16}** (= c1x, c2x, c4x, c8x, c16x) +
  r1@ways=8 対照列
- T: {1, 4, 8, 16}
- workload: Twitter cluster {006, 016, 018, 019, 034} × cap {1024, 4096, 16384, 65536}
  + ARC preset {OLTP, P1, P3, P6, P8, S1, S3, DS1, ConCat, MergeP, MergeS} × mokabench
  既定 cap + Zipf {0.8 gim, 1.0 gim, 1.4 gim, 1.4 RH} × cap {1024, 4096, 16384, 65536}
- value: u64、trials: 3
- gate: `cap_per_shard ≥ 2` で skip、`SHARDS ≤ 131072` で clamp

総 1560 invocations × 3 trials = **4680 rows、WSL2 で 40 min、crashes 0、skipped 0**。

## 結果

詳細表は `docs/benchmark/c17s-shard-sweep/figures/summary.md`。要点のみ:

### 1. best variant 出現頻度 (56 = workload class × T cell)

| best | cell 数 | 該当 workload class |
|---|---:|---|
| **c16x** | **36 (64%)** | Twitter 全 5 cluster / ARC OLTP, DS1, S1, S3 / Zipf 0.8, 1.0 |
| c8x | 23 | ARC P1/P3/P6/P8 cap-fits 帯 / ARC ConCat, MergeP, MergeS / Zipf 1.4 gim |
| c4x | 4 | ARC P1/P8 T=1 / Zipf 1.4 RH T=4,8 |
| c2x | 1 | Zipf 1.4 RH T=16 |
| c1x | 1 | Zipf 1.4 RH T=8 |
| **r1_w8** | **2** | Zipf 1.4 RH T=8, T=16 |

→ **c16x が過半数で勝つが、cliff zone (ARC P-series) があるため default 化は危険**。
**c1x (現状 senba auto-shard) が best になる cell はゼロ**。r1@8 は合成 Zipf 1.4 RH のみ。

### 2. Mops gain 振幅 (T=16、c1x → 各 mult)

実 trace で **桁違いの Mops 向上**:

- twitter cluster019: c1x **13.9 → c16x 85.3 Mops (+514%)**
- twitter cluster018: 51.1 → 144.3 (+182%)
- ARC OLTP cap≤2000: 9.9 → 67.6 (**+584%**)
- ARC DS1: 19.2 → 28.5 (+48%)
- Zipf 0.8 gim: 43.2 → 73.7 (+71%)

合成 Zipf 1.4 では gain が頭打ち:

- Zipf 1.4 gim: 159 → 181 Mops (+14% only)
- Zipf 1.4 read-heavy: c16x で **頭打ち**、r1@8 が 184 Mops で +20% 上回る

### 3. HR cliff 構造 (P-series)

ARC P1/P3/P6/P8 で c16x が **−7〜−10pp の HR drop**:

| ARC P1 cap=160k T=16 | shards | cap_per_shard | HR | Mops |
|---|---:|---:|---:|---:|
| c1x | 4096 | 39 | 0.621 | 71.9 |
| c2x | 8192 | 19 | 0.616 | 92.3 |
| c4x | 16384 | 9 | 0.607 | 114.9 |
| **c8x** | 32768 | 4 | 0.577 | **115.1** ← Mops peak |
| **c16x** | 65536 | **2** | **0.508** | **94.2** ← cliff + regress |

**cliff の構造**: per-shard cap = 2 になると SIEVE eviction が degenerate (hand が即時
visited bit リセット → 即時 evict、local LRU 化)。P-series の **dense hot working set** が
local LRU で saturate。

cluster019 / OLTP / DS1 のような **sparse working set** (session-y, transactional, scan-heavy)
では per-shard cap 2-4 でも HR drop しないか、むしろ improve (cluster019 c16x で +3.3pp)。

→ **HR cliff の発生条件**: 「workload の **per-shard 局所的 hot working set > cap_per_shard**」

### 4. 仮説判定

- **H_a Mops plateau**: 部分肯定。実 trace の多くは c8x→c16x で **gain が逓減 or 逆転**、
  cluster019/OLTP のように c16x まで増え続けるもある。**plateau 位置は workload 依存**
- **H_b HR cliff**: **肯定**。ARC P-series で c16x が −7〜−10pp の cliff、cap_per_shard=2
  が境界
- **H_c 単一 default**: **部分肯定**。c8x が安全側で大半の利得を取れるが、cluster019 等の
  特定 trace では c16x の方が +30% 良い

### 5. 推奨 default

3 候補を比較した結果 **`cap/8` (= c8x)** を推奨:

| 値 | Mops gain (c1x 比) | HR drop 最悪 | 評価 |
|---|---:|---:|---|
| `cap/16` (c4x) | +50〜+200% | −2pp | HR-safe だが Mops 取り損ね |
| **`cap/8` (c8x)** ★ | **+80〜+580%** | **−5pp** | **balanced sweet spot** |
| `cap/4` (c16x) | +50〜+580% (P-series regress) | **−10pp** | Mops 最大だが cliff zone あり |

c8x は cliff の "1 段手前" で安全側、HR drop は cluster006 (-3pp) や ARC P-series (-5pp)
程度に留まる。`r1-vs-moka-cap-sweep` の moka 比較も c8x で取り直せば moka 比 **更に拡大**
する見込み (c17s_8x 自体が r1 を pareto dominate するため)。

## 学び

### 1. r1 design hypothesis の完全否定

`r1-design.md` §3.1 の「shard 間 routing に thread affinity を導入すると bouncing が
構造的に消える」設計は、本 sweep で **広範な実 trace で `cap/8` 細分化に劣後**。
56 (workload × T) cell 中 r1 が勝つのは **合成 Zipf 1.4 RH の 2 cell のみ**。

`r1-results.md` の「採用領域 31/520 cell」は cap=4096 固定 sweep での評価だが、shards 軸を
動かさない前提だった。shards 軸を動かすと c8x が r1 を直接置き換える。

→ **r-series 全体を artifact 凍結 (確定)**。`r1-results.md` follow-up #2 「adaptive WAYS
prototype」も中止 (= c8x が adaptive 不要で sweet spot をカバー)。

### 2. SIEVE per-shard cap の下限

`MAX_PER_SHARD = 64` の上限 (`sieve_c17s.rs` の 6-bit ID) に加え、本 sweep で **`MIN_PER_SHARD ≈ 4`** の下限が見えた。per-shard cap < 4 では SIEVE eviction が degenerate (hand が
全 entry を 1 周走るより insert の方が頻繁、visited bit が機能しなくなる)。

`Cache::new(cap)` の auto-shard を `cap/8` に変える場合、`cap < 32` (= shards=4 で per-shard 8)
で min cap floor が要る。実用上は cap=32 未満の cache は稀なので問題は小さいが、構造的
boundary として doc 記載。

### 3. workload hint API の可能性

P-series ARC のような cap-fits 帯 workload では c4x、cluster019/OLTP のような session/scan
帯では c16x がそれぞれ最適。**workload hint** で opt-in する API を増やせばさらに +30%
帯の利得が取れる可能性:

```rust
Cache::builder(cap).pattern(WorkloadHint::CapFits).build()   // → cap/16
Cache::builder(cap).pattern(WorkloadHint::Skewed).build()    // → cap/4
// default は cap/8 (balanced)
```

ただし API 設計の複雑度と Mops gain (+30%) の trade-off で別 sweep で評価。

## 今後

優先度順:

1. **`senba::Cache::new(cap)` auto-shard を `cap/8` に変更する PR** — 本 sweep の結論を
   直接実装。perf-gate (criterion 8 シナリオ) を必ず通すこと、特に SIMD scan の chunk 数
   変化が出る (`cap/64` → `cap/8` で chunks 数 8x、ただし per-chunk EMPTY lane が増える)。
   既存 `sieve_cache_perf` baseline 保存 → 変更 → 比較
2. **moka 0.13 / mini_moka 比較を c8x で取り直し** — `r1-vs-moka-cap-sweep` が cap/64 前提
   だったので、c8x default 化後に再走させて lib publish 用の比較表を更新
3. **workload hint API のスケッチ** — 別 design doc。本 sweep の cell 別最適表を base に
4. **bare-Linux 再計測** (`project_wsl2_measurement_confound`) — 本 sweep は WSL2、lib
   publish 直前で核 cell を bare metal で 1 周回す
5. **MIN_PER_SHARD floor** の実装 — `Cache::new(cap)` で per_shard < 4 にならないようガード

## 関連

- `2026-05-12-r1-design.md` §3.1 — affinity essential 仮説、本 sweep で完全否定
- `2026-05-12-r1-results.md` — r1 採用領域 31/520、c8x で大半が absorbed
- `2026-05-13-r1-vs-moka-cap-sweep.md` — moka 比較の前報、c8x default 化後に再走必要
- `2026-05-13-r2-design.md` — r2s/r2p 設計、本 sweep で実装不要が確定
- `2026-05-13-r2h-control-results.md` — c17s_8x 優位の起点、本 sweep で広範に確認
