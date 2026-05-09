# 2026-05-10 — VTune (Windows native) で c14s の write contention を直接観測

- 観測装置: `research/src/bin/bench_vtune_concurrent.rs` (本レポートで新設)
- 関連:
  - `2026-05-10-write-contention-design-space.md` (hot-key 4 系統分類、本稿はその hot-line 仮説の直接検証)
  - `2026-05-10-visited-bitmap.md` (per-shard u64 bitmap 化、本稿で hot-line 化が再露呈)
  - `2026-05-08-c14s-design.md` (Path A lock-free 化、Path B/C は Mutex 残)
  - `2026-05-08-c14s-sweep.md` (T1 uniform read-heavy 16T fail の構造的天井)
  - `2026-05-09-vtune-windows-orig-vs-senba.md` (single-thread 版の方法論を踏襲)
  - `2026-05-10-c15s-sloppy-visited.md` (Phase 1 reject、本稿は同 design space を別軸で攻める動機を提供)
- 種別: **実測ノート**。VTune Microarchitecture Exploration + Memory Access の 2 回連続採取、function-level 分析まで

## 0. TL;DR

c14s @ 4 thread / Zipf skew=1.0 / cap=4096 / 80M ops を Windows native VTune で
`uarch-exploration` と `memory-access` 両方計測した。**hot key 帯における c14s の
構造的 tax は cross-core cache line bouncing で、bounce している line は単一でなく
3 cluster ある** が確定。

1. **L3 Bound 21–22%, LLC Miss Count = 0, DRAM Bound 0.2%** — working set 全体が L3 内で閉じている
   のに L3 が pipeline を 1/5 食う。これは coherence traffic 純粋分 (= cross-core HITM/forwarding)
   以外には説明不能。**「c14s の wall-clock を削っているのは c2c bouncing」が断定可能**。
2. **hot line は 3 cluster** で、絶対 mem stall がほぼ並ぶ:
   - `writer_evict_and_install` 内の writer state cluster (head/tail/hand/len/tags 書き換え列): 0.416s mem stall
   - per-shard `Box<[AtomicU64]>` visited (= `Shard::vbit`): 0.276s
   - per-shard `parking_lot::Mutex` word: 0.251s (lock + unlock 合算)
3. **Mutex word も bouncing 主体**。前回 single-thread 版分析で「Mutex は 8.7% で従」と書いたのを修正。
   total CPU 比 8.8% は変わらないが、その内部の **35–46% が memory stall** (Mutex word が core 間移動している)。
   "LOCK の serialization" ではなく "Mutex word の cache line ownership 移動" が主犯。
4. **read-side は完全に健康**。`scan_evict` Memory Bound **3.8%**、`find_get_avx2` 13.9%。
   tag array の AVX2 連続 read は MESI Shared 状態で全 core が同時保有でき bouncing しない。
   **contention は 100% atomic write 経路に集中している**。
5. 設計含意: 「**Mutex word を消す**」軸は天井が見えた (c9→c14s で 6 Mops 改善が頭打ち)。
   次の改善は (a) hot 3 line を **1 line に co-locate して 1 回の transfer で取らせる**、
   (b) writer が一度 shard を取ったら **複数 op を batch する**、の 2 軸が ROI 上位。
   visited 単独狙いの sloppy 化 (c15s) が reject されたのもこの 3-line picture から後付けで説明できる。

## 1. なぜ計測したか — write-contention-design-space §3 の検証

`2026-05-10-write-contention-design-space.md` §3 で立てた仮説:

> **「atomic = 速い」は誤解で、実態は MESI で hardware level に直列化される** — lock-free の真の利得は
> 「lock を消すこと」ではなく「cache line ownership を分散すること」。

これを **directly observable な数字** で確認したかった。`perf c2c` 相当 (Memory Access analysis with
`analyze-mem-objects=true`) を Windows native で取れば、HITM 経由でどの cache line が bouncing して
いるか function 単位で分かる。

`bench_vtune.rs` (single-thread, senba vs orig) は既存だが concurrent 版が無かった。

## 2. 観測装置: `bench_vtune_concurrent.rs`

設計の踏襲点と改変点:

- **`bench_vtune.rs` の ITT bracket idiom を継承**。`-start-paused` 必須も同じ。warmup と Zipf 列展開は
  collection 外。
- **`bench_concurrent.rs` の `ConcCache` trait + scoped thread + Barrier を縮小流用**。ただし
  moka / mini-moka は省略 — プロファイル対象は senba 系のみで、外部 crate のシンボルが grouping
  を汚すと見たい hot line がノイズで埋まる。
- **shared keyspace + per-thread seed**: 全 thread が同じ Zipf 分布から独立 draw → k=0 周辺が
  共通の hot spot。
- **Zipf 列を thread ごとに事前展開** (`Vec<u64>`)。measurement window 中の RNG / CDF 二分探索は
  なし。total memory は ops × 8 B × threads (今回 80M × 8 × 4 = 2.4 GB → main で生成して thread に
  hand off するため OK)。
- **対応 variant**: c8 / c9 / c14s。c14s は SHARDS=64 固定、c8 は const generic dispatch、c9 は
  runtime shards。
- **per-shard ≤ 64 (6-bit ID 上限) を parse_args で先取り assert**。constructor まで panic を持ち越さない。

main thread の bracket:

```rust
ittapi::pause();      // 起動直後、collection 停止 (-start-paused と二重防御)
// thread spawn (各 thread は warmup → barrier.wait → measurement)
ittapi::resume();     // main の barrier.wait() 直前
barrier.wait();       // 全 thread 同時に measurement loop へ
// join
ittapi::pause();      // collection 停止
```

## 3. 計測条件

```
bench_vtune_concurrent --variant c14s --threads 4 \
    --cap 4096 --keys 100000 --skew 1.0 \
    --warmup 1000000 --ops 80000000 --seed 42

vtune -collect uarch-exploration -start-paused -- bench_vtune_concurrent.exe ...
vtune -collect memory-access -knob analyze-mem-objects=true \
      -start-paused -- bench_vtune_concurrent.exe ...
```

- cap=4096, SHARDS=64 → per-shard cap=64 (c14s/c8 系の 6-bit ID 上限ぴったり)
- keys=100k, skew=1.0 → Zipf(s=1.0) on 100k で `Pr[k=0] ≈ 1/H_100000 ≈ 8.3%`。
  4 thread が全部 hot key を叩く確率が高い contention 専用 workload
- working set (~100k × ~24 B/entry) が L1 + L2 + L3 (Raptor Lake P-core L3 35 MB) に余裕で収まる
  → DRAM/LLC capacity miss は構造的に出ない
- ops=80M, warmup=1M → measurement window ~2s (uarch run の `Elapsed - Paused` で 4.059 − 2.049 = 2.010s)

## 4. 結果 — Microarchitecture Exploration (top-down)

### 4.1 Top-down 主要数値

| 指標 | 値 | 解釈 |
|---|---:|---|
| Elapsed | 4.059s | |
| Paused | 2.049s | warmup 区間 |
| **Active (measurement)** | **2.010s** | 80M ops / 2s ≈ **40 Mops/s aggregate** |
| Clockticks | 32.12B | |
| Instructions Retired | 60.58B | |
| **CPI Rate** | 0.530 | 単独だと普通だが retire 比で見ると不健康 |
| MUX Reliability | 0.998 | counter 信頼可 |
| Average CPU Frequency | 4.5 GHz | |
| Total Thread Count | 5 | main + worker × 4 |

### 4.2 Top-down breakdown (P-core)

| 指標 | 値 | コメント |
|---|---:|---|
| Retiring | 29.3% | pipeline の 1/3 弱しか有効仕事に回っていない |
| Front-End Bound | 13.3% | DSB 落ちなし、icache 圧なし。許容範囲 |
| Bad Speculation | 9.3% | seqlock VERSION flip による reader retry の影響 (§4.4) |
| **Back-End Bound** | **48.1%** | 半分が backend 待ち |
| └ **Memory Bound** | **25.2%** | |
| │  ├ L1 Bound | 10.8% | ほぼ全部 Lock Latency |
| │  │ ├ **Lock Latency** | **10.7%** | **LOCK-prefixed atomic で 1/10 のサイクル消費** |
| │  │ ├ L1 Latency Dependency | 15.4% | dependency chain 系 |
| │  │ └ Loads Blocked by Store Forwarding | 2.1% | |
| │  ├ L2 Bound | 0.6% | 無視可 |
| │  ├ **L3 Bound** | **20.9%** | **cross-core line 移動の税** |
| │  │ └ L3 Latency | 4.4% | |
| │  └ DRAM Bound | 0.2% | working set L3 内 |
| └ Core Bound | 22.9% | |
|   ├ Port Utilization | 20.2% | |
|   └ Serializing Operations | 3.2% | LOCK fence の影響 |

`Load Bound = 0.675` / `Stores Bound = 0.000` — 完全に load 側偏在。store はバッファに吐いて先に
進めているが、load が L3 から戻ってくるのを待っている。これは hot line を別 core から取り直して
いる典型 pattern。

### 4.3 Function-level top-K (Clockticks 順)

| function | Clockticks | CPI | 解釈 |
|---|---:|---:|---|
| `Shard::writer_evict_and_install` | 6.80B (21.2%) | **1.72** | Path B/C 全体。cap=4096 ≪ keys=100k で eviction 常時 |
| `Shard::find_get_avx2` | 3.79B (11.8%) | 0.55 | reader 本体。健康 |
| `Shard::writer_find` | 3.11B (9.7%) | **0.18** | scan 本体は速い、retire 中 |
| **`Shard::vbit`** | **3.07B (9.6%)** | **3.18** | **異常 stall** |
| `Shard::try_candidate` | 2.45B (7.6%) | 0.65 | Path A 試行 |
| `parking_lot::raw_mutex::lock` | 1.91B (5.9%) | 0.85 | |
| `xxh3::buffered_input` | 1.86B (5.8%) | 0.29 | |
| `parking_lot::raw_mutex::unlock` | 0.90B (2.8%) | 1.35 | |

`Shard::vbit` が top-K で唯一 retire が cycle に追いつかない (CPI 3.18)。これが第一の hot-line 候補。

### 4.4 Bad Speculation の正体

`MACHINE_CLEARS.MEMORY_ORDERING` が `writer_evict_and_install` 行で 1.6M event 観測。これは
「投機実行された load が memory ordering 違反検出で squash された」イベント。c14s の Path A
reader が `seqlock-via-tag` retry に巻かれているサイン。c13s で導入した reader bounded retry (MAX=4)
は効いているが完全には吸えていない、という残課題が裏付けられる。

## 5. 結果 — Memory Access (LLC Miss = 0 が決定打)

### 5.1 全体

| 指標 | 値 | コメント |
|---|---:|---|
| Elapsed | 4.373s | |
| CPU Time | 7.344s | active × 4 thread の総 CPU 時間 |
| Paused | 2.252s | |
| **LLC Miss Count** | **0** | **DRAM への traffic なし** |
| **DRAM Bandwidth Bound** | **0.2%** | 帯域問題ゼロ |
| L1 Bound | 11.2% | uarch run と一致 |
| L2 Bound | 0.3% | |
| **L3 Bound** | **21.9%** | uarch 20.9% と一致 |
| DRAM Bound | 0.2% | |
| Loads | 12.51B | |
| Stores | 4.94B | |

これが本レポートの **最重要 1 データ点**: working set が L3 内で完全に閉じているのに L3 が
pipeline の 1/5 を食う。**capacity miss でも DRAM 読み出しでも prefetch も帯域問題でもない** ⇒
**残るのは coherence traffic (= cross-core HITM / forwarding) 以外にない**。
前回の Microarchitecture Exploration での "L3 Bound = c2c 兆候" 仮説は、ここで「兆候」から
「断定」に確定する。

### 5.2 Function-level (CPU time + Memory Bound%)

| function | CPU time | Memory Bound% | abs mem stall | Loads | Stores | S/L |
|---|---:|---:|---:|---:|---:|---:|
| `writer_evict_and_install` | 1.476s | 28.2% | **0.416s** | 1.11B | 224M | 0.20 |
| `find_get_avx2` | 0.830s | 13.9% | 0.115s | 1.90B | 1.35B | 0.71 |
| **`vbit`** | 0.698s | **39.6%** | **0.276s** | 332M | 67M | 0.20 |
| `writer_find` | 0.694s | 20.1% | 0.139s | 3.09B | 66M | 0.02 |
| `try_candidate` | 0.605s | 19.8% | 0.120s | 678M | **702M** | **1.04** |
| **`parking_lot::lock`** | 0.445s | **35.0%** | 0.156s | 534M | 350M | 0.66 |
| `xxh3::buffered_input` | 0.389s | 15.7% | 0.061s | 861M | 592M | 0.69 |
| `scan_evict` | 0.213s | **3.8%** | 0.008s | 279M | 3M | 0.012 |
| **`parking_lot::unlock`** | 0.204s | **46.4%** | 0.095s | 202M | 124M | 0.61 |

### 5.3 三つの hot line

絶対 mem-stall (CPU time × Memory Bound%) でランクすると、**top 3 が同格** に並ぶ:

| 順位 | hot line cluster | abs mem stall | 該当 function |
|---:|---|---:|---|
| 1 | **writer state cluster** (Mutex 配下で書く tags / hand / head / tail / len / entries) | **0.416s** | `writer_evict_and_install` |
| 2 | **per-shard `AtomicU64` visited word** (1 shard = 1 word = 1 line 内 8 byte に集約) | **0.276s** | `vbit` |
| 3 | **per-shard Mutex word** (parking_lot 64-bit atomic) | **0.251s** | `lock` + `unlock` 合算 |

合計 ~0.94s mem stall。残り (writer_find / try_candidate / find_get_avx2 / xxh3) は分散していて単独
hot line を作っていない。

### 5.4 Negative confirmation: scan_evict 3.8%

`scan_evict` は hand 進行で tag array を順に **読むだけ** (write 無し)。Memory Bound **3.8%** は
ほぼゼロ。これは:

- tag array の read は AVX2 連続 chunk で hardware prefetcher に乗る
- read-only line は MESI **Shared** 状態で全 core が同時保有可能 → bouncing しない
- **contention は読み込みではなく "atomic write"** に集中している

という構造を直接裏付ける。**c14s の AVX2 reader は健康で、対 c2c の負債は 100% writer 側にある**。

### 5.5 stores ratio で writer hot を見分ける

| function | S/L 比 | 解釈 |
|---|---:|---|
| `try_candidate` | **1.04** | store > load。Path A の tag CAS + entries write + tag store の write 列 |
| `find_get_avx2` | 0.71 | reader が store 多め (← seqlock retry の cache miss-fill / vbit fetch_or) |
| `parking_lot::unlock` | 0.61 | release store |
| `vbit` | 0.20 | RMW (fetch_or) |
| `writer_evict_and_install` | 0.20 | tag/visited/entries 書き換え |
| `writer_find` | 0.02 | scan のみ |
| `scan_evict` | 0.012 | scan のみ、write 皆無 |

`try_candidate` の S/L 1.04 は注目に値する。Path A は lock-free だが contention は逃げていない:
**Path A 成功時に書き出す line (tag CAS → entries → tag publish) が、別 thread の Path A or
writer Mutex 経路と重なっている**。lock-free 化が「Mutex を消す」目的では達成されているが、
「cache line ownership を分散する」目的では未達。

## 6. 前回 single-thread 観測との接続 / 修正

`2026-05-09-vtune-windows-orig-vs-senba.md` で立てた **「c2c bouncing が cap-fits 帯 senba<orig
の主因」** という仮説は、本稿の 4 thread c14s + LLC Miss = 0 + L3 Bound 21.9% で **直接観測
された** ことになる (ただし対象は senba::Cache ST ではなく c14s だが、c14s の per-shard 構造は
senba::Cache lineage を継承しており c2c の地形は連続的)。

ただし、本稿の途中考察で出した **「Mutex word は 8.7% で hot line として従」** という暫定評価は
**修正が必要**:

- CPU time 比 8.8% (lock + unlock 合算) は変わらず。
- だが内部の Memory Bound% が 35% / 46.4% で、**Mutex word そのものが core 間 bouncing している**。
  LOCK serialization ではない。
- 絶対 mem stall 0.251s は vbit 0.276s と並ぶ。

つまり前回 c14s が c9 (Mutex-only) より 6 Mops しか速くなかった理由は、「Mutex word が消えて
visited line だけ残った」のではなく、**「Mutex word の bouncing は Path A で部分的に逃げたが、
visited word と writer state cluster が代わりに bouncing している」** という picture が正確。
Mutex を消す軸の天井は、3 line 全部攻めない限り見えてこない。

## 7. c15s sloppy visited reject の構造的説明

`2026-05-10-c15s-sloppy-visited.md` で c15s (visited を 1/16 に sample) は skew=1.0 で 0.91× の
regression で reject。これは **本稿の 3-line picture から後付けで説明できる**:

c15s が削れたのは vbit の 0.276s だけ。残り 0.667s (writer state + Mutex) は手付かず。さらに
TLS RNG draw cost (~3 ns / call) を reader hot path に追加していた。トータルで net 負け。

「visited 単独で殴っても全体の 1/3 しか触れない」という量的な裏付けが本稿で得られた。
sloppy 化の方向性自体は否定されないが、**残り 2/3 の hot line を同時に殴る変更とセット** で
ないと結果に出ない。

## 8. 設計含意 — 次の variant 設計方針

3 line picture を踏まえると、ROI 上位の改変は単独 line 狙いより **構造変更** の側にある。
write-contention-design-space.md §2 の 4 系統に当てはめて優先順位:

### 8.1 Co-locate hot 3 line into 1 line (= 空間結合)

現状: per-shard で `Mutex<WriterState>` (1 line) + `Box<[AtomicU64]>` visited (別 box の 1 line)
+ tags (`Box<[AtomicU16]>` 別 box の 2 line) と分散している。writer は 1 op で 3〜4 line を
順に取りに行く → coherence transfer が op あたり multiple round trip 発生。

これらを **1 cache line に集約** すれば、別 core が shard ownership を奪うときに 1 transfer で
全部 dirty を持っていける。論理的には:

```
#[repr(C, align(64))]
struct ShardHot {
    mutex: AtomicU8,        // parking_lot raw word
    head: u8, tail: u8, hand: u8, len: u8,  // 4 byte
    visited: u64,           // 1 shard 全 visited
    // 残り ~50 byte に tags をどこまで詰められるか
}
```

ただし tags は 2 byte × 64 = 128 byte なので 1 line には乗らない。**writer-frequent な metadata
だけ詰めて tags は別 line** が現実解。この変更で 0.416s + 0.276s + 0.251s = 0.943s mem stall が
1 line transfer に圧縮できる可能性。

### 8.2 Writer-side batching (= 直列化を amortize)

writer が一度 Mutex を取ったら、自スレッドの **次の数 op を looking ahead で消化** してから
release する。Caffeine 流の write batching と同じ idea を per-shard レベルで縮小実装。

- write_batch_size=8 で transfer cost を 1/8 に amortize
- ただし latency に影響 (batch 待ちで p99 悪化の可能性)
- c14s の eviction 順序保存制約を壊さないなら採用可

### 8.3 Read-side は触らない

reader (`scan_evict` 3.8%, `find_get_avx2` 13.9%) は健康。c11s の conditional load-then-fetch_or
trick + AVX2 chunked scan が機能している。ここに手を入れるのは ROI 下位。
c15s sloppy visited が reject されたのは、reader を狙ったから。

### 8.4 Shard-affinity (per-shard worker)

write-contention-design-space.md §3.3 で論じた shard-affinity は、本稿の hot key 4-thread workload
には効かない (1 shard に hot key が集中するので 1 worker thread に直列化される)。**moderate Zipf
帯の保険札** という整理は変えない。

## 9. 反証 / 再評価リスト

- ✓ **「c2c bouncing が wall-clock を削っている」** (write-contention-design-space §3) — LLC Miss = 0
  + L3 Bound 22% で **断定可能**
- ✗ **「hot line は visited 単一」** (本稿 §5.2 まで暫定の見立て) — 3 cluster が同格、と修正
- ✗ **「Mutex word は LOCK serialization が主因」** — Mutex word そのものの bouncing が主因、と修正
- ✓ **「reader-side は対 c2c 健康」** — `scan_evict` 3.8% で確認
- ✗ **「Path A lock-free 化で contention を逃げられた」** — `try_candidate` S/L=1.04 で contention は
  Mutex から tag/entries write に移っただけ、と修正
- — **c15s sloppy visited reject の量的根拠** — vbit は全体の 1/3 で、単独狙いでは届かない

## 10. 次のステップ候補

1. **`bench_vtune_concurrent` を c8 と c9 でも回す**。本稿は c14s 単体観測。c9 (Mutex-only) で
   `parking_lot::lock` の Memory Bound% がさらに高くなるはず → Mutex word bouncing 仮説の追加検証。
2. **3 line co-locate (§8.1)** の prototype を c14s から派生させて perf-gate + bench_vtune_concurrent
   AB。`#[repr(C, align(64))]` の hot struct を 1 個生やすだけなので 1 日 work。
3. **writer batching (§8.2)** は eviction 順序保存の難しさが先に来るので、design doc を書いてから
   実装に入る。
