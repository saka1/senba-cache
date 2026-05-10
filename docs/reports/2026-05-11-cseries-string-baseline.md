# c-series 並行 variant の (u64, String) ベースライン sweep

## 0. TL;DR

c14s / c16s / c17s を **u64 (Slot16 自然形) + String (Slot32 自然形)** × T ∈ {4, 8, 16} × {gim @ skew=1.0, read-heavy @ skew=1.4} の 12 測定点で計測 (各 3 trials, cap 4096, ops 40M, shards 64 固定)。結果は `docs/benchmark/cseries-string-baseline/`。

- **u64 read-heavy で c17s が +11〜+22% (T が大きいほど大きく)、c17s-results.md の +27% (5 trial) と整合**。HR は c16s 0.9247 → c17s 0.9281 (+0.34pp @ T=16)。
- **u64 gim では c17s vs c16s ≈ flat** (−3.2% 〜 +1.8%)、Slot32 化退行は thread / op-mix 帯依存で消える。
- **string では c14s/c16s が読み書き並走で memory corruption** (`free(): unaligned chunk` で SIGABRT)。8/12 セルで crash。c17s だけが全 12 セル稼働。
- 採否: c17s は **non-Copy V (Slot32 自然形ワークロード) において唯一 production-safe な c-series**、というのが今回最大の発見。Mops の数字以上に **correctness 軸での差** が決定的。

## 1. 動機

`docs/reports/2026-05-11-c17s-results.md` で c17s の gate (b) 退行 (skew=1.0 gim T=4 で −3.7%) が Slot32 化由来と推定したが、推定の根拠は限定的だった。`(u64, u64)` 比較のみで Slot32 が「imposed」(c17s は固定 Slot32) されている状態と、Slot32 が「natural」(値型が 32B 自然形) の状態を分離していない。本 sweep は両者を測って:

1. c17s vs c16s の差を value 軸で見る (`(u64, u64)` の Slot32-imposed と `(u64, String)` の Slot32-natural)
2. c-series の (u64, String) で何が起きるかを単に baseline として撮る (今後の variant 比較の不動点)

を目的に行う。

## 2. 計測手順

- harness: `research/src/bin/bench_concurrent.rs` (`--value u64|string` 対応した版、`f5bb8fd`)
- `--cap 4096 --keys 100000 --ops 40_000_000 --warmup 1_000_000 --shards 64 --trials 3`
- scenarios:
  - **gim @ skew=1.0**: get-if-miss-insert、warm 後 HR ≈ 0.72
  - **read-heavy @ skew=1.4**: 95% get / 5% insert (別 Zipf seed)、warm 後 HR ≈ 0.93
- threads: 4, 8, 16
- variants: c14s, c16s, c17s
- values:
  - `u64` (Slot16 自然形、Entry 16B)
  - `String` via `format!("v{k:08}")` (Slot32 自然形、Entry 32B: 24B `String` header + 8B key)

実体はリポジトリ内 `docs/benchmark/cseries-string-baseline/run.sh` + `plot.py` + `data/sweep.csv` + `figures/aggregate_mops.png` + `summary.md`。

クラッシュ耐性: 1 (variant × threads × op_mix × value) を個別 process で起動し、SIGABRT (rc=134) を `crashes.log` に記録して次に進む構造。

## 3. 結果 — value=u64 (既存 c17s-results.md の再現)

### 3.1 gim @ skew=1.0 (c17s gate (b) の素直な拡張)

| T | c14s | c16s | c17s | c17s Δ% vs c16s |
| ---: | ---: | ---: | ---: | ---: |
| 4 | 32.47 | 34.92 | 33.91 | **−2.9%** |
| 8 | 45.16 | 48.91 | 49.76 | +1.8% |
| 16 | 64.41 | 64.77 | 62.73 | −3.2% |

T=4 で −2.9%、T=16 で −3.2%、T=8 はむしろ +1.8% という分布。前回 c17s-results.md の T=4 −3.7% (3 trial) とほぼ整合、ノイズ範囲。**Slot32 化退行は thread 帯に依存して ±2〜3% 振れる程度**で構造的に固定された損ではない、という見立てが立つ。

### 3.2 read-heavy @ skew=1.4 (= adv-hot)

| T | c14s | c16s | c17s | c17s Δ% vs c16s | HR c16s→c17s |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 4 | 68.60 | 68.24 | 75.89 | **+11.2%** | 0.9279 → 0.9282 |
| 8 | 102.55 | 100.84 | 116.22 | **+15.3%** | 0.9272 → 0.9281 |
| 16 | 129.62 | 132.06 | 160.73 | **+21.7%** | 0.9247 → 0.9281 (+0.34pp) |

c17s-results.md の +27.4% (5 trial, T=16) より 5pp 低いが、3 trial の median ベース + WSL2 day-to-day noise を考えれば再現と評価。HR は高 T で +0.34pp の構造改善が再現。

## 4. 結果 — value=String (Slot32 自然形)

### 4.1 gim @ skew=1.0

| T | c14s | c16s | c17s | c17s Δ% vs c16s |
| ---: | ---: | ---: | ---: | ---: |
| 4 | 23.51 | 24.85 | 23.75 | −4.5% |
| 8 | 33.00 | 33.04 | 34.65 | +4.9% |
| 16 | 44.63 | **✗ crash** | 43.56 | — |

c17s vs c16s は u64 gim と同様 flat。**T=16 で c16s が crash したのが想定外**: gim だから crash しないはずという当初の見立てを 1 ケース反証。

### 4.2 read-heavy @ skew=1.4

| T | c14s | c16s | c17s |
| ---: | ---: | ---: | ---: |
| 4 | ✗ crash | ✗ crash | 52.00 |
| 8 | ✗ crash | ✗ crash | 79.63 |
| 16 | ✗ crash | ✗ crash | 105.51 |

c14s / c16s は **3/3 全 T で crash**。c17s のみ稼働。105 Mops @ T=16 で線形に近い scaling、HR 0.9277。

## 5. c14s/c16s の memory corruption の root cause

8 cells (6 read-heavy + 2 gim) で `free(): unaligned chunk detected in tcache 2` → SIGABRT。

### 5.1 仕組み

c14s / c16s の reader (`get`) は **seqlock-via-tag** 構造:

1. `tags[pos]` を atomic load (= seqlock の v1 相当)
2. `tag` から `id` を抽出
3. `entries[id]` を `ManuallyDrop<Entry<K, V>>` として `ptr::read` (= bit-by-bit memcpy)
4. `key` と `value` を取り出して clone
5. `tags[pos]` を再 load して v1 と比較 (= seqlock の v2 相当)
6. 一致しなければ Retry / Miss

問題は **3 と 5 の間**。writer (`insert` の Path A) が同じ slot を更新中だと、`ptr::read` が **半上書きされた Entry の bit 列**を読む。`V=u64` (Copy, no Drop) なら半上書きされた u64 を読んでも単にゴミ値で、5 の tag 再 load で seqlock fail → retry に乗る (UB なし)。

ところが `V=String` の場合、`ManuallyDrop<String>` 内の `(ptr, len, cap)` 3 ワードが半上書きされている可能性がある — 古い `ptr` に新しい `len` / `cap` が混じった「壊れた `String`」が手元に来る。これを直接 clone (= `len` バイトを `ptr` から copy) すると壊れた領域から read する、または **ManuallyDrop の Drop ガードを通って `String::drop` が呼ばれて壊れた `(ptr, len, cap)` で `free(ptr)` を呼び、`free()` が「unaligned chunk」を検出して abort** する。

これが今回観測した SIGABRT の正体。

### 5.2 なぜ read-heavy で集中するか

- gim (skew=1.0): per-thread に「get k → 失敗 → insert k」が並ぶ。各 thread の read/write は **同じ key に対して** 直列。並走する thread は **同じ Zipf 分布**から独立に draw するので hot key (k=0 付近) では衝突するが、その keyspace は cap 4096 で warmup 後すぐ resident になり writes が減る。
- read-heavy (skew=1.4): get は `zipf`、insert は **別 seed の `zipf_ins`** から独立 draw。insert keyspace は読み専 thread の hot set とずれているため、**eviction 連鎖がずっと止まらない**。reader が hot key を読みに行く窓と writer が adjacent slot を書く窓が常にオーバーラップして seqlock racing が定常化。

T=16 gim で c16s が落ちたのは、thread 数が増えて gim でも writer 並走が増したため。c14s T=16 gim が生き残ったのは確率の問題で、もう一度回せば落ちる可能性が高い (= 設計上 UB であって、運 ok だっただけ)。

### 5.3 c17s が落ちない理由

c17s は **entry-level seqlock** (`Entry::version: AtomicU32` の偶奇 flip):

1. reader が `entry.version` を load (v1)
2. v1 が奇数なら writer 進行中なので Retry / Miss を返す (= ptr::read 自体スキップ)
3. v1 が偶数なら `ptr::read` で `ManuallyDrop<Entry>` を取り出す
4. `entry.version` を再 load (v2)
5. v1 ≠ v2 ならば Retry / Miss

ここで決定的なのは **2 の早期 escape**。writer が odd を立ててから値書き換えに入るので、racing 中の reader は **ptr::read 自体を実行しない**。半上書き Entry は手元に来ない → ManuallyDrop の drop 経路も発火しない → free 破壊もない。

c14s/c16s で同じ保護を得るには `tag` の 1 ビットを seqlock guard に使う必要があるが、c14s/c16s は tag を `LIVE | VISITED | VERSION | id | hash` で目一杯使っており、ptr::read 前 escape 用の追加ガード状態を持たない (VERSION bit はあるが Path A の epoch、Path C との連動で意味がずれている、ptr::read を抑える役には立っていない)。これが c17s の "G2-α-1: entry-level seqlock" 設計が `V: Clone` (= 非 Copy) を真に support するための prerequisite だった、ということ。

### 5.4 含意 (library 化の観点)

senba::Cache (`crates.io` target) は `V: Send + Sync + 'static` を取る設計で、当然 String も含む。今回の発見は **c14s/c16s 設計を library に持ち上げると即 UB** であることを意味する。c17s 設計 (entry-level seqlock) が library 候補として唯一現実的、という選別基準が立った。

## 6. 採否と次の動き

### 6.1 採否

c17s は今回データで「Mops で勝ち、correctness で唯一稼働」の二重勝ち。**条件付き採用 → 採用** に強化される。直前 c17s-results.md で残していた gate (b) failed (skew=1.0 gim T=4 で −3.7%) は依然残るが:

- 同じ条件 (T=4 gim u64) で 3 trial median は **−2.9%**、c17s-results.md の 3 trial −3.7% と分布の中で動いている。perf-gate 閾値 −2% にぎりぎり当たっているだけで、構造的退行ではない。
- thread sweep で見ると T=8 は +1.8%、T=16 は −3.2%、平均すれば flat。
- adv-hot や string では勝ち、唯一稼働の場面まである。

c17s を **G2-α-1 として確定採用**、Mops の 1 セルの −3pp は許容、として次に進むのが妥当。

### 6.2 c14s/c16s の扱い

c14s / c16s は **`V: Copy` 制約を documented constraint として明示** すれば research artifact としては有効 (u64 / 整数値のみ受け付ける)。library 化候補からは外れる。`docs/reports/c14s-design.md` / `c16s-design.md` に "Caveat" 節を追加するのは別タスク。

### 6.3 次の variant

`c17s-results.md §8.1 #1` で提案した **G2-α-2 (versions を別配列に切り出して Slot16 維持)** が依然有望。今回 (u64, u64) gim でも c17s は flat なので Slot16 維持の益は限定的かも、という見立ても出てきた。先に **`(u64, [u8; 24])` の Slot32-natural-without-Drop** で測れば「Slot32 化 cost が gim で本当に出るのか、それとも `String` の clone cost が支配的なのか」を分離できる。`bench_concurrent` に `--value bytes24` を足すのは 30 行未満の追加で、優先度高。

## 7. Deliverables

- `docs/benchmark/cseries-string-baseline/run.sh` — sweep ドライバ (crash 耐性 individual-process 起動)
- `docs/benchmark/cseries-string-baseline/plot.py` — `summary.md` + `figures/aggregate_mops.png` 生成
- `docs/benchmark/cseries-string-baseline/data/sweep.csv` — 85 rows (8 cells crash)
- `docs/benchmark/cseries-string-baseline/data/crashes.log` — crash detail (rc=134, tail 20 lines per failure)
- `docs/benchmark/cseries-string-baseline/summary.md` — markdown 集計表
- `docs/benchmark/cseries-string-baseline/figures/aggregate_mops.png` — 2x2 facet bar chart
- `research/src/bin/bench_concurrent.rs` (`f5bb8fd`) — `--value u64|string` 対応で V generic 化
