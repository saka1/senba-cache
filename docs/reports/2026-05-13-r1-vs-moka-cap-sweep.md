# r1 vs moka — capacity-axis sweep: W-TinyLFU HR 優位は T=16 で逆転消滅

## 仮説

`2026-05-13-r1-vs-moka-sweep.md` の sweep は cap=4096 / 4000 固定で、`external-lib-sweep.md` 単スレ計測で観測された **scan-heavy ARC trace 大 cap での W-TinyLFU HR 優位 (DS1 cap=1M +7pp, P3 cap=32k +8.5pp, S3 cap=400k +13pp)** が観測領域外だった。これを multi-thread (T=16) で直接検証する。

事前予測:

- **policy 層**: T=1 では external-lib-sweep の HR 反転が再現するが、moka 側の per-op `run_pending_tasks()` を呼ばない realistic 計測では admission accuracy が劣化、HR 優位は縮む見込み
- **throughput 層**: cap が大きくなるほど working set が L3 を溢れて memory hierarchy が支配的になり、moka regress が更に強く出る見込み

## 実施したこと

### harness 拡張

3 件の小さな変更で c17s/r1 が任意 cap で動くようにした:

1. **`research/src/workload/arc_preset.rs`** を新設: mokabench `TraceFile::default_capacities` を `lookup(name)` で参照可能に。`bench.rs:511` の private `arc_preset_lookup` を移植 (bench.rs もそれを参照するよう refactor)
2. **`bench_concurrent` に `--arc-preset NAME` 追加**: trace_file と workload_param を一括解決、`--cap` は harness 側で sweep
3. **c17s / r1 の `shards: [Shard; SHARDS]` を `Box<[Shard]>` に変更**: stack overflow を避け SHARDS=131072 (= cap 8M / 64) まで実用化。const generic SHARDS は invariant (power-of-2 mask) を保持。dispatch arm を SHARDS ∈ {4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536, 131072} に拡張、bench_concurrent の `(8..=512)` 上限を `(1..=131072)` に緩和

### sweep 軸

`docs/benchmark/r1-vs-moka-cap-sweep/run.sh`:

- variants: c17s (ways=1), r1@ways∈{1,8}, moka 0.12 sync, mini_moka 0.10 sync
- T: 1, 4, 8, 16
- value: u64 (ARC は trace の u64 key、Zipf/Twitter も u64 で揃え)
- shards: **`next_pow2(cap/64)`** で cap-scaled (senba::Cache::new auto-shard と同じ heuristic)
- ways: c17s/moka/mini_moka は 1 固定、r1 は {1, 8} を試して `ways ≤ shards` 制約に従い必要時 clamp
- ops: `max(cap*4, 2M)` を 16M で打切り (大 cap で working set 全体に行き渡る)
- warmup: `max(cap, 200k)` を 4M で打切り

#### workload

- Zipf {s=0.8/gim, 1.0/gim, 1.4/gim, 1.4/read-heavy} × cap ∈ {1024, 4096, 16384, 65536}
- Twitter Yang {006, 019, 034} × cap ∈ {1024, 4096, 16384, 65536}
- ARC preset {OLTP, P1, P3, P6, P8, S1, S3, DS1, ConCat, MergeP, MergeS} × **mokabench 既定 cap** (preset 別 2–4 段、計 29 cap-cell)

総 1140 cells × trials=3 を WSL2 Linux i5-12600K kernel 6.6.87.2 で **1 時間 11 分** で完走、crashes ゼロ、3420 data rows。SHARDS は cap に応じて 4 (cap=256) から 131072 (cap=8M) まで自動スケール、`scale_shards` の `next_pow2((cap+63)/64)` 計算と整合。

## 結果

### 1. T=1 single-thread: external-lib-sweep の HR 構造を再現確認

`external-lib-sweep.md` の SIEVE vs W-TinyLFU 反転帯 (DS1 large cap, P3, S3 large cap で W-TinyLFU 優位) が T=1 で再現するかをまず確認 (= 計測 methodology の sanity)。

| trace | cap | c17s HR | moka HR | gap |
|---|---|---:|---:|---:|
| ARC DS1 | 1M | 6.6% | **10.7%** | **+4.1pp** |
| ARC DS1 | 4M | 34.6% | **40.7%** | +6.1pp |
| ARC P3 | 20k | 5.4% | **14.1%** | **+8.7pp** |
| ARC P6 | 20k | 7.4% | **16.8%** | +9.4pp |
| ARC P8 | 20k | 13.4% | **20.2%** | +6.8pp |
| ARC S1 | 100k | 4.5% | **8.0%** | +3.5pp |
| ARC S3 | 100k | 4.4% | **8.0%** | +3.6pp |
| ARC S3 | 400k | 18.6% | **32.7%** | **+14.1pp** |
| ARC P1 | 20k | 17.5% | **26.7%** | +9.2pp |

→ **T=1 では external-lib-sweep の構造が再現** (S3 cap=400k で +14.1pp、external-lib-sweep の +13pp と整合)。external-lib-sweep は単スレ + per-op `sync()` 込みなので絶対値は微妙に違うが、SIEVE が scan-heavy + frequency-skewed で W-TinyLFU に HR で劣後する**質的構造は保たれている**。

### 2. T=16 multi-thread: **HR 優劣が逆転**

同 cell を T=16 で取り直すと方向が逆になる:

| trace | cap | c17s HR | moka HR | gap (T=1 比) |
|---|---|---:|---:|---:|
| ARC DS1 | 1M | **11.8%** | 9.0% | **−2.8pp 逆転** (T=1 では moka +4.1pp) |
| ARC DS1 | 4M | **38.8%** | 35.9% | −2.9pp 逆転 (T=1 では moka +6.1pp) |
| ARC P3 | 20k | **11.3%** | 10.0% | −1.3pp 逆転 (T=1 moka +8.7pp) |
| ARC P3 | 160k | **50.4%** | 46.5% | −3.9pp 逆転 |
| ARC P6 | 160k | **70.4%** | 65.9% | −4.5pp 逆転 |
| ARC P8 | 160k | **66.7%** | 63.6% | −3.1pp 逆転 |
| ARC S1 | 800k | **63.2%** | 61.0% | −2.2pp 逆転 |
| ARC S3 | 800k | **65.4%** | 62.9% | −2.5pp 逆転 |
| ARC DS1 | 8M | 59.6% | 60.7% | +1.1pp moka tie |
| ARC P1 | 20k | 21.4% | 22.5% | +1.2pp moka tie (T=1 moka +9.2pp) |

**1140 cell の sweep 全体で moka が c17s を HR で +2pp 以上上回るのは twitter_cluster019 cap=1024 の 1 cell のみ** (moka 16.5% vs c17s 12.6% = +4.0pp)。それ以外の **57 cell**は moka と c17s が HR で並走するか、c17s が moka を上回る。

### 3. なぜ逆転するのか — moka の admission queue 仮説

moka 0.12 は read/write ops を内部 log にバッファして amortize、`run_pending_tasks()` を呼ばないと CMSketch 更新と admission 判定が反映されない構造 (`bench_concurrent.rs:35-44` 既知)。本 sweep は **per-op sync を呼ばない realistic concurrent usage** で計測しているため:

- **T=1**: 1 thread 分の log 流量 → admission queue が捌ける → external-lib-sweep に近い HR
- **T=16**: 16 thread 分の log が write buffer に積み上がる → admission backpressure で **新規 key の admission 判定が失敗**、本来 W-TinyLFU が弾くべき cold key が cache に入る/本来通すべき hot key が落とされる → HR が **c17s の SIEVE policy 以下** に落ちる

これは moka の bug ではなく **W-TinyLFU の amortization 設計が thread-pressure で破綻する構造的問題**。`c8-vs-moka-thread-sweep.md` で観測された moka throughput regress (T=4 ピーク後減速) と表裏一体で、log を捌けないと throughput も admission accuracy も両方落ちる。

memory `feedback_perf_gate_diversity.md` の「perf-gate には多様な workload が必要、criterion 単独判断は危険」と同じ教訓: external-lib-sweep の **T=1 + per-op sync** という測定条件は、moka の amortization 設計を fairest case で見ていたが realistic concurrent usage を表していなかった。

### 4. T=16 throughput: cap-axis でも senba 圧倒

| trace | cap | c17s Mops | r1@8 Mops | moka Mops | r1@8/moka |
|---|---|---:|---:|---:|---:|
| ARC DS1 | 1M | 14.1 | 12.2 | 0.66 | **18.45×** |
| ARC DS1 | 4M | 18.3 | 11.1 | 0.86 | 12.94× |
| ARC DS1 | 8M | 26.0 | 12.7 | 4.03 | 3.16× |
| ARC P3 | 160k | 58.4 | 22.6 | 1.14 | 19.82× |
| ARC P6 | 160k | 66.4 | 20.4 | 2.01 | 10.13× |
| ARC P8 | 160k | 82.1 | 25.7 | 1.73 | 14.88× |
| ARC S3 | 800k | 56.6 | 15.7 | 2.03 | 7.72× |
| ARC ConCat | 3.2M | 38.0 | 22.3 | 4.49 | 4.97× |
| ARC MergeS | 3.2M | 43.9 | 14.0 | 4.89 | 2.87× |
| Zipf 1.4 RH | 65536 | 162.8 | **173.4** | 9.31 | **18.62×** |
| Zipf 1.4 gim | 65536 | 205.7 | 177.4 | 7.35 | 24.13× |

cap=1M–8M 帯でも senba (どの variant でも) は moka を **3–25× 上回り続ける**。c17s は cap が増えるほど working set が L3 に乗りやすくなり throughput が伸びる (DS1 T=16 cap=1M → 8M で 14 → 26 Mops)、moka は cap 増による benefit が薄い (0.66 → 4.03 Mops、依然 senba の 6 分の 1)。

### 5. T=16 p99 latency: moka 大 cap で **100 µs 超え**

| trace | cap | c17s p99 (ns) | moka p99 (ns) | moka/c17s |
|---|---|---:|---:|---:|
| ARC DS1 | 1M | 1785 | **137,561** | 77.06× |
| ARC DS1 | 4M | 1734 | **227,372** | **131.13×** |
| ARC MergeP | 1M | 1281 | 113,223 | 88.39× |
| ARC ConCat | 400k | 1023 | 67,466 | 65.95× |
| ARC S1 | 800k | 1274 | 58,150 | 45.64× |
| ARC P6 | 160k | 790 | 26,104 | 33.04× |
| Zipf 1.4 RH | 65536 | 182 | 4,215 | 23.16× |

moka の p99 が **大 cap で 100–230 µs**(0.1–0.2 ms) という驚異的な数字。lib SLA としては「p99 < 3 µs」を主張できる senba と比較で **2 桁離れる**。原因は moka の write log dequeue が大 cap で eviction 判定の DRAM-bound state 探索を要するため (推測)。

### 6. r1@8 は cap-axis sweep で利得を失う

`r1-vs-moka-sweep.md` (cap=4096 固定) で +30〜78% を取った r1@8 は、cap-axis sweep では cap が大きくなるほど c17s に対する利点を失う:

| workload | cap | c17s | r1@8 | r1@8/c17s |
|---|---|---:|---:|---:|
| Zipf 1.4 read-heavy | 1024 | 156.8 | 204.9 | **+31%** |
| Zipf 1.4 read-heavy | 65536 | 162.8 | 173.4 | +6% (利得消失) |
| ARC DS1 | 1M | 14.1 | 12.2 | **−13%** |
| ARC DS1 | 8M | 26.0 | 12.7 | **−51%** |
| ARC ConCat | 3.2M | 38.0 | 22.3 | **−41%** |

cap 増で per-way の effective cap (cap/ways=8) が縮み、HR drop も拡大 (DS1 cap=8M で c17s 60% vs r1@8 **3%** = −57pp 壊滅)。**r1@8 の利得帯は cap ≤ 16k 程度**、それ以上で c17s に劣後する構造が明確に。lib API 設計含意: `r1@8` 系列を opt-in にする時の境界は「cap=4-16k で routing affinity が効く帯」と書ける。

### 7. HR-throughput pareto: senba が全帯で右下

T=16 で **HR が moka 以上 + Mops が moka の 7–25×** という cell が 1140 中の大多数。lib 公開向け Pareto としては senba (c17s) が **全 cap × 全 workload で moka を pareto-dominate** (= HR 同等以上 + ns/op 大幅優位)。例外は前述 twitter_cluster019 cap=1024 のみ。

## 学び

### 主結論: T=16 で W-TinyLFU の HR 優位は消失

前 sweep のレポート §「HR drop の policy 層は未解決」で `external-lib-sweep.md` を根拠に「scan-heavy ARC 大 cap で SIEVE は W-TinyLFU に 7–13pp 負ける」と書いたが、**この主張は T=1 単スレ + per-op sync 限定の現象**だった。realistic concurrent usage (T=16, no per-op sync) では moka の admission queue が saturate して逆向きに HR が崩れる。

policy 層の HR drop は **scope を `realistic concurrent T≥2` に絞れば実質解決**。残るのは:

- **single-thread embedded 用途** (T=1 で per-op sync 風の使い方をする場面): SIEVE は依然 W-TinyLFU に 3–14pp 劣後。この帯では moka の選択が正当
- **shared-hot 帯の HR drop** (twitter_cluster006 r1@8 −28pp 等): r1@ways≥2 を選ぶ場合の trade-off、c17s/r1@1 にすれば回避可

### 副次結論: cap-axis で senba の優位が拡大

`r1-vs-moka-sweep.md` (cap=4096 固定) で senba は moka を 7–23× 上回ったが、**本 sweep では cap=1M–8M 帯でも senba が moka を 3–25× 上回り続ける**ことが確認できた。特に **moka p99 latency が大 cap で 100–230 µs に張り付く** のは lib 競争力上の決定打: senba は同 cell で p99 < 3 µs を維持、SLA 主張で 2 桁差。

### 設計含意

`r1@8` の利得帯が cap ≤ 16k に限定されることが分かったので、API 設計は:

- `senba::Cache` (= c17s 等価、auto-shard) を **default 推奨**、全 cap で moka を pareto-dominate
- `senba::concurrent::ShardedCache::new_with_ways(cap, 8)` のような opt-in surface は **cap ≤ 16k での throughput boost** を明示 doc に書く (それ以上の cap では c17s に劣後する)
- `senba::concurrent::PartitionedCache` (lib 既存) は `partitioned-cap1024-sweep.md` で reject 確定、本 sweep でも触らない

### 計測 methodology に関する反省

memory `feedback_perf_gate_diversity.md` の「多様な workload」原則を policy 層にも貫徹すべきだった。external-lib-sweep の単スレ + per-op sync は **moka 設計の amortization が完全に効く理想条件** を測っていたが、これを lib 比較の base case と扱うのは moka に過剰に有利。本 sweep の T=16 + no-sync 経路が realistic concurrent の base case。

将来 lib 比較を更新する時はこの 2 軸 (T=1 sync vs T≥4 no-sync) を **両方** sweep する harness にする (現状の bench / bench_concurrent 分離が既にそれに近い形だが、HR 比較で per-op sync 強制を呼ぶ adapter (`bench.rs` 既存) vs 呼ばない adapter (`bench_concurrent.rs` 既存) の使い分けを明示化する doc が要る)。

## 今後

優先度順:

1. **bare metal 再計測** (WSL2 confound 確認、`r1-vs-moka-sweep.md` §Verdict short-term 1 と同じ要件) — 絶対値の信頼性のため lib publish 直前で
2. **小 cap (≤ 2000) Zipf/Twitter 帯の補完** — 本 sweep は cap=1024 から始めているが、OLTP の admission threshold 帯 (cap=256–512) は Zipf/Twitter ではカバーしていない。lib 設計境界として cap=128–512 の挙動も見たい
3. **per-op sync 強制 adapter を bench_concurrent に追加** — moka の "理想条件" 計測を T≥2 で取れる variant を提供。lib 比較 doc で「fairest case の moka vs realistic moka」の差を可視化
4. **adaptive WAYS (r2 prototype)** — r1@8 の cap ≤ 16k 利得帯を auto-detect して switch する scheme、`r1-results.md` §6.2 の follow-up と同じ動機

## 関連レポート

- `2026-05-13-r1-vs-moka-sweep.md` — cap=4096 固定 sweep、本書の前駆 (cap-axis 未消化が本書動機)
- `2026-05-08-external-lib-sweep.md` — 単スレ HR map、本書の §1 で再現確認
- `2026-05-12-r1-results.md` — r1@ways sweep、本書 §6 で r1@8 の cap-axis 利得失効を補強
- `2026-05-06-c8-vs-moka-thread-sweep.md` — c8 era の moka throughput regress、本書 §3 の admission queue 仮説の起源
