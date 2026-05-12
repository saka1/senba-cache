# `senba::concurrent::PartitionedCache` cap=1024 sweep — VTune 予測の end-to-end 反証と採否確定

## 仮説

`2026-05-13-partitioned-vtune.md` で「L3 latency 主犯、cap=1024 で per-partition working set を L1d (48 KiB) fit に縮めると T=16 N=16 で partitioned が 80.4 → **157.5 Mops** (+96%)」を確認。VTune 単体 (Windows native i5-12600K, bench_vtune_concurrent の synthetic loop) では partitioned が c17s baseline と並ぶか上回るカーブが見えていた。

これを **bench_concurrent の full (T × N × workload) sweep**で再現できるか確認し、partitioned の採否を確定させる。`2026-05-12-partitioned-results.md` (cap=4096) の比較対象を CAP のみ差し替えて同条件で取り直す:

- 設計 gate (Zipf 1.4 read-heavy T=16 N=16) で partitioned が c17s と並ぶ → **accept**
- 並ばない → cap-tune が VTune synthetic では効くが end-to-end では効かない構造的理由がある → **partitioned reject 確定**

## 実施したこと

`docs/benchmark/partitioned-cap1024-sweep/run.sh` (= `partitioned-sweep/run.sh` の CAP=1024 ARC_CAP=1000 派生) を WSL2 Linux (i5-12600K, kernel 6.6.87.2 microsoft-standard) で実行。stages/T/N/workload 軸は cap=4096 sweep と全同一:

- `T_LIST="1 2 4 8 16"`, `N_LIST="1 2 4 8 16"`, `WAYS_LIST="1 4 16"`
- workload: zipf {0.8,1.0,1.4}×{gim} + zipf 1.4 read-heavy + twitter {006,019,034} + arc {OLTP,DS1}
- TRIALS=3, OPS=2M, WARMUP=200k, seed=42, --shards=64

総 1216 行 (header 込み), 0 crashes。

Plot は `partitioned-sweep` から減らし、`fig_pareto_overlay_all.png` + `summary.md` のみ生成 (per-workload heatmap / scalability 図は廃止 — 同 sweep 反復で「個別図は冗長、overlay 1 枚 + 表で要約のほうが実用」と判明したため)。

## 結果

### 採用領域: partitioned は **悪化** した

| | cap=4096 | cap=1024 | Δ |
|---|---|---|---|
| partitioned accept rate | **19.6%** (44/225) | **7.6%** (17/225) | **−12.0pp** |
| r1 accept rate | 5.9% (8/135) | 7.4% (10/135) | +1.5pp |

accept zone は VTune 報告書と同じ `HR drop ≤ 5pp AND Mops gain ≥ +20% vs c17s`。**仮説と逆向き** — cap=1024 で partitioned の優位は縮んだ。

### 設計 gate (Zipf 1.4 read-heavy) の生数値

partitioned aggregate Mops (cap=1024):

| N \ T | 1 | 2 | 4 | 8 | 16 |
|---|---|---|---|---|---|
| 1 | 23.95 | 13.74 | 8.66 | 4.67 | 2.59 |
| 2 | 22.93 | 14.70 | 14.48 | 10.72 | 6.60 |
| 4 | 19.79 | 15.40 | 29.06 | 21.70 | 13.80 |
| 8 | 21.50 | 20.25 | 26.99 | 42.16 | 34.92 |
| 16 | 18.99 | 19.99 | 25.80 | 41.03 | **73.04** |

c17s baseline (cap=1024): T=1 24.9 / T=16 **155.2** Mops。

**Zipf 1.4 RH T=16 N=16: partitioned 73 vs c17s 155 = −53%**。cap=4096 (partitioned 75 vs c17s 133 = −43%) より差が広がった。

| | partitioned T=16 N=16 | c17s T=16 | gap |
|---|---|---|---|
| cap=4096 | 75.4 | 133.1 | **−43%** |
| cap=1024 | **73.0** | **155.2** | **−53%** |
| VTune 予測 (Win native, synthetic) | **157.5** | — | — |

VTune が観測した partitioned 157.5 Mops は **WSL2 + 実 workload では再現せず**。partitioned 自体は cap 縮小で +0% (75→73、ノイズ域)、c17s は +17% (133→155) と c17s 側だけが cap-tune の利益を取った。

### 採用領域の質的構造

partitioned accept cell (17 個) の分布:

- **arc_DS1 が支配的** (8/17): T=1 で全 N、T=8 で N=8,16、T=16 で N=8,16。base が低い workload (c17s 14–20 Mops) なので relative gain が出やすい
- **twitter T=1 のみ** (cluster006/019/034 計 6 cell): T≥2 では accept zone 外
- **arc_OLTP T=1 N=1** (1 cell): HR drop 0.29pp で辛うじて合格
- **Zipf accept = ゼロ**

cap=4096 sweep では Zipf 0.8 gim T=16 N=16 等が +20–55% gain で accept、Twitter cluster019 T=16 N=16 が +390% gain だった (HR-tolerant 帯の partitioned 圧勝)。**cap=1024 では Twitter cluster019 T=16 N=16 が +88.8% に縮み、HR drop が 2.6pp → 10.5pp に悪化** → HR drop ≤5pp 基準を外れる:

| | cluster019 T=16 N=16 Mops gain | HR drop |
|---|---|---|
| cap=4096 | **+390%** | 2.6pp |
| cap=1024 | +88.8% | 10.5pp (reject) |

ARC DS1 だけは HR drop ≈ 0 のまま (元々 working set が小さく、cap=1000 でも HR が 0 のまま) で +118% gain、cap 縮小に robust。

### r1 は real trace 帯で逆転

r1 (sharded, ways=16) は cap=1024 でも twitter_cluster019 T=16 で +183% (HR drop 4.8pp、ギリギリ合格)、arc_DS1 T=16 で +93%。**cap=1024 で r1 が partitioned を抜く workload が出てきた** — r1 は HR が partitioned ほど崩れず、scaling は SHARDS=64 のグローバル shard で稼ぐので、cap 縮小に対する HR penalty が小さい。

## 学び

### VTune 予測は end-to-end では再現しない

VTune Windows native の bench_vtune_concurrent (synthetic ループ) は cap=1024 で T=16 partitioned 157.5 Mops を出したが、WSL2 + bench_concurrent では 73 Mops。差分要因の候補:

1. **WSL2 confound** (memory note `wsl2-measurement-confound`): 過去のベンチで Windows native / WSL2 で cap-fit 帯の数値が乖離する例が記録済み。今回も同種のバイアス可能性大
2. **synthetic loop vs full workload**: bench_vtune_concurrent は Zipf 列を事前生成して固定 op-mix の単純ループ。bench_concurrent は warmup / trial 管理 / hit-ratio 計上 等の追加 work が thread あたりに乗る
3. **c17s が同じ cap-tune を取ってしまう**: VTune 報告書では partitioned 単独の数値しか比較せず、「c17s も cap=1024 でどう変わるか」が未測定だった。実際は c17s も +17% 取って差が開いた

差分の正体は (1) と (3) の合算と推定。(2) の影響は **partitioned/c17s に対称的にかかる**はずなので相対差を逆転させる説明にはなりにくい。

### cap-tune は library 設計の決め手にならない

VTune 単独では「cap=1024 が L1d fit を実現して partitioned 採否を復活させる」と結論づけたが、end-to-end では c17s も同様の利益を取ってしまうため **相対的な partitioned 優位は消える**。partitioned が cap-tune で「絶対値で勝つ」絵は描けず、c17s の cap=1024 が新たな baseline 上限を作って partitioned を上回る。

設計的に言えば、L1d fit は variant 構造ではなく **cache サイズ自体の関数** なので、partitioned に固有の利益として帰属できない。partitioned の固有利益は「per-partition mutex で contention を散らす」だけで、cap=1024 帯の c17s は read-heavy で contention 自体が薄いので利益のソースが消える。

### partitioned 採否: **conditional reject**

- **reject**: Zipf 全帯、Twitter T≥2、arc_OLTP T≥2 — 設計 gate (`partitioned-design.md` §Acceptance) を満たさない
- **accept**: arc_DS1 全帯 (cap-insensitive な workload で +93–118% gain、HR drop ≈ 0)、twitter T=1 (低 T で base が低いため relative gain 出る)

採用領域は **workload-specific** で公開 lib の variant としては薄い。`senba-research` 側に keep するのは妥当だが、`senba` に上げる根拠は出なかった。

### r1 が cap=1024 で見直し対象

cap=4096 sweep で r1 accept rate 5.9% だったのが cap=1024 で 7.4% に微増、特に twitter_cluster019 / arc_DS1 で +93–183% gain。c17s も cap-tune の利益を取ったが、SHARDS=64 + ways=16 の r1 は更に取れている。**partitioned より r1 の方が cap=1024 帯では生産的** という形勢が見える。

## 今後

優先度順:

1. **bench_concurrent を Windows native で sweep**: r1 cap=4096 T=16 が VTune uarch で **206 Mops, Retiring 71.2%** = Alder Lake P-core 構造的天井近くを取れることが確定した。WSL2 sweep (c17s 155 Mops) は明らかに低く、Win native sweep で r1 採用領域マップを取り直すと workload-level の話と uarch-level の話が直結する。`bench_concurrent` を `cargo xwin` で Win cross-build → cap=4096 を主軸に r1/c17s/partitioned を sweep。
2. **r1 の Front-End Bound 17.2% を深掘り (低優先)**: Back-End / Bad Spec を絞り切った後の残骸が Front-End。`vtune -collect uarch-exploration -knob collect-frontend-bound=true` で uop cache miss / ICache footprint を見る。ただし P-core Retiring 71% は実効的に天井域なので、ROI は限定的。明確な hot spot (例: 特定 inline 関数の code expansion) が見えなければ放置でよい。
3. **two-tier / fingerprint は不要**: 元の動機 (partitioned 採否復活) は cap=1024 sweep で消え、r1 VTune で「sharded variant は L3 trap が構造的に発生しない」「r1 cap=4096 で既に 200 Mops 帯、partitioned cap=1024 ピークを超過」が確認できた。L3 を緩める algorithmic 介入は **partitioned 単独問題のパッチ** で、senba 全体としては不要。partitioned variant 自体の採否は (1) の Win native sweep で正式判定。

## r1 への L3 仮説波及 — VTune 検証手順

sweep で r1 が cap=1024 帯で逆に好調 (twitter cluster019 T=16 +183%, arc_DS1 T=16 +93%) だったのは、partitioned と同じ L3 → L1d fit の利益を更に強く取った可能性が高い。memory layout で見ると:

- partitioned T=16 N=16 cap=1024: SMT pair (2 thread / L1d 48 KiB) が 2 partition = **64 KiB を奪い合う** (報告書 §仮説 3 で L1 Bound +2.5pp 残存の主因)
- r1 cap=1024 SHARDS=64: **総 32 KiB が全 thread で共有**、SMT pair が同 line を read shared → L1d 48 KiB に収まり、coherence traffic も発生しない

**partitioned が cap=1024 でも取り切れなかった SMT pair L1d 共有の害を、r1 では構造的に解消できているか** を VTune で直接確認する。`bench_vtune_concurrent` には r1 variant が既に組み込まれている (`Variant::R1 { ways }`, `build_with_ways` 経路) ので、追加実装不要。

### クロスビルド

```bash
cargo xwin build --release -p senba-research \
    --bin bench_vtune_concurrent --target x86_64-pc-windows-msvc \
    --features "senba/concurrent"
# 成果物: target/x86_64-pc-windows-msvc/release/bench_vtune_concurrent.{exe,pdb}
```

### 測定セル (4 cell)

partitioned VTune (`2026-05-13-partitioned-vtune.md`) と直接比較できるよう **T と cap** を揃え、cap=4096/1024 × T=8/16 の 4 cell。ways は SHARDS と一致させて r1 の partition-like 最大構成。

| variant | ways | cap | T | 目的 |
|---|---|---|---|---|
| r1 | 16 | 4096 | 8 | partitioned cap=4096 T=8 (L3 Bound 41.7%) と比較 |
| r1 | 16 | 4096 | 16 | partitioned cap=4096 T=16 (L1 Bound 23.9% SMT pair 害) と比較 |
| r1 | 16 | 1024 | 8 | partitioned cap=1024 T=8 (L3 Bound 26.9% に縮小) と比較 |
| r1 | 16 | 1024 | 16 | **本命**: SMT pair L1d 共有まで取り切れているか |

共通パラメータは partitioned VTune run と同一: `keys=100000, skew=1.4, warmup=400000, ops=100000000, seed=42, shards=64`。collection は `memory-access` (top-down memory bound 内訳) を最重要、`uarch-exploration` (CPI / Retiring / Bad Spec) は cap=1024 T=8 cell でだけ追加で取れば十分 (構造比較は cap=4096 partitioned と一対一で並べる)。

### VTune 起動例 (本命 cell)

```text
vtune -collect memory-access -knob analyze-mem-objects=true \
    -knob mem-object-size-min-thres=64 -start-paused \
    -result-dir r_r1_w16_cap1024_T16_mem -- \
    bench_vtune_concurrent.exe \
    --variant r1@ways=16 --threads 16 --shards 64 \
    --cap 1024 --keys 100000 --skew 1.4 \
    --warmup 400000 --ops 100000000 --seed 42
```

他の cell は `--cap 4096|1024` と `--threads 8|16` を差し替え、`-result-dir` を `r_r1_w16_cap{cap}_T{T}_mem` に揃える。

### 結果 (Windows native i5-12600K)

#### Mops (Elapsed − Paused を測定窓に取る、100M ops 固定)

| variant | cap | T | meas (s) | aggregate Mops | partitioned 比 |
|---|---|---|---|---|---|
| **r1 w=16** | 4096 | 8 | 0.606 | **165.0** | partitioned 50.8 → **+225%** |
| **r1 w=16** | 4096 | 16 | 0.560 | **178.6** | partitioned 80.4 → **+122%** |
| **r1 w=16** | 1024 | 8 | 0.604 | 165.6 | partitioned 133.7 → +24% |
| **r1 w=16** | 1024 | 16 | 0.648 | 154.3 | partitioned 157.5 → **−2%** |

**仮説 (r1 が cap=1024 で更に伸びる) は反証**。r1 は cap=4096 T=8 の時点で 165 Mops を出しており、cap を縮めても伸びない (むしろ T=16 で −15% 微減)。partitioned が cap=1024 でやっと到達した 157.5 Mops は、**r1 が cap=4096 で既に超えていた水準**。

#### Memory Bound 内訳

| 指標 | r1 cap=4096 T=8 | r1 cap=4096 T=16 | r1 cap=1024 T=8 | r1 cap=1024 T=16 | partitioned cap=4096 T=8 (比較) |
|---|---|---|---|---|---|
| **Memory Bound (P-core)** | **4.6%** | 4.0% | 10.2% | 8.2% | **44.8%** |
| DRAM BW Bound (Uncore) | 11.6% | 1.8% | 0.0% | 7.7% | 0.0% |
| LLC Miss Count | 0 | 550,231 | 0 | 0 | 0 |
| Loads (10⁹) | 9.15 | 9.32 | 8.93 | 9.22 | (同水準) |
| **Stores (10⁹)** | 4.60 | 4.67 | **5.07 (+10%)** | **5.26 (+13%)** | — |

**r1 は最初から memory 律速ではない** — Memory Bound (P-core) が 4–10% で、partitioned cap=4096 T=8 の 44.8% に対して桁違いに低い。L3 / L1d の cache fit 仮説は r1 には適用されない、というのが正しい絵。

cap=1024 で Memory Bound が逆に **増えた** (4.6 → 10.2%) のも非自明: working set は確かに縮むが、cap=1024 では eviction 頻度が上がり、Stores が +10–13% (4.6B → 5.1B) 増えて store buffer / write-back path への圧力が出る。**r1 の cap-tune 帯では「L3 latency 解消」より「eviction churn 増加」の方が勝つ**。

### 学び — 仮説の反証から見えた r1 の本質

「**r1 は sharded access pattern が構造的に L1d fit を実現している**」が正解。partitioned のように per-thread が 128 KiB の partition を抱え込む形ではなく、SHARDS=64 で per-access あたり 1 shard (= 2 KiB at cap=4096, 512 B at cap=1024) しか touch しないので、cap=4096 でも L3 trap に落ちない:

| variant | per-thread working set / per-access | L3 律速? | cap-tune の効果 |
|---|---|---|---|
| partitioned | partition 丸ごと (128 KiB at cap=4096) | **yes** (L3 Bound 41.7%) | cap=1024 で working set 32 KiB → L1d fit → +163% Mops |
| r1 | 1 shard 分 (2 KiB at cap=4096) | **no** (Memory Bound 4.6%) | 効果なし、むしろ eviction churn で微減 |

つまり「partitioned が cap-tune でやっと r1 に並んだ」が正しい解釈で、r1 が cap=1024 で更に飛ぶ余地はなかった。`partitioned-cap1024-sweep` で r1 が twitter cluster019 T=16 で +183% gain を取ったのは、cache 階層の利益ではなく **c17s baseline 自体が cap=1024 帯で r1 ほど伸びなかった** ことに起因する (= sweep 内の相対値で、絶対値の伸びは VTune が示す通り flat)。

設計的に言えば、senba 系の sharded variant (r-series, c-series) はそもそも「per-shard cap を小さく保てば L3 trap は構造的に発生しない」性質を持っており、partitioned が抱えた問題は **partition 単位の routing が cache 階層と噛み合わない**特殊事情だった。partitioned 採否を覆す根拠は r1 VTune データからは得られない。

**残る疑問**: r1 の Memory Bound 4-10% が hardware ceiling なのか、それともまだ詰める余地のある不可視のボトルネック (mutex acquire/release、SIEVE state machine の compute、tag scan の SIMD utilization) があるのか。これは r1 単独の uarch-exploration (CPI / Retiring / Front-End / Back-End 内訳) で切り分けるべき次の問い。

### 生データ保管

VTune の `-report summary` 出力 (本セクションで引用した数値の出所) は `docs/benchmark/partitioned-vtune/data/r1_w16_cap{C}_T{T}_memory-access.txt` に保管。`-result-dir` 本体 (binary, 数百 MB) は Windows host 側に残し、本リポジトリには CLI summary text のみ。同 dir の README に命名規約と共通実行パラメータを記載。

### uarch-exploration 結果 — r1 は compute 律速、ほぼ理想 (case B)

`docs/benchmark/partitioned-vtune/data/r1_w16_cap4096_T{8,16}_uarch.txt` を取得。memory-access で 4-10% しか出なかった Memory Bound の **残り 96% は単純に「実行が回っている」だけ** だった。

#### top-down 比較

| | r1 T=8 | r1 T=16 | partitioned T=8 (`partitioned-vtune.md` 既知) | partitioned cap=1024 T=8 (既知) |
|---|---|---|---|---|
| Clockticks (10⁹) | 13.85 | 18.39 | — | — |
| **CPI Rate** | **0.358** | 0.475 | 1.746 | 0.608 |
| **Retiring (P-core)** | **59.6%** | **71.2%** | 13.6% | 44.0% |
| Front-End Bound | 15.6% | **17.2%** | 6.9% | 10.8% |
| Bad Speculation | 11.2% | 1.5% | 10.0% | 5.5% |
| **Back-End Bound** | **13.5%** | **10.1%** | **69.5%** | 39.7% |
| Load Bound (Info) | 0.087 | 0.116 | 0.539 | 0.343 |
| Store Bound (Info) | 0.010 | 0.005 | — | — |
| Retiring (E-core) | 34.3% | 37.4% | 8.4% | 30.2% |
| Bad Spec (E-core) | 43.1% | 36.6% | (中) | (中) |
| Freq (GHz) | 4.4 | 4.2 | — | — |
| Mops aggregate (= 100M / (Elapsed−Paused)) | **178** | **206** | 50.8 | 133.7 |

#### 判定: case B 確定

仮説 3 候補 (A: mutex 律速 / B: compute 律速 / C: Bad Spec) のうち **B が圧倒的**:

- **A (mutex) は否定**: Back-End Bound が r1 T=8 で 13.5% / T=16 で 10.1% しかない。partitioned 69.5% との対比で明らか、Lock Latency が hot path にいる絵にならない。`parking_lot::Mutex` の uncontended acquire は OoO で完全に隠れている
- **B (compute) が正解**: Retiring 59.6% (T=8) / **71.2% (T=16)** — Alder Lake P-core (sustained ~3 IPC = CPI 0.33 が実効上限) に対し CPI 0.358–0.475。**Instructions Retired は T=8/T=16 でほぼ同じ (38.7B)** で同じ work を 8 thread / 16 thread で割っているだけ。T=16 で CPI が悪化 (0.358 → 0.475) するのは SMT pair で issue slot を奪い合うため、ただし thread が 2x になるので aggregate Mops は 178 → 206 で +16% 取れる、SMT が ideal 域でちゃんと効いている
- **C (Bad Spec) も否定**: T=8 で 11.2%、T=16 で **1.5%** に落ちる。SIEVE state machine の branch は SMT pair sharing で BTB が温まると消える、SMT pair が enough sample を共有すると mispredict 自体が起きなくなる構造

#### 残る optimization 余地

P-core Retiring 71.2% の残り 28.8% を分解すると、**Front-End Bound 17.2% が最大**:

- Front-End Bound 17.2% — instruction fetch / decode の stall。uop cache miss / ICache footprint / 大きな code path の問題
- Back-End Bound 10.1% — Load Bound 11.6% of cycles、ただし Memory Bound 4% と整合的で、これは「load の latency が小さく cycle に乗ってる」だけで stall ではない
- Bad Spec 1.5% — 無視

つまり r1 を **これ以上削るには Front-End** (uop cache fit、indirect call 削減、scan loop の code size 圧縮) が候補。Back-End (memory / lock / scheduler) を弄っても効かない。

partitioned に対する r1 の決定的アドバンテージは **「Back-End Bound 69.5% → 13.5%」** が示す通り、memory wait に CPU が縛られていた partitioned に対し、r1 は CPU を実際に演算に使えている、という構造差。これは sharded routing が L3 trap を構造的に回避していることの uarch 側証拠。

#### 副次: Mops が memory-access 計測より高い

memory-access 計測 (前 §結果): r1 cap=4096 T=8 で 165 Mops、T=16 で 178.6 Mops
uarch 計測 (本 §): r1 cap=4096 T=8 で **178 Mops**、T=16 で **206 Mops**

差は ~10–15%。memory-access collection は uop sampling + PEBS が密で probe overhead が乗る、uarch-exploration は軽い、という構図と整合的。**r1 cap=4096 T=16 の真の天井は 200 Mops 超** と読むのが妥当 (= c17s WSL2 sweep の 155 Mops を 30%+ 上回り、partitioned cap=1024 ピーク 157.5 を 30%+ 上回る)。

## 関連レポート

- `2026-05-13-partitioned-vtune.md` — VTune 予測 (cap=1024 で +96% scaling 復活) の出所、本書の出発点
- `2026-05-12-partitioned-results.md` — cap=4096 sweep の元データ、accept rate 19.6%
- `2026-05-12-partitioned-design.md` — accept/reject 基準の出所
