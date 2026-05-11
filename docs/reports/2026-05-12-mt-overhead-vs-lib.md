# 2026-05-12 — MT overhead vs single-thread lib: c17s が払う構造的コストの定量化

- 種別: **観測 + 解釈ノート**。`docs/reports/2026-05-12-r1-results.md` の sweep 中に発覚した
  「c17s/r1 (MT 系) は T=1 で senba::Cache (lib) より構造的に大幅劣後する」事実の定量化と、
  その意味の解釈。今後の c/r-series 設計優先度の参照点として独立 report 化。
- 結論: c17s T=1 は senba::Cache (lib) 比で **read-dominant workload で ~4-5x、miss/insert
  比率の高い workload で最大 15.7x** の ns/op overhead を払う。原因は Mutex acquire ではなく
  **reader fast path で touch する複数の atomic Acquire load の累積**。これは c/r-series の
  「絶対値 ceiling」を実質的に規定しており、現在の T=16 aggregate Mops は単スレ lib の
  0.6-1.8x にとどまる。
- 関連:
  - `docs/reports/2026-05-12-r1-results.md` — 本 sweep の MT 数値 (c17s baseline + r1)
  - `docs/reports/2026-05-12-c17s-step1-len-load-removal.md` — c17s reader hot path の現状
  - `docs/reports/2026-05-10-c14s-vtune-write-contention.md` — c14s VTune による hot-line bouncing
    観測 (本 report の "Acquire load 累積" 仮説のヒント)
  - `docs/benchmark/r1-sweep/data/results.csv` — 本 report の c17s T=1 数値の出所

## 0. TL;DR

`bench` (単スレ専用 lib driver) と `bench_concurrent --threads 1` で **完全 apples-to-apples**
で取った T=1 ns/op:

| workload | senba::Cache (lib) | c17s ConcurrentSieve | overhead |
|---|---:|---:|---:|
| Zipf 0.8 gim u64 (HR 45%) | 27.8 ns/op | 129.2 ns/op | **4.65x** |
| Zipf 1.0 gim u64 (HR 72%) | 22.4 ns/op | 86.1 ns/op | 3.85x |
| Zipf 1.4 gim u64 (HR 98%) | 10.1 ns/op | 43.1 ns/op | 4.26x |
| Twitter cluster006 (HR 35%) | 32.0 ns/op | 158.5 ns/op | 4.96x |
| Twitter cluster019 (HR 32%) | 29.3 ns/op | 460.8 ns/op | **15.71x** |
| Twitter cluster034 (HR 36%) | 29.7 ns/op | 261.8 ns/op | 8.81x |
| ARC OLTP cap=4000 (HR 52%) | 28.8 ns/op | 173.3 ns/op | 6.02x |

ns/op delta は **+15〜+430 ns/op** の範囲で workload 依存。共通項として **lib の 10-30 ns/op が
c17s では 40-170 ns/op に膨らむ**。

## 1. 動機と Method

### 1.1 動機

r1-sweep results §4 で T=16 cluster019 r1 w=8 が 20.27 Mops に到達して「best cell」になった
ものの、user の問い「senba::Cache 単スレでは典型 20 Mops 出てるよね」を起点に lib と直接
突き合わせると、**「16 thread の MT cache が単スレ lib より遅い」cell が複数ある**ことが
判明 (cluster019: lib 36.5 Mops vs MT T=16 best 20.27 Mops)。

c-series / r-series は単スレ lib 相手の競争を念頭に設計してきたわけではなく、moka /
mini-moka 等の MT 系を比較対象にしてきたが、**lib 自体の絶対値を rationality 軸として
明示的に measure し直す** のが本 report の目的。

### 1.2 Method

- 単スレ lib (`senba::Cache`): `research/src/bin/bench` を release で build (`--features
  senba-research/external-traces` 込み)、`--variant senba` で `senba::Cache<u64, u64>`
  (Slot32 default, hasher = Xxh3Build) を `&mut self` API 経由で回す。`drive<C>` (line 255-)
  は `get-then-miss-insert` (= gim) パターン 1 op = 1 access の counting loop。
- MT 系 (`c17s`): 既存の r1-sweep 結果から `--variant c17s --ways 1 --threads 1 --op-mix gim`
  cell を直接抽出 (HR が semantics 等価性の sanity)。`ConcurrentSieveC17S` は `Arc<Shard>`
  内に Mutex + ShardHot (atomic visited / tag / state) を持つ並行版。
- workload: Zipf 3 skew + Twitter cluster 3 + ARC OLTP cap=4000、全て gim op-mix u64 value
  cap=4096 (ARC のみ cap=4000)。trial 数は lib 側 1、c17s 側 3 trial median (sweep の生数値)。
- HR の sanity: lib と c17s で HR が ±2pp 一致することを確認 (gim semantics が algorithm 等価)。

apples-to-apples 補強: cluster019 で 1M ops cap=4096 warmup=0 T=1 を両側で別途回し、HR が
**0.316 で完全一致** することを確認 (semantics 揃え)。

```text
$ ./target/release/bench --source twitter --path external/twitter-cache-trace/cluster019 \
    --capacity 4096 --variant senba
senba,twitter,NaN,0,1000000,4096,29302180,316021,683979,679883
$ ./target/release/bench_concurrent --variant c17s --shards 64 --cap 4096 --ops 1000000 \
    --warmup 0 --trials 1 --seed 42 --threads 1 ... --source twitter-yang ...
c17s,0,1,twitter-yang,cluster019,gim,u64,1,100000,1,4096,64,1000000,460809089,2.1701,0.3160,...
```

senba 29.3 ns/op vs c17s 460.8 ns/op、HR は 0.316 で bit-for-bit 一致。

## 2. Results

### 2.1 Zipf (合成 workload)

cap=4096, --ops 4M (lib は trace 全体 4M、c17s は warmup 200k + ops 4M):

| skew | HR (lib) | HR (c17s) | lib ns/op | c17s ns/op | overhead | overhead Δ |
|---:|---:|---:|---:|---:|---:|---:|
| 0.8 | 0.451 | 0.453 | 27.8 | 129.2 | 4.65x | +101 ns/op |
| 1.0 | 0.716 | 0.718 | 22.4 | 86.1 | 3.85x | +64 ns/op |
| 1.4 | 0.975 | 0.976 | 10.1 | 43.1 | 4.26x | +33 ns/op |

**観察**:
- 高 skew (= 高 HR) ほど絶対値 overhead Δ が小さい (+33 ns)。reader hot path が
  支配的で、miss/insert path の Mutex 取得は 2.5% しか起きないため。
- 低 skew (= 低 HR) では Δ +101 ns/op まで膨らむ。miss path の Mutex acquire (~15 ns) +
  Path B/C escalation の atomic 系 + allocator contention が累積。

### 2.2 Twitter cluster (OSDI Yang 形式)

cap=4096, lib は trace 1M ops 1 pass、c17s は sweep の T=1 値:

| cluster | HR (lib) | HR (c17s T=1) | lib ns/op | c17s ns/op | overhead |
|---|---:|---:|---:|---:|---:|
| cluster006 | 0.353 | 0.356 | 32.0 | 158.5 | 4.96x |
| cluster019 | 0.316 | 0.316 | 29.3 | 460.8 | **15.71x** |
| cluster034 | 0.355 | 0.358 | 29.7 | 261.8 | 8.81x |

**観察**:
- cluster006 / cluster034 は overhead 4-9x、Zipf と同じ band。
- **cluster019 だけ overhead 15.7x で異常値**。HR は 0.316 と 3 cluster で最低、つまり
  miss/insert 比率が最大。miss path 1 回あたりのコストが c17s で極端に大きい構造を
  示唆。
- これは c14s-vtune §3 の "3 hot line bouncing" が **single thread でも (= cross-core c2c
  なしでも) c17s の miss path で per-shard hot line を 3 本以上 touch する累積コスト**として
  顕在化していることの間接証拠。

### 2.3 ARC OLTP (cap=4000)

ARC は preset cap が違うので明示 cap=4000 で揃え、914k ops 1 pass:

| | HR | ns/op | overhead |
|---|---:|---:|---:|
| senba::Cache | 0.517 | 28.8 | 1.0x |
| c17s T=1 | 0.520 | 173.3 | 6.02x |

OLTP は HR 0.52 で Zipf 1.0 と同等帯、overhead も 6.0x と Zipf 0.8 と Twitter 中間。

### 2.4 単スレ lib の workload 別 ns/op レンジ

senba::Cache (lib) を上記 7 workload で回した結果の per-op cost 分布:

```
10 ns/op  ──■───────────────────────  Zipf 1.4 (HR 98%, hot path dominant)
20 ns/op  ─────■──────────────────── Zipf 1.0 (HR 72%)
28 ns/op  ──────■■■■──────────────── Zipf 0.8 / ARC OLTP / Twitter (HR 32-52%)
32 ns/op  ──────────■■──────────────  Twitter cluster006/034 (HR 35%)
```

**lib は全 workload で 10-32 ns/op の range に収まる**。これは Slot32 + AVX2 find + Xxh3
+ inline 構造で達成された数値で、性能の絶対値として「市販 cache library で凌駕しうるもの
を探すのが難しい」帯 (= 過去セッションでの perf-gate 最適化の累積成果)。

## 3. Discussion

### 3.1 構造的コストの所在 — Acquire load 累積

c17s の reader fast path (`Shard::get_by_hash` → `find_lockfree_for_path_a` → AVX2 tag scan
+ visited fetch_or) を atomic 操作で展開すると、1 op あたり:

1. `path_c_epoch.load(Acquire)` (Path C との競合検査) ×1
2. `ShardHot.len.load(Acquire)` (Step 1 で削除済みだが他経路で残存) ×1
3. `AlignedTags.load(Acquire)` (tag SIMD scan の前段) ×1
4. `slot.visited.fetch_or(1, Relaxed)` (hit 時のみ) ×1 — atomic RMW
5. (miss 時) `Mutex::lock()` (Path B/C 取得) ×1
6. (miss 時) `path_c_epoch.fetch_add(1)` などの insert path atomic

x86 で `lock`-prefix RMW は 10-20 サイクル (~3-6 ns)、Acquire load は plain load + 軽い
fence で ~2-4 ns ペナルティ。これが reader fast path で 3-4 本累積すると **~15-25 ns/op が
"MT-correctness 税"** として hot path で必ず課金される。

lib (`senba::Cache`) は `Cell<T>` + 普通の loads/stores で完全に同 semantics を 10 ns/op で
達成しているので、+15-25 ns/op は ほぼ完全に **atomic 化の memory order コスト**である。

### 3.2 Workload 別 overhead 比率の解釈

| HR 帯 | overhead 倍率 | 構造 |
|---|---:|---|
| 95%+ (Zipf 1.4) | ~4.3x | reader fast path のみ、miss path は 2% でほぼ無視 |
| 70% (Zipf 1.0, OLTP) | 3.8-6.0x | hit/miss 比率均衡、Mutex 取得 30% |
| 30-45% (Zipf 0.8, Twitter, OLTP) | 4.7-9x | miss path 累積、allocator pressure |
| < 35% (cluster019) | **15.7x** | miss path 支配、hot-line bouncing がシングルスレでも積む |

特に cluster019 の overhead 15.7x は、c14s-vtune が報告した「3 hot line cross-core bouncing」
の **single thread 版** が顕在化したものと解釈できる: bouncing は無いが、3 本の hot line を
ぐるぐる回って touch するアクセスパターン自体が **L1 内でも L2/L3 round-trip を生む** ため
miss path 1 回が 1μs 級になる (460 ns/op × 0.68 miss rate = miss path 1 回 ~677 ns)。

### 3.3 構造的 ceiling の推定

c/r-series が atomic-correctness 維持の制約下で達成しうる **理論的単スレ ceiling** を推定:

- lib reader fast path: ~10 ns/op (Zipf 1.4)
- 不可避な MT 税 (Acquire load × 3 本累積): +~15 ns/op
- → **c/r-series の単スレ ceiling は ~25 ns/op = ~40 Mops**

T=16 で perfect scaling (= bouncing 完全除去) が成立すれば **640 Mops** が aggregate ceiling。
現状 Zipf 1.4 gim u64 T=16 c17s は 159.81 Mops、r1 w=1 が 174.26 Mops で、**ceiling の
25-27% にしか到達していない**。

T=16 で残り 75% の差分は (a) Acquire load の memory bandwidth saturation、(b) hot-line
bouncing 残存 (r1 で部分的に解消)、(c) Mutex acquire の uncontested cost、の合計。c-series
journey は (b)(c) を c14s-c17s で削ってきたが (a) は構造的不可避領域に入る可能性が高い。

### 3.4 数字の置き場所

これは **c/r-series を否定する数字ではない**。3 つの解釈が並立する:

1. **競争相手は lib ではなく moka / mini-moka 等の MT cache**: T=16 で moka が ~40 Mops、
   mini-moka が ~10-30 Mops の帯にいる workload で、c17s/r1 が 160 Mops 出るのは強い。
2. **app architecture 次第で MT cache 自体が要らない**: 単スレ app + lib なら 99 Mops、
   per-worker 小 lib + 共有 backing store の構成なら更に上を狙える。
3. **MT cache の "用途" は thread 数ではなく "1 cache instance で multi-thread 共有が
   必要な系" に限定される**: web server worker thread が hot data cache を共有する典型例。
   こういう系では lib は API 不適合 (`&mut self`)、c-series が唯一の選択肢。

過去セッションでの senba::Cache 最適化の累積が、たまたま「lib の絶対値が市販品の上位帯」に
押し上げたことで、MT 化の overhead が **比較対象の物差し次第で 4-15x に見える** という
状態になっている。

## 4. 今後の Action items

### 4.1 高優先

1. **VTune memory-access で atomic Acquire 累積を直接観測**
   (`bench_vtune_concurrent --variant c17s --threads 1 --workload zipf-1.4-gim`):
   - Acquire load 1 本あたりの実測サイクル数を取り、§3.3 の +15 ns/op 仮説を確証 / 反証
   - 並列に r1 でも回して "T=16 で残る 75% の天井" の内訳 (memory bound vs compute bound) を
     切り分ける
   - r1-results follow-up §6 #3 と統合実施可能
2. **Acquire 化候補の Relaxed 化可否を 1 件ずつ検証**:
   - `path_c_epoch.load(Acquire)` の Acquire は本当に必要か (seqlock の version 取得なので
     必要)、`ShardHot.len` は前 report で削除済み、`AlignedTags.load(Acquire)` は SIMD scan
     前の synchronization 用 — これを Relaxed に落としても correctness が崩れない proof を
     書ければ ~3-5 ns/op 改善の可能性
   - 失敗例は report に記録 (= 不可避領域の確定)
3. **cluster019 を single-thread perf benchmark の固定 workload に追加**: HR 0.32 の
   miss-heavy 系は perf-gate (criterion) の現 8 シナリオに無い帯で、本 report で明らかに
   なった c17s 弱点 cell を future regression check に組み込む

### 4.2 中優先

4. **per-op cost decomposition tool**: c17s の get / insert 経路の各 step に `RDTSCP` を
   挟んだ instrumented build を作り、各 atomic op の per-op cost を直接測る。VTune が無い
   環境でも回せる軽量 profiler。
5. **lib (senba::Cache) に Mutex/RwLock wrapper を被せた MT 軸 baseline 計測**: c17s と
   比較する moka/mini-moka 以外に、**Mutex<senba::Cache> at T=16** を比較基準に追加すると、
   c-series 全体の design rationale (「naïve な Mutex 戦略から始めて非自明な改善を積めるか」)
   が定量化される
6. **r-series report verdict の文脈再評価**: r1 が cluster019 で T=16 r1 w=8 = 20 Mops を
   出したのは、(senba::Cache 単スレ 36 Mops の) 56% であって、絶対値で見ると「単スレ lib に
   劣る」事実を r1-results §4 / §5 verdict に追記しておく方が誠実 (本 report で fact は
   明示したが、r1-results 本文に back-reference を入れる)

### 4.3 長期

7. **lock-free writer protocol 再挑戦** (c13s 路線): SIEVE は hand pointer のシリアル進行に
   依存するので CAS-based writer は意味退行を起こした (c13s reject)。しかし **eviction を
   epoch-based defer** にすれば writer path の Mutex を消せる可能性が残る。
   r-series が routing affinity で勝負する一方、c-series の next phase として lock-free 化を
   再検討する筋がある (c19s 候補)。
8. **lib の "MT-safe variant" 路線**: lib API 自体を `&self` で multi-thread 共有可能に
   する変種を library 側に作る案。c17s は internal 構造としては既にそれだが、API としては
   experimental に閉じている。**senba::CacheMT** 等の名前で公開する判断は本 report の
   data があれば慎重に下せる (= 単スレ lib の優位を犠牲にしない API 構造の検討)。

## 5. 結論

c-series / r-series が払う MT-correctness 税は **read-dominant で +15-30 ns/op、miss-heavy
で +100-430 ns/op** で、これが c/r-series の絶対値 ceiling を ~40 Mops/core 程度に規定して
いる。現在 T=16 で 160 Mops 出ているのは構造的 ceiling 640 Mops の 25-27%、つまり「まだ
3-4 倍の伸びしろが残る一方、その大半が memory order の構造的下限に近づいている」。

この数字は MT cache の意義を否定しない: 用途は thread 数ではなく **1 cache instance を
multi-thread で共有する必要があるかどうか**で、その用途では lib は使えず c-series が
唯一の選択肢。perf-gate の役割は「lib の単スレ性能を守る」で確定、c/r-series の perf-gate は
「MT cache 同士の競争 (moka / mini-moka / Mutex<lib>) で勝つ」に再定義するのが筋。

senba::Cache の lib 性能は publishable surface の到達点として確立した。MT 版は別軸の戦いで
あり、本 report が定量化した ceiling と現在地の gap (75%) は **次の 2-3 variant で取りに
行く余地**として残っている。
