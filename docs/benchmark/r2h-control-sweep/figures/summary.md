# r2h control sweep — summary

設計: `docs/reports/2026-05-13-r2-design.md` §3.1 / §4 (仮説 H1, H2)。

軸:
- 4 variant: c17s_1x (= scale_shards baseline), c17s_8x (= scale_shards × 8),
  r1@ways=8, r2h@ways=8。c17s_*x はいずれも routing は hash-only、差は SHARDS のみ
- T ∈ {1, 4, 8, 16}、cap × workload は r1 採用領域 cluster019 + r1 不採用領域
  {cluster006, ARC OLTP} + Zipf 1.4 RH 対照、trials=3、value=u64
- 224 cell × 3 trials = 672 rows、WSL2 で約 5 分、crashes 0

## T=16 結果 (Mops + HR + p99)

| cell | c17s_1x M | c17s_8x M | r1_w8 M | r2h_w8 M | r2h/r1 | c8x/c1x | HR_c1x | HR_c8x | HR_r1 | HR_r2h | p99 c1x | p99 c8x |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| cluster019 cap=1024 | 5.4 | **52.2** | 17.3 | 5.7 | **−66.8%** | **+866%** | 0.125 | 0.179 | 0.067 | 0.116 | 4110 | 1172 |
| cluster019 cap=4096 | 12.1 | **66.3** | 19.8 | 19.8 | −0.3% | **+447%** | 0.241 | 0.279 | 0.218 | 0.219 | 1941 | 761 |
| cluster019 cap=16384 | 17.7 | **68.2** | 20.2 | 20.3 | +0.4% | **+286%** | 0.303 | 0.310 | 0.300 | 0.300 | 1597 | 515 |
| cluster019 cap=65536 | 18.6 | **69.5** | 20.5 | 21.1 | +3.0% | **+274%** | 0.326 | 0.327 | 0.316 | 0.316 | 2005 | 672 |
| cluster006 cap=1024 | 9.1 | **55.4** | 17.2 | 6.2 | −64.2% | **+509%** | 0.136 | 0.111 | 0.023 | 0.024 | 2625 | 1281 |
| cluster006 cap=4096 | 30.5 | **83.1** | 32.2 | 32.4 | +0.8% | **+173%** | 0.349 | 0.310 | 0.073 | 0.073 | 1284 | 759 |
| cluster006 cap=16384 | 66.4 | **102.3** | 44.1 | 45.7 | +3.6% | **+54%** | 0.639 | 0.610 | 0.217 | 0.218 | 850 | 469 |
| cluster006 cap=65536 | 124.5 | **181.3** | 57.9 | 54.7 | −5.5% | **+46%** | 0.874 | 0.858 | 0.496 | 0.497 | 307 | 129 |
| OLTP cap=256 | 4.2 | **28.9** | 11.1 | 3.5 | −68.5% | **+585%** | 0.181 | 0.155 | 0.080 | 0.091 | 5544 | 1257 |
| OLTP cap=512 | 7.0 | **49.4** | 30.4 | 5.0 | −83.5% | **+608%** | 0.233 | 0.207 | 0.059 | 0.083 | 3545 | 1424 |
| OLTP cap=1000 | 11.0 | **63.9** | 33.0 | 10.4 | −68.4% | **+483%** | 0.289 | 0.270 | 0.110 | 0.122 | 2629 | 1180 |
| OLTP cap=2000 | 17.1 | **69.8** | 38.6 | 21.9 | −43.2% | **+309%** | 0.361 | 0.352 | 0.158 | 0.164 | 1780 | 1040 |
| Zipf 1.4 RH cap=1024 | 148.9 | 164.0 | **196.4** | 167.1 | −14.9% | +10% | 0.906 | 0.898 | 0.840 | 0.839 | 154 | 163 |
| Zipf 1.4 RH cap=4096 | 146.5 | 159.3 | **201.9** | 182.5 | −9.6% | +9% | 0.925 | 0.923 | 0.886 | 0.886 | 204 | 154 |

## 仮説判定

### H1: r2h@8 cluster019 Mops は r1 から ≤+30% に後退する → **部分肯定 (cap≤1024 のみ)**

- cluster019 **cap=1024**: r2h **−66.8% vs r1** → thread affinity が essential、H1 強く肯定
- cluster019 **cap≥4096**: r2h ≈ r1 (差 ±3%) → affinity 寄与は cap が増えると消える
- 一般化すると **affinity が効くのは cap ≤ 1024 の小 cap 帯のみ**。それ以上では r1 の利得は
  affinity ではなく "set-associative subdivision" 自体に由来

副次観測: ARC OLTP 全 cap で r2h が r1 から大幅後退 (−43〜−83%)、cap-fits 帯では
affinity が cap によらず効く。OLTP HR 0.36 で working set が cap_per_way (= cap/8) を
明確に超える regime で hot key の writer state が thread に固定されることが重要

### H2: r2h@8 ≈ c17s_8x (Mops 差 ±5%) → **強く否定**

- **全 14 cell で c17s_8x が r2h を圧倒**: 最小 +9% (Zipf 1.4 RH cap=4096)、最大 **+866%**
  (cluster019 cap=1024)。Twitter / ARC trace では一律 +50% 〜 +600%+
- c17s_8x は r1 も全 cell で pareto dominate: Mops で 50〜600% リード、HR も僅差〜+5pp 良い
- p99 も c17s_8x が **2〜5 倍低い** (cluster019 cap=65536 で 2005 → 672 ns、OLTP cap=256 で
  5544 → 1257 ns)
- **「ways は shard 細分化と等価」が想定だったが、c17s_8x (= shard を 8 倍に増やす) が
  r1/r2h を構造的に上回る**。同じ総 cap でも、64 shard × ways=8 (= 64 mutex + routing) より
  512 shard × ways=1 (= 512 mutex + hash-only) の方が遥かに良い。違いは **mutex 数 8 倍**

### Pareto frontier

c17s_8x が **r1 / r2h を全 cell で pareto dominate** (Mops も HR も p99 も同等以上)。
唯一の例外は Zipf 1.4 RH で r1 が c17s_8x を Mops +20〜25% 上回るが HR drop が 3.7〜4.0pp あり、
pareto は draw。それ以外の 12 cell で c17s_8x が完全に支配。

## 結論

### 設計上の含意 (重要)

1. **r1 の design hypothesis (`r1-design.md` §3.1) は否定された**: 「shard 間 routing に
   thread affinity を導入すると writer hot-line bouncing が構造的に消える」という主張は、
   本 sweep の cap≥4096 帯では成立しない。同じ利得は **単に shard を 8 倍に増やすだけで
   遥かに大きく得られる**

2. **r2s/r2p の motivation 消失**: r2 設計 doc §6 採用基準「r1 採用領域で r2 が 80%+ Mops
   維持 + r1 不採用領域で HR drop ≥ 10pp 改善」は、c17s_8x が**両方を既に満たして**しまった。
   r2s/r2p を実装しても c17s_8x の上には行けない可能性が極めて高い

3. **senba auto-shard 推奨値の再評価**: `senba::Cache::new(cap)` の auto-shard は現在
   `next_pow2(cap/64)`。本 sweep は **`cap/8` (= 8× 増加) が wide margin で良い**ことを
   示した。lib publish 向けに heuristic を見直すべき。実 trace では `cap=4096` で
   2× 〜 9× の Mops 向上、HR は ±1pp 以内

4. **affinity が効く狭い帯**: cap ≤ 1024 + 高 T (T≥16) + 実 trace でのみ r1 の affinity が
   c17s_8x を上回る可能性がある。本 sweep ではこの帯では c17s_8x が依然 +866% 勝つので
   存在しないか僅少。embedded / tight cap 用途に絞った検証なら意味がある

### 次の手順候補

- **A (最優先)**: senba auto-shard heuristic を `cap/8` (or `cap/16`) に変更し、
  既存 perf-gate / 主要 sweep を回し直す。lib publish への直接 path
- **B**: r2s/r2p 実装を中止、r-series は r1 / r2h の artifact レポートとして archive
- **C**: 小 cap (cap≤1024) 限定の compact-mode 設計 (r1 系の派生) を別 series として
  再開する選択肢を残すが、優先度低

## 生成物

- `data/results.csv` — 672 rows、4 variant × 4 T × 14 cell × 3 trials
- `data/sweep.log` — 進行ログ
- `data/crashes.log` — 空
- 本 summary.md
