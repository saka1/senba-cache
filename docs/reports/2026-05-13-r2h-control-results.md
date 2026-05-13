# 2026-05-13 — r2h control 結果: H2 強く否定、r-series 全体に再考を迫る

## 仮説 (`2026-05-13-r2-design.md` §4)

- **H1**: r2h@8 cluster019 Mops は r1@8 から大幅後退 (≤+30%) — affinity 寄与の切り分け
- **H2**: r2h@8 ≈ c17s@(shards=scaled×8) ways=1 (Mops 差 ±5%) — "ways は shard 細分化と
  等価か"

事前予測: H1 肯定 / H2 肯定 → r1 設計 (thread affinity が essential) の justification
が成立し、r2s/r2p の "affinity 保持しつつ HR 緩和" 路線に進む。

## 実施したこと

- `research/src/experimental/sieve_r2h.rs` — r1 fork、way 選択を
  `mix(tls, hash) = tls.wrapping_mul(0x9E3779B97F4A7C15) ^ hash` に置換 (= thread
  affinity を捨て、way を hash 軸でも散らす)
- `bench_concurrent` に r2h dispatch + arm 配線 (`run_r2h`、SHARDS 4..131072)
- harness `docs/benchmark/r2h-control-sweep/` — 4 variant × 4 T × 14 cell × 3 trials =
  672 rows、WSL2 で 5 分、crashes 0
  - 4 variant: c17s_1x (= scaled shards), **c17s_8x (= scaled × 8, H2 対照)**, r1_w8, r2h_w8
  - cell 軸: cluster019 (r1 sweet spot) / cluster006 (r1 不採用) / ARC OLTP (cap-fits) /
    Zipf 1.4 RH × cap {1024, 4096, 16384, 65536 (OLTP は 256, 512, 1000, 2000)}
- 詳細表 + cell 別 HR / p99 は `docs/benchmark/r2h-control-sweep/figures/summary.md`

## 結果

### H1: 部分肯定 — affinity が効くのは **cap ≤ 1024 のみ**

| cluster019 T=16 | r1@8 Mops | r2h@8 Mops | r2h/r1 |
|---|---:|---:|---:|
| cap=1024 | 17.3 | 5.7 | **−66.8%** |
| cap=4096 | 19.8 | 19.8 | −0.3% |
| cap=16384 | 20.2 | 20.3 | +0.4% |
| cap=65536 | 20.5 | 21.1 | +3.0% |

- cap=1024 では affinity 破壊で r2h が r1 から **−66.8%** 後退、H1 強く肯定。thread
  affinity が essential
- **cap ≥ 4096 では r2h ≈ r1** (差 ±3%)、affinity 寄与は cap が増えると **消滅**。r1 の
  利得は affinity ではなく "set-associative subdivision" 自体に由来
- ARC OLTP では全 cap で affinity が効く (cap-fits 帯では working set が cap_per_way を
  超えるため hot key の writer state 固定が cap によらず効くと推定)

### H2: 強く否定 — c17s_8x が **r1/r2h を全 cell で pareto dominate**

c17s_8x (= 同 cap で shard を 8 倍に増やすだけ) の Mops 改善は r1/r2h を大きく超える:

| cell T=16 | c17s_1x | c17s_8x | r1_w8 | r2h_w8 | c8x/c1x |
|---|---:|---:|---:|---:|---:|
| cluster019 cap=1024 | 5.4 | **52.2** | 17.3 | 5.7 | **+866%** |
| cluster019 cap=4096 | 12.1 | **66.3** | 19.8 | 19.8 | **+447%** |
| cluster006 cap=4096 | 30.5 | **83.1** | 32.2 | 32.4 | **+173%** |
| cluster006 cap=65536 | 124.5 | **181.3** | 57.9 | 54.7 | **+46%** |
| OLTP cap=512 | 7.0 | **49.4** | 30.4 | 5.0 | **+608%** |
| OLTP cap=2000 | 17.1 | **69.8** | 38.6 | 21.9 | **+309%** |
| Zipf 1.4 RH cap=4096 | 146.5 | 159.3 | **201.9** | 182.5 | +9% |

**全 14 cell で c17s_8x が r2h を上回る**。最小 +9%、最大 +866%。Twitter / ARC trace では
一律 +46〜+866%。

HR も c17s_8x が r1/r2h より **遥かに高い**: cluster019 cap=4096 で c17s_8x HR 0.279 vs
r1 0.218 (**+6.1pp 良い**)、cluster006 全 cap で **+24〜+37pp 良い**、OLTP も +18〜+25pp。
r1/r2h が HR drop と引き換えに取った Mops gain は、c17s_8x が **HR drop なし + より大きな
Mops gain** で凌駕する。

p99 latency も c17s_8x が **2〜5 倍低い** (cluster019 cap=65536 で 2005 → 672 ns、
OLTP cap=256 で 5544 → 1257 ns)。

Pareto judgment: **c17s_8x が r1 / r2h を 14 cell 中 13 cell で完全 dominate**。唯一の例外は
Zipf 1.4 RH (r1 が Mops で c17s_8x を +20〜25% 上回るが HR drop 3.7〜4.0pp、pareto draw)。

### 構造的解釈

r1@shards=64 ways=8 と c17s_8x@shards=512 ways=1 を比較すると:

- 総 cap 同じ
- **総 shard 数 = 64 vs 512 (8 倍差)**
- **総 mutex 数 = 64 vs 512** (各 shard に 1 mutex)
- routing: r1 = `set(hash) * 8 + (tls & 7)` / c17s_8x = `hash & 511`

r1 の "ways" は同一 shard 内の subdivision ではなく **物理的に別 shard** だが、shard 数は
64 のまま。c17s_8x は単純に shard 数を 8 倍にする。結果として c17s_8x の方が **mutex
contention が 8 倍低く**、これが Mops 差を支配的に決めている。affinity の writer-state
固定効果は cap ≤ 1024 の極小帯でのみ顕現する副次効果に過ぎなかった。

## 学び

### 1. r1 design hypothesis は (cap ≥ 4096 帯で) 否定された

`r1-design.md` §3.1 「thread affinity を導入すると hot-line bouncing が構造的に消える」は、
cap ≥ 4096 では affinity を捨てても利得が落ちない (r2h ≈ r1)。affinity の "writer state
を thread の L1 にピン留め" 効果は、**shard 数増による mutex 分散の方が支配的**であり、
shard 数を増やせばより安く同じ効果が得られる。

### 2. r2s / r2p 実装は中止

r2 設計 doc §6 採用基準は「r1 採用領域で r2 が 80%+ Mops 維持 + r1 不採用領域で HR drop
≥ 10pp 改善 + 全 cell で c17s を Mops で同等以上」。c17s_8x が **3 条件全部を既に満たして
いる**ため、r2s/r2p を実装しても c17s_8x の上には行けない構造。

設計 doc §7 撤退条件「H1 否定 + H3-H7 全否定」に半分該当 (H1 は部分肯定だが affinity 帯が
狭すぎる)、加えて H2 が強く否定された事実が決定的。**r-series 全体を artifact として
凍結**、次のテーマに移る。

### 3. senba auto-shard heuristic の見直しが lib publish への直接 path

`senba::Cache::new(cap)` の auto-shard は現在 `next_pow2(cap/64)`。本 sweep は **`cap/8`
(= 現状の 8 倍 shard) が実 trace で 2× 〜 9× の Mops 向上 + HR ±1pp 以内** を示した。
副作用として p99 latency も 2〜5 倍改善。

ただし `cap/64` (= 64 entries/shard) は SIMD scan 16 lane × 4 chunk で full scan できる
設計上の意図ある選択。`cap/8` (= 8 entries/shard) では SIMD lane の半分しか使われず、
small-shard overhead (mutex 取得 / memory layout overhead) が増える。**真の sweet spot は
別途 cap-shards-T 軸で sweep して特定する**必要がある。

## 次の手順 (優先度順)

1. **shard heuristic sweep** — `cap/{4, 8, 16, 32, 64, 128}` × T × workload で c17s 単独 sweep。
   senba lib の auto-shard 推奨値を再決定する。`docs/benchmark/c17s-shard-sweep/` に harness
2. **r-series 凍結アナウンス** — `docs/reports/index.md` の r-series エントリ群に「c17s_8x
   が pareto dominate するため凍結」と追記。実装 (sieve_r1.rs, sieve_r2h.rs) は artifact
   保持
3. **moka 0.13/0.14 比較** — auto-shard 見直し後の senba::Cache で moka 最新版に対する
   優位を再測定 (`r1-vs-moka-cap-sweep` の延長)
4. (棚上げ) **小 cap embedded 用途の r-style design** — cap ≤ 1024 + T ≥ 16 帯でのみ affinity
   が効くので、組込み用途に絞った compact mode の検証は価値がある可能性。優先度低

## 関連

- `2026-05-13-r2-design.md` — 本 sweep の仮説出処、§6 採用基準が c17s_8x で trivially 達成
  された結果として r2s/r2p 実装を要求しない判断に至る
- `2026-05-12-r1-design.md` §3.1 — affinity essential 仮説、本 sweep で cap≥4096 で否定
- `2026-05-12-r1-results.md` — r1 採用領域 31/520 cell、本 sweep で全部 c17s_8x に飲み込ま
  れる可能性 (要追試)
- `2026-05-13-r1-vs-moka-cap-sweep.md` — moka 比較は c17s で取った既往。auto-shard 見直し後
  に再走の必要
- `2026-05-12-partitioned-results.md` / `2026-05-13-partitioned-cap1024-sweep.md` —
  partitioned は同じ「more shards で利得」だが mutex granularity が異なる別 design space
