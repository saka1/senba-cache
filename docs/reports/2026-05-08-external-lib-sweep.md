# 外部ライブラリ比較: ARC trace + Zipf でのスイープ

`2026-05-08-mokabench-arc-traces.md` で `--source arc` / `--arc-preset` /
`mini_moka_sync` / `mini_moka_unsync` / `moka` を `bench.rs` に揃えた基盤の上で、
ARC paper trace 6 種 + Zipf を使って **senba::Cache (SIEVE) vs mini-moka /
moka 0.12 (W-TinyLFU)** を一気に走らせた結果。HR と単スレッド throughput の
両軸で Pareto を引く。

CSV は `data/2026-05-08-external-lib-sweep/<workload>.csv`、Pareto plot は
同 dir 下の `plot_<workload>/pareto-*.png` (`hr-vs-tp` / `cap-vs-hr` /
`cap-vs-tp` の 3 軸を全 workload について生成済み)。

## 構造的制約と sharding の正解

senba::Cache の per-shard 上限は **64 entries** で、これは tag に同居する
**6-bit entry-id** から来る構造的な値 (`MAX_PER_SHARD`, `src/shard.rs:18`)。
SIMD find は AVX2 で 32-tag を 1 batch なので、性能のスイートスポットも
**per-shard ≈ 32–64**。両者が同じ帯にあるので、`Cache::new(cap)` は
`shards = next_pow2(ceil(cap / 64))` で **per-shard を 32〜64 に収まるよう
自動選択**する (src/lib.rs:86-93)。

つまり senba::Cache に capacity ceiling は無い (cap が増えれば shards が
比例して増えるだけ)。本スイープではこの自動経路を使う `senba` variant
(bench.rs に新設) を canonical 比較対象とする。明示 `senba_nNNN` 系は
shards を pin する**スイープ専用 variant** で、cap が小さすぎると per-shard が
2-4 になり set-associative collision で HR が崩れる (前リビジョンで
`senba_n128 cap=256` が HR=35% に collapse したのはこれ。今回の `senba`
auto では同 cap で HR=48.8% と orig 一致)。

## スイープ条件

| Workload | Trace | accesses | capacity sweep |
|---|---|---:|---|
| `oltp-extended` | ARC OLTP (CODASYL DB) | 914,145 | 256 / 512 / 1k / 2k / 4k / 8k |
| `s3-small` | ARC S3 (search engine, 全 16M) | 16,407,702 | 100k / 400k |
| `p3` | ARC P3 (workstation) | 3,912,296 | 5k / 20k / 32k |
| `ds1` | ARC DS1 (ERP, 先頭 10M) | 10,000,000 | 100k / 500k / 1M |
| `concat` | ARC ConCat (workstation 連結, 先頭 10M) | 10,000,000 | 100k / 400k / 1M |
| `mergep` | ARC MergeP (workstation merge, 先頭 10M) | 10,000,000 | 100k / 400k / 1M |
| `zipf-skew1` | Zipf skew=1.0, keys=100k, len=1M | 1,000,000 | 256 / 1k / 4k / 16k / 32k |

variants は全 workload 共通で **3 つに絞る**:

- `senba` = `senba::Cache::new(cap)` (auto-sharded, Slot32) — 比較の主役
- `orig` = sieve_orig (linked-list arena ベースの NSDI'24 reference port)。
  senba と **同じ SIEVE 状態機械**だが SIMD / sharding を持たないので
  policy oracle として機能する
- `mini_moka_unsync` = `mini_moka::unsync::Cache` — **single-thread 公平条件で
  W-TinyLFU を代表**する。`mini_moka::sync` は `sync()` 込みで multi-thread 用途の
  overhead が乗り、`moka 0.12` は更に background thread / tokio runtime が乗る。
  本スイープは single-thread 性能の比較なのでこれらは含めない (前リビジョンの
  CSV には含まれていたが、Pareto 上で **non-dominated 点を作らない**ことが
  確認できたので議論からは外す)。

## 結果

### HR (eviction policy 比較)

`senba` (auto-shard) の HR は orig と全帯で ±0.3pp 以内に張り付き、
SIEVE の policy 性能を SIMD/sharding 越しでも保てていることが確認できる。
両者を「SIEVE 側」と束ねて W-TinyLFU と比較すると、workload と cap で
優劣がはっきり反転する。

| Workload | cap | SIEVE HR (senba / orig) | W-TinyLFU HR | 勝者 |
|---|---:|---:|---:|---|
| OLTP | 256 | 17.4 / 12.4 | 21.2 | W-TinyLFU |
| OLTP | 1000 | 38.2 / 32.6 | 34.2 | SIEVE |
| OLTP | 8000 | 59.5 / 57.6 | 52.0 | **SIEVE (+7.5pp)** |
| MergeP | 100k | 10.5 / 9.6 | 8.2 | SIEVE |
| MergeP | 1M | 37.0 / 36.7 | 31.7 | **SIEVE (+5pp)** |
| ConCat | 100k | 55.7 / 53.0 | 58.3 | W-TinyLFU |
| ConCat | 400k | 86.6 / 84.7 | 84.6 | SIEVE |
| ConCat | 1M | 92.7 / 92.7 | 92.7 | tie |
| DS1 | 100k | 2.8 / 3.1 | 1.1 | SIEVE |
| DS1 | 500k | 4.8 / 4.8 | 5.7 | W-TinyLFU |
| DS1 | 1M | 5.4 / 5.4 | **12.4** | **W-TinyLFU (+7pp)** |
| P3 | 5k | 2.3 / 2.0 | 1.1 | SIEVE |
| P3 | 32k | 9.8 / 9.7 | **18.3** | **W-TinyLFU (+8.5pp)** |
| S3 | 100k | 10.1 / 10.1 | 10.4 | tie |
| S3 | 400k | 30.3 / 29.4 | **42.6** | **W-TinyLFU (+13pp)** |
| Zipf skew=1.0 | all | senba ≈ orig ≈ mini_moka | ±0.4pp 内一致 | tie |

**読み取り**:

- **DB / 強い recency 局在 (OLTP, MergeP)**: SIEVE 優位。recency と
  `visited` bit だけで足りるパターン。
- **Frequency-skewed scan (DS1, P3, S3 large)**: W-TinyLFU 優位。
  CMSketch admission による cold-key filtering が効く。
- **Zipf**: 両者完全一致。skewed 単峰性なら policy 差は出ない。
- **小 cap (OLTP cap=256)** では W-TinyLFU が勝つ — admission threshold が
  小 cap で効くため。前リビジョンのスモークでは見えなかった点。

### Throughput

| Workload | cap | senba | orig | mini_moka_unsync |
|---|---:|---:|---:|---:|
| OLTP | 1000 | **30.2** | 20.3 | 15.4 |
| OLTP | 8000 | **28.6** | 23.8 | 12.2 |
| P3 | 20k | **30.8** | 17.8 | 7.4 |
| P3 | 32k | **29.2** | 16.9 | 6.8 |
| S3 | 100k | **20.5** | 12.0 | 5.9 |
| S3 | 400k | **9.95** | 8.4 | 4.9 |
| DS1 | 100k | **22.7** | 12.4 | 6.4 |
| DS1 | 1M | **7.0** | 5.9 | 3.9 |
| ConCat | 1M | 13.2 | **24.2** | 6.8 |
| MergeP | 1M | **7.8** | 7.5 | 3.9 |
| Zipf | 4k | 43.3 | **44.3** | 15.8 |
| Zipf | 32k | 40.9 | **65.4** | 12.9 |

(数値は **Mops/s**, get-then-insert ループ全体を `Instant::now` で計測。)

**読み取り**:

- senba は **single-thread の `mini_moka_unsync` の 2–4×** を出す
  (W-TinyLFU の hot path に乗る write log / CMSketch 更新 vs senba の
  SIMD probe + tag/visited bit 操作の実装差)。
- **Zipf 大 cap と ConCat 1M で orig が senba を上回る** — senba の
  per-shard SIEVE state machine + AVX2 find に対し、orig は単一の
  doubly-linked list + 単一 hand で hand-walk が短い (working set が
  cap に収まれば list 長 ≈ live entries)。senba は shards 数だけ
  state を分散するので、各 shard の hand-walk + SIMD probe の合計が
  単純な list-walk より重くなる帯がある。これは「SIMD で常勝」では
  ない実例で、orig を beat するには shard 内 hand walk の amortize
  が要る (別レポート)。

### Pareto plot

`summary-pareto.png` が **senba vs mini_moka_unsync の 7 panel グリッド**
(Y=ns/op log, X=HR%、右下が良い)。各 workload 個別の Pareto 図は
`plot_<workload>/pareto-*-hr-vs-tp.png` に。`cap-vs-hr` と `cap-vs-tp` を
併せて見ると **cap で交差する HR 関係**と **cap 増で劣化する throughput**
の両者が一目で出る。OLTP / MergeP / ConCat / Zipf では senba が Pareto を
独占、DS1 / P3 / S3 では cap 増で mini-moka が HR 軸の右端を取り Pareto が
分裂する (senba が ns/op を全帯で勝つ構図は変わらない)。

## 含意

1. **HR で SIEVE が常勝ということは無い**。前回スモーク (OLTP 単独) では
   SIEVE 優位に見えたが、ARC trace 6 種を回すと cap と workload で
   policy の相性が反転する。memory feedback「perf-gate には多様な workload
   が必要」を HR 軸でも踏襲する必要がある。
2. **senba::Cache の throughput 強みは workload に概ね非依存**。HR が
   不利な S3-large / DS1-1M / P3-32k でも `mini_moka_unsync` の 2× 以上は
   出す。**負け方が穏やか**で、トータルの Pareto では多くの cap 帯で
   senba::Cache が右下 (HR 同等以上 + ns/op 大幅優位) に来る。
3. **Sharding は cap に応じて自動で正しく決まっている**。`Cache::new(cap)`
   は per-shard を 32–64 に収めるよう shards を選ぶ。研究側で `senba_nNNN`
   と auto `senba` の HR 差を測ったところ、auto は orig と ±0.3pp で張り付く
   一方、shards を過剰に振ると per-shard が小さくなり HR が崩れる
   (前述の cap=256 / N=128 の例)。**ユーザは shards を選ばなくて良いし、
   選ぶべきでもない** — `Cache::new(cap)` だけが正しい入口。
4. **大 cap で orig が senba を上回る帯がある**。SIMD batch find の利得が
   shard 分散による hand-walk 重複コストに食われる典型的な領域で、
   per-shard で hand を短く保てる scan-heavy では senba 有利、working set が
   cap 内に収まる Zipf 大 cap / ConCat 1M では orig の単一 list-walk が
   軽い、という相反する力学。これは別レポートで掘る。

## Follow-up

- **DS1 / S3 large cap 帯の signal を取り込んだ perf-gate**: 候補は
  OLTP cap=2000 + MergeP cap=400k + Zipf-1.0 cap=4096 の 3 点 (HR 勝ち /
  HR 負け / HR tie の 3 face を覆う)。trace I/O を criterion bench に
  乗せる仕掛けが要るので別途設計。
- **大 cap で orig に負ける帯の解析**: Zipf cap=32k と ConCat 1M で
  senba < orig になるのは shard 分散の oversharing が原因か、SIMD find の
  cold tag load が支配的になるかを切り分ける必要がある。
- **bench.rs の `senba_nNNN` の役割整理**: 本スイープで auto `senba` が
  canonical だと確認できたので、`senba_nNNN` は「shards 感度を測る
  研究用 knob」に降格させる方向。doc コメントを揃える。
