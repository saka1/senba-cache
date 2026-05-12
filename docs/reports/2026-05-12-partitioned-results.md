# `senba::concurrent::PartitionedCache` sweep 結果

## 仮説

`senba::Cache` を N 個並べて thread-id でルーティングする最小構成 (`PartitionedCache`) は、reader fast path の Acquire load 累積を構造的に 0 にできるため、
T=16 で c17s 159 Mops を ceiling 推定 640 Mops に向けて押し上げられる。
`2026-05-12-partitioned-design.md` の事前モデルは T=16, N=16, Zipf 1.4 read-heavy u64 で **~457 Mops (c17s の 2.9×)** を期待値、最低でも **+50%** を採用ライン (= keep in lib) と定めた。

## 実施したこと

`senba/concurrent` feature を有効化した `bench_concurrent` で (T × N × workload × variant) sweep。
詳細は `docs/benchmark/partitioned-sweep/`。

- 軸: T ∈ {1,2,4,8,16}、N (partitioned) ∈ {1,2,4,8,16}、WAYS (r1) ∈ {1,4,16}
- workload: Zipf {0.8/gim, 1.0/gim, 1.4/gim, 1.4/read-heavy} (cap=4096) + Twitter Yang {006,019,034} (cap=4096) + ARC {OLTP, DS1} (cap=4000)
- value: u64 (string は将来扱い)、trials=3、ops=2M、warmup=200k、shards=64
- variants: `partitioned`, `c17s` (baseline @ ways=1), `r1` (cross-reference)
- 計 1215 trial、5 分弱で完走、crash log 空

## 結果

### 設計 gate cell (Zipf 1.4 read-heavy, T=16, N=16): **採用ライン未達**

事前モデル 457 Mops 目標、採用ライン c17s 比 +50% 以上。実測:

| variant | T=16 Mops | HR | vs c17s |
|---|---|---|---|
| c17s @ ways=1 | **133.1** | 0.925 | (baseline) |
| partitioned N=16 | 75.4 | 0.865 | **−43.3%** |
| r1 ways=16 | 172.2 | 0.865 | +29.3% |

partitioned は c17s に **倍以上の差で負ける**。design §Acceptance / Reject の reject 条件 ("Zipf 1.4 read-heavy u64 で c17s と同等またはそれ以下") に該当する。
原因の構造: ① c17s が Zipf hot-key 帯で AVX2 scan + epoch fast path で per-op ~7.5 ns に収束、② partitioned は `parking_lot::Mutex` acquire (~5 ns) + lib の ST hot path (~25 ns) で per-op ~30 ns、つまり「mutex を取ること自体」が Zipf 帯では負債。事前モデルの 35 ns/op はオーダー通りだが、c17s 側の `2026-05-12-mt-overhead-vs-lib.md` 計測時点から **Zipf hot-key 帯の per-op が改善している** ため ceiling 差が縮んでいる。

T 別の Mops (zipf 1.4 read-heavy):

| T | c17s | partitioned N=16 | r1 w=16 |
|---|---|---|---|
| 1 | 23.3 | 17.0 | 18.8 |
| 2 | 42.6 | 20.6 | 42.0 |
| 4 | 77.7 | 26.3 | 80.6 |
| 8 | 104.8 | 40.3 | 109.8 |
| 16 | 133.1 | 75.4 | 172.2 |

T=1 ですら partitioned は c17s に負ける。**Zipf 帯では partitioned は構造的に競合できない**。

### しかし real trace では partitioned が圧勝する

T=16 で各 workload の **best variant** を比較:

| workload | c17s | partitioned best (N) | gain | HR drop | r1 best | r1 gain |
|---|---|---|---|---|---|---|
| zipf 0.8 gim | 32.5 | 41.2 (N=16) | +27% | 24.3 pp | 47.1 (w=16) | +45% |
| zipf 1.0 gim | 52.7 | 46.8 (N=16) | **−11%** | 23.3 pp | 62.8 (w=16) | +19% |
| zipf 1.4 gim | 147.2 | 73.7 (N=16) | **−50%** | 6.3 pp | 152.7 (w=1) | +4% |
| zipf 1.4 read-heavy | 133.1 | 75.4 (N=16) | **−43%** | 6.0 pp | 172.2 (w=16) | +29% |
| twitter cluster006 | 24.6 | 49.9 (N=16) | **+102%** | 30.6 pp | 34.2 (w=16) | +39% |
| twitter cluster019 | 10.8 | **52.9 (N=16)** | **+390%** | 2.6 pp | 19.6 (w=16) | +82% |
| twitter cluster034 | 16.8 | 51.3 (N=16) | **+206%** | 11.1 pp | 32.2 (w=16) | +92% |
| arc OLTP cap4000 | 25.7 | 53.7 (N=16) | **+109%** | 28.0 pp | 47.9 (w=16) | +86% |
| arc DS1 cap4000 | 9.5 | **45.7 (N=16)** | **+383%** | 0.2 pp | 11.3 (w=16) | +19% |

つまり領域は **真っ二つに割れる**:

- **Zipf 系 (skew ≥ 1.0)**: c17s 圧勝、partitioned は乗り越えられない。hot-key 集中 → partition 内 contention が atomic ops 並みになり、かつ HR penalty が partitioned で増幅。
- **real trace (Twitter, ARC)**: partitioned 圧勝。c17s 自身の per-op ns が trace のランダム性で AVX2 + epoch fast path から外れて 25–50 Mops 帯に張り付くため、その上から見ると "ST lib を T 並べる" 構造の優位が露呈する。

### 採用領域 (HR drop ≤ 5pp AND Mops gain ≥ +20% vs c17s) のカウント

| variant | total cells | accept cells | accept rate |
|---|---|---|---|
| partitioned | 225 | **44** | 19.6% |
| r1 | 135 | 8 | 5.9% |

設計の「100 cell 以上」採用条件には届かなかった (44/225 = 19.6%)。
ただし accept zone に乗ったセルは **HR-tolerant な real trace + T が大きい領域に集中** していて、領域として意味のある塊。

### 鍵となる contrast cells (設計書 §sweep 5 種)

| note | workload | T | N | Mops | c17s | gain | HR drop |
|---|---|---|---|---|---|---|---|
| uncontended ceiling | zipf 1.4 read-heavy | 16 | 16 | 75.4 | 133.1 | **−43%** | 5.98 pp |
| degenerate (1 mutex) | zipf 1.4 read-heavy | 16 | 1 | 2.4 | 133.1 | −98% | −0.02 pp |
| T<N surplus | zipf 1.4 read-heavy | 4 | 16 | 26.3 | 77.7 | −66% | 5.61 pp |
| HR-sensitive | arc OLTP | 16 | 16 | 53.7 | 25.7 | +109% | 28.04 pp |
| HR-tolerant | twitter cluster019 | 16 | 16 | 52.9 | 10.8 | **+390%** | 2.60 pp |

事前モデル通り degenerate (N=1) は全 thread 1 mutex 直列化で −98%、T<N surplus は uncontended でも HR penalty を取り戻せず −66%。
HR-sensitive ARC OLTP は予想通り +109% Mops と引き換えに 28 pp HR drop、HR-tolerant Twitter cluster019 は予想以上に強く +390% (HR drop は 2.6 pp で accept zone 内)。

## 学び

### 仮説の更新

- **「ST lib を T 並べると lib 単スレの T 倍」は Zipf hot-key 帯では成立しない**。c17s の AVX2 scan + epoch fast path は read miss が少ない領域で per-op を mutex 1 本 (≈ 5 ns) より小さくしている疑いが強い。Zipf 1.4 で c17s 133 Mops は T=16 で per-thread 8 Mops = per-op 125 ns、これは ceiling 推定 640 Mops 比 21% に過ぎず "詰まっている" のは事実だが、partitioned の 30 ns/op 床ですら超えられない。`mt-overhead-vs-lib` を再計測すると c17s 側が下がっている可能性 (lib 改善が c17s に乗っている)。
- **partition の真の領分は "1 Mutex を取った後の per-op の小ささではなく、c17s の per-op が trace で膨らむ workload"**。Twitter / ARC では c17s が trace の access pattern (long-tail / sparse) で per-op 100–400 ns 帯に張り付くので、partitioned が 30 ns/op を出すと per-thread 30+ Mops × T で圧倒する。
- HR penalty は事前モデル通り `keys が thread 跨ぎ shared か` で決定。Twitter cluster019 は scan-heavy (HR drop わずか 2.6 pp)、ARC OLTP は OLTP 特有の repeated hot key で 28 pp drop。後者は accept zone から外れる。

### 設計 reject 条件と現実

design §Acceptance / Reject は「Zipf 1.4 read-heavy で c17s 同等以下 → reject (senba-research 行き)」を明文化していて、本実測はこれに完全合致する。
ただし

1. real trace の +109% – +390% は library として強い実需要 (mini-moka / moka に対する選択肢として独立に価値あり)
2. 44 cell の accept zone は「採用ライン 100 cell」未満だが、領域として連続している (= scan-heavy & T 大)
3. API surface は最小 (lib hot path 完全非影響、`concurrent` feature default-off)

ので、**機械的に reject せず "lib に置く / experimental 行き" は人間が決める** べき結果。本書はあくまで定量を残すまで。

### Scaling と物理層のボトルネック

uncontended (N=T) でも per-thread Mops が T=1 比で 4–5× 落ちる。zipf 1.4 read-heavy:

| | per-op (ns) | T=1 比 |
|---|---|---|
| T=1, N=1 (= ST lib + parking_lot Mutex) | 44 | 1.0× |
| T=8, N=8 | 185 | 4.2× |
| T=16, N=16 | 212 | 4.8× |

計測機は i5-12600K (6 P-core + 4 E-core、SMT で 16 thread)。N=T を上げても per-thread が伸びないのは **partition Mutex / SIEVE ロジックが各 thread 独立にも関わらず物理コアを共有している** からで、要因の候補は (a) E-core 混入 (T>6 で割込み)、(b) SMT pair の L1/L2 共有、(c) cross-core snoop / memory BW 飽和。

working set 見積りで `senba::Cache` cap=4096 / Slot32 は tags 8 KiB + entries 96 KiB ≈ **128 KiB / partition**。L1d 48 KiB から確実に溢れ、L2 1.25 MiB/P-core には 1 partition は乗るが SMT pair が 2 partition 共有で 256 KiB と詰まり始める。tag scan + entry follow で per-op に 2–3 cache line を触るパスが SMT pair で取り合いになっている、というのが現状の最有力仮説。

T=1→T=8 で +4× per-op、T=8→T=16 で +15% という伸び方は「物理コア飽和 + E-core 混入が支配、SMT 追加分は edge」のカーブと整合する。真の uncontended ceiling は SMT を切って T ≤ P-core 数 (= 6) で取り直さないと出ない。

### 今後

- **VTune memory-access + microarch profiling で SMT thrashing の直接観測** (hot-key 競合は partitioned では構造的に発生しないので scope 外、純粋に L1/L2 port contention と cache-line evict rate を見る)。`research/src/bin/bench_vtune_concurrent.rs` をベースに `partitioned --partitions {1,8,16}` × `--threads {1,8,16}` の 6 cell。期待: T=8→16 で L1d miss / L2 demand miss が SMT pair で増幅。
- T を P-core 数 (= 6) に絞った "no SMT" sweep — uncontended ceiling の本当の値を取り、64 Mops に近いのか 200 Mops に近いのかで partitioned の library 価値が変わる
- string value sweep (mt-overhead-vs-lib で string 帯は overhead が増幅する系統あり、partitioned 有利かも)
- ARC capacity sweep (cap/N が 1 partition cap = 250 まで縮むときの HR drop 曲線、§学び の "HR–cap slope" 仮説の直接検証)
- mini-moka / moka との横比較 (今回 scope 外、real trace 帯で partitioned が library として勝てるか)
- adaptive N (workload に応じて N を runtime 切替) は設計書 out-of-scope だが、real-trace 圧勝 / Zipf 完敗の領域分割があまりに鮮明なので候補

## 関連レポート

- `2026-05-12-partitioned-design.md` — 本書の前段、API / sweep 計画 / accept-reject 基準
- `2026-05-12-mt-overhead-vs-lib.md` — c17s T=1 overhead vs lib (ceiling 推定の出所)
- `2026-05-12-r1-results.md` — set-associative 流の cross-reference (本 sweep でも r1 を横に並べた)
- `docs/benchmark/partitioned-sweep/` — 生 CSV / 36 figures / summary.md
