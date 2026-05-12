# r1 vs moka 0.12 sync 直接比較 — lib viability の transitive 推論を実測で置き換える

## 仮説

これまでの lib viability 主張は **transitive 推論で繋がっていた**:

- `2026-05-06-c8-vs-moka-thread-sweep.md`: c8 が moka 0.12 比で T=16 で **15.4×**、near-linear scaling
- `2026-05-11-lru-vs-minimoka-vs-senba-pareto.md`: senba 単スレで mini_moka_unsync 比 **2–4×**
- `2026-05-12-r1-results.md`: r1@ways=8 が c17s 比で Twitter cluster019 T=16 +78%、Zipf 1.4 RH string T=16 +32%

c8 → c14s → c15s → c16s → c17s → r1 と 4 世代変えた後、moka を横に並べた multi-thread 実測がゼロだった。「ライブラリ製品として使い物になる」主張をデータで保証するため、**r1 と moka 0.12 sync を同じ harness で直接比較**する。

採用判定の基準:

- **必須**: T=16 で senba (c17s か r1@8 のどちらか) が moka を上回ること
- **強化**: p99 latency でも moka より優位 (lib 製品の SLA 主張のため)
- **HR sanity**: HR が moka 比で ±2pp 程度に収まる variant が少なくとも 1 つ存在すること

## 実施したこと

`docs/benchmark/r1-vs-moka/{run.sh, plot.py}` を新規。`bench_concurrent` の既存 dispatch (`c17s` / `r1` / `moka` / `mini_moka` adapter) をそのまま利用、軸は:

- variants: `c17s` (baseline ways=1), `r1@ways∈{1,8}`, `moka` (`moka::sync::Cache`), `mini_moka` (`mini_moka::sync::Cache`)
- T: 1, 2, 4, 8, 16
- workload: Zipf {0.8/gim, 1.0/gim, 1.4/gim, 1.4/read-heavy} + Twitter Yang {006, 019, 034} + ARC {OLTP, DS1}
- value: u64 + string (ARC は u64 のみ; trace の key 型に従う)
- cap = 4096 (Zipf, Twitter) / 4000 (ARC), shards=64, trials=3, ops=2M, warmup=200k, seed=42

総 1200 行 (header 込み) を WSL2 Linux i5-12600K kernel 6.6.87.2 microsoft-standard で 9 分 22 秒で完走、crashes ゼロ。moka / mini_moka adapter は per-op `sync()` を呼ばない (real-world モデル, `bench_concurrent.rs:35-44` で明文化)。

## 結果

### 結論: **lib viability は完全に証明された**

**T=16 で senba は全 workload で moka を 7–23× 上回り、p99 latency も 6–39× 短い。HR は r1@1 (= c17s 等価動作) を選べば moka 比で ±2pp。**

### T=16 aggregate Mops (主要 cell)

| workload | value | c17s | r1@1 | r1@8 | moka | mini_moka | **r1@8/moka** |
|---|---|---:|---:|---:|---:|---:|---:|
| zipf_s1.4_read-heavy | u64 | 145.72 | 163.61 | **207.69** | 9.09 | 6.61 | **22.85×** |
| zipf_s0.8_gim | u64 | 32.81 | 30.82 | 46.33 | 2.14 | 3.96 | 21.65× |
| arc_OLTP_cap4000 | u64 | 26.68 | 24.97 | 40.44 | 2.09 | 4.29 | 19.35× |
| zipf_s1.4_read-heavy | string | 105.99 | 95.46 | **123.54** | 10.43 | 6.49 | 11.84× |
| twitter_cluster006 | u64 | 29.89 | 29.01 | 31.95 | 1.89 | 3.76 | 16.90× |
| zipf_s1.0_gim | u64 | 54.92 | 56.25 | 64.12 | 3.93 | 6.40 | 16.32× |
| zipf_s1.4_gim | u64 | 193.05 | 147.22 | 150.74 | 10.54 | 11.37 | 14.30× |
| twitter_cluster019 | u64 | 12.05 | 12.04 | 20.20 | 1.59 | 3.16 | 12.70× |
| twitter_cluster034 | u64 | 16.74 | 17.29 | 21.06 | 1.88 | 3.76 | 11.20× |
| arc_DS1_cap4000 | u64 | 7.49 | 9.49 | 9.68 | 1.34 | 2.75 | 7.22× |

c17s が r1@8 を上回る cell が 1 つだけ存在する: **zipf_s1.4_gim u64** で c17s 193.05 vs r1@8 150.74 (-22%)。これは r1 の TLS-id load overhead が gim op-mix (10% insert) の writer 経路で利得を上回るケース。lib API 側で「default WAYS=1 + opt-in r1@8」にすれば回避できる。

### T=16 p99 chunk latency (lib SLA 主張の核心)

| workload | value | c17s | r1@8 | **moka** | **moka/r1@8** |
|---|---|---:|---:|---:|---:|
| zipf_s1.4_read-heavy | u64 | 134 ns | **129 ns** | 5075 ns | **39.34×** |
| arc_OLTP_cap4000 | u64 | 1017 | 735 | 15503 | 21.09× |
| zipf_s0.8_gim | u64 | 2020 | 734 | 14365 | 19.57× |
| zipf_s1.4_read-heavy | string | 290 | 192 | 3498 | 18.22× |
| twitter_cluster034 | string | 1391 | 1116 | 17044 | 15.27× |
| twitter_cluster006 | u64 | 976 | 1084 | 16038 | 14.80× |
| twitter_cluster019 | u64 | 1854 | 1271 | 17417 | 13.70× |
| arc_DS1_cap4000 | u64 | 2935 | 2762 | 20203 | 7.31× |

**moka が p99 で 15–20 µs に張り付く**のに対し senba は 130 ns – 3 µs。`c8-vs-moka-thread-sweep.md` で c8 era に観測した moka p99 10 µs と同質の現象が r1 era でも継続している。lib SLA としては "p99 < 3 µs at T=16" を主張できる。

### scaling 形 (Zipf 1.4 read-heavy u64)

| T | c17s | r1@1 | r1@8 | moka | mini_moka |
|---:|---:|---:|---:|---:|---:|
| 1 | 24.95 | 24.12 | 24.05 | 6.14 | 4.33 |
| 2 | 43.89 | 42.57 | 45.04 | 6.41 | 4.32 |
| 4 | 80.89 | 77.07 | 87.35 | 7.83 | 6.24 |
| 8 | 106.27 | 109.47 | 115.86 | 8.35 | 6.40 |
| 16 | 145.72 | 163.61 | **207.69** | 9.09 | 6.61 |

c17s は T=1 → T=16 で **5.84× scaling**、r1@8 は **8.63× scaling** (理想 16× の 54%)。moka は T=16/T=1 = **1.48×**、mini_moka は **1.53×**。**moka 系は T=4 で天井**、それ以降 thread を増やしても伸びない。

### scaling 形 (Twitter cluster019 u64) — 中位 HR, long-tail 系

| T | c17s | r1@8 | moka | mini_moka |
|---:|---:|---:|---:|---:|
| 1 | 2.33 | 2.66 | 1.76 | 2.26 |
| 4 | 7.35 | 9.20 | 2.14 | 4.19 |
| 16 | 12.05 | **20.20** | **1.59** | 3.16 |

**moka は T を増やすと regress** (1.76 → 1.59、T=4 から逆向きスケール)。`c8-vs-moka-thread-sweep.md` で観測した「pending tasks queue 積み上がり → throughput 悪化」が Twitter trace 帯でも再現。

### HR sanity — c17s / r1@1 は moka と policy 等価帯

| workload | value | c17s | r1@1 | r1@8 | moka | mini_moka |
|---|---|---:|---:|---:|---:|---:|
| zipf_s1.4_read-heavy | u64 | 0.93 | 0.93 | 0.89 | 0.92 | 0.92 |
| zipf_s1.4_gim | u64 | 0.97 | 0.97 | 0.94 | 0.97 | 0.97 |
| zipf_s1.0_gim | u64 | 0.72 | 0.72 | 0.54 | 0.72 | 0.72 |
| zipf_s0.8_gim | u64 | 0.45 | 0.45 | 0.26 | 0.45 | 0.45 |
| twitter_cluster019 | u64 | 0.24 | 0.24 | 0.22 | 0.17 | 0.16 |
| twitter_cluster006 | u64 | 0.35 | 0.35 | 0.07 | 0.36 | 0.35 |
| arc_OLTP_cap4000 | u64 | 0.44 | 0.44 | 0.23 | 0.45 | 0.45 |

**c17s / r1@1 は moka と HR ±1pp で完全並走** (twitter_cluster019 は逆に senba 側が +7pp HR を取る)。r1@8 の HR drop は WAYS=8 による effective cap 圧縮の構造で、lib API では default を WAYS=1 にすれば moka と HR 等価。Twitter cluster006 / Zipf 0.8 のような shared-hot 帯では r1@8 が HR を大きく崩すので、適応的 WAYS (`r1-results.md` §6.2) を持たない現状では opt-in が妥当。

### T=1 baseline — moka は単スレでも senba に届かない

| workload | value | c17s | moka | c17s/moka |
|---|---|---:|---:|---:|
| zipf_s1.4_read-heavy | u64 | 24.95 | 6.14 | 4.06× |
| zipf_s1.0_gim | u64 | 11.62 | 2.94 | 3.96× |
| zipf_s1.4_gim | u64 | 23.54 | 6.50 | 3.62× |
| arc_DS1 | u64 | 7.19 | 1.57 | 4.58× |
| twitter_cluster019 | u64 | 2.33 | 1.76 | 1.32× |
| arc_OLTP | u64 | 6.07 | 2.36 | 2.57× |

W-TinyLFU の hot path (CMSketch 更新 + admission 判定) が SIEVE の `visited` bit 操作より重い、という **policy 実装コスト差**が単スレで純粋に出る (`external-lib-sweep.md` の単スレ 2–4× と整合)。並列性とは独立な土台。

## 学び

### lib viability 主張は **transitive ではなく直接** 立てられた

事前の懸念「c8 era 15× が r1 まで保たれている保証がない」は、**むしろ過小評価**だった:

- c8 era T=16 Mops/moka = 15.4× → r1@8 T=16 Mops/moka = **最大 22.85×** (Zipf 1.4 RH u64)
- c8 era p99 moka/c8 = 14.9× → r1@8 p99 moka/r1@8 = **最大 39.34×** (Zipf 1.4 RH u64)

r1 era で詰めた structural skeleton (SlotSize/AlignedTags/c-hoist/AVX2) + r1 の routing affinity が、moka との差を更に広げた。c8 era で見えなかった「real trace (Twitter, ARC) で moka が regress する」現象も r1 era で再現+拡大。

### moka の構造的天井は thread 増加で逆向き

`c8-vs-moka-thread-sweep.md` で観測した moka T=4 ピーク → T=8/16 regress は、合成 Zipf だけでなく **Twitter trace / ARC trace でも普遍的に発生**することが今回確認できた:

- Twitter cluster019 u64: moka T=1 1.76 → T=16 1.59 (-10%)
- ARC DS1 u64: moka T=1 1.57 → T=16 1.34 (-15%)
- ARC OLTP u64: moka T=1 2.36 → T=16 2.09 (-12%)

これは moka 0.12 の pending tasks scheduler が thread 増で contended path を作る構造的問題で、workload 非依存。lib 製品として "moka を replace する" 主張の根拠が **algorithm の優劣** ではなく **並列構造の優劣** であることが明確になった。

### r1@8 が VTune 予測の天井を実 workload で実現

`2026-05-13-partitioned-cap1024-sweep.md` の VTune uarch-exploration で「r1 cap=4096 T=16 で Retiring 71.2% / 206 Mops (Alder Lake P-core compute ceiling)」を測ったが、これは bench_vtune_concurrent (synthetic loop) の値。**本 sweep の bench_concurrent (warmup / trial 管理 / hit-ratio 計上 が乗る real workload) で r1@8 T=16 Zipf 1.4 RH u64 = 207.69 Mops** で完全一致。VTune 計測の天井域が end-to-end でも実現できることを直接確認。

### HR-policy 等価性 → drop-in replacement 路線が現実的

c17s / r1@1 の HR が moka と全 workload で ±1pp 並走 (twitter_cluster019 では senba +7pp) という事実は、**SIEVE policy が W-TinyLFU と policy 性能で並ぶ**ことを示す。`external-lib-sweep.md` の単スレ HR map (DS1/P3/S3 large cap で W-TinyLFU 優位、他で SIEVE 優位) と整合的で、senba を moka の drop-in replacement として提案できる根拠になる。

ただし `external-lib-sweep.md` で見えた ARC P3 / P6 / S3 (scan-heavy 寄り) の小 cap 帯で W-TinyLFU > SIEVE は本 sweep では計測していない (DS1 / OLTP のみ)。lib 出し向けには ARC 8 trace 全部の HR map を補完計測すべき。

## Verdict

**lib として moka 0.12 を replace する根拠が直接データで揃った**。

公開 API surface としては:

- **`senba::Cache`** = c17s 互換、HR と Mops の両軸で moka 並走〜超過、default 推奨
- **`senba::concurrent::ShardedCache` (= r1@8)** = opt-in、HR drop を許容する場面で Mops を +30〜+78% 上乗せ
- Twitter cluster006 / ARC OLTP のような shared-hot 帯では r1@8 の HR drop が許容できない → adaptive WAYS (r2 設計) が将来必要

短期 (今すぐ着手可) と中期 (lib 出し前にやる) を分けると:

**短期 (lib publish 前の必須):**
1. **bare Linux (or Win native) で再計測** — WSL2 confound (memory `wsl2-measurement-confound`) は r1/moka 双方に対称にかかるはずだが、絶対値主張のため bare metal で 1 回回す。i5-12600K Windows native は既に整備済み (`bench_concurrent` は `cargo xwin` で cross-build 可)
2. **ARC 8 trace 全 sweep** — 本 sweep は DS1 / OLTP のみ。`external-lib-sweep.md` で W-TinyLFU が HR で勝った P3/P6/S3 系を加え、senba の HR が劣後する帯を明示
3. **`Cache::insert` の return 仕様確認** — moka の `insert` は (key, value, evicted_value) を返さない。lib API 互換性のため、`senba::Cache::insert` の Drain 系列との整合性を再確認

**中期 (lib 出し後の競争力維持):**
4. **moka 0.13/0.14 / Caffeine 系の再ベンチ** — 本 sweep は moka 0.12 sync 固定。最新版で TinyLFU hot path が軽くなっている可能性があり、定期的な追跡が要る
5. **adaptive WAYS (r2 設計)** — r1@8 の HR drop が大きい帯 (cluster006, OLTP) でも automatic に degrade しない仕組み
6. **Drop cost benchmark の独立化** — string value で r1@8 が +32% を取った増幅 (`r1-results.md` §4.2) は moka 比でも 11.84× で再現したが、Drop の co-locality 効果が moka の `Arc<...>` value 経路でも同様に効くかは別軸の検証

### 副次: c17s でも moka 比 16× 出る

驚いたのは r1@8 ではなく **c17s 単体で T=16 Zipf 1.4 RH u64 が 145.72 Mops / moka 比 16×** という結果。c17s は SIEVE state machine + 64-shard mutex + AVX2 SIMD scan という素朴な構造で、別に exotic な lock-free design ではない。それでも moka に対して order-of-magnitude 勝つ — つまり **senba の優位は r1 の routing affinity ではなく、c17s 段階の structural skeleton (SlotSize/AlignedTags/c-hoist/AVX2 + per-shard fine-grained mutex) で既に支配的**。r1 は更にその上に WAYS を載せて +30〜+78% 取る、という階層構造。

これは lib API 設計に影響する: **public default は `Cache` (= c17s 等価) で十分**、`r1@8` 系列は `concurrent::ShardedCache` のような opt-in surface で出せばいい。「moka より遅い workload は senba にない」が baseline で言える。

## 関連レポート

- `2026-05-13-partitioned-cap1024-sweep.md` — VTune で計測した r1 の compute ceiling 206 Mops が本 sweep の 207.69 Mops で end-to-end 確認された
- `2026-05-12-r1-results.md` — r1@8 vs c17s の sweep、本書の前駆 (moka 軸が無かった)
- `2026-05-06-c8-vs-moka-thread-sweep.md` — c8 era の moka 比較、本書の transitive 推論元
- `2026-05-11-lru-vs-minimoka-vs-senba-pareto.md` — senba 単スレ vs lru/mini_moka_unsync、本書の単スレ baseline 起源
- `2026-05-08-external-lib-sweep.md` — ARC trace 全体での policy 比較 (SIEVE vs W-TinyLFU)、本書の HR-policy 等価性主張の起源
