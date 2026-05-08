# VTune (Windows native) で orig vs senba::Cache を μarch 解析

`docs/reports/2026-05-08-external-lib-sweep.md` で観測した **「working set
が cap に収まる高 HR 帯 (Zipf cap=32k, ConCat 1M, OLTP cap=8000) で
`senba::Cache (auto-shard) < sieve_orig`」** という現象に対して、当該
レポートは **「shards 16k に hot 集合が分散して cacheline 局在を失う
(senba 3 cacheline / op vs orig 2 cacheline / op)」** という
**cacheline dispersion 仮説**を提示していた。これを VTune の
Top-Down (Microarchitecture Exploration) と Memory Access analysis で
直接検証した結果、**仮説は反証された一方で、senba は Windows native でも
orig 比 +12–16% 遅い** ことが判明した。Linux/WSL2 で見えた 40% gap よりは
小さいが、Windows native でも構造的 gap は残る。本レポートは観測データ、
解釈、library 設計への含意、follow-up の優先付けをまとめる。

## 計測方法論についての注意 (重要)

VTune を `-start-paused` **無し** で起動すると、プロセス起動から
`ittapi::pause()` が VTune driver に届くまでの数百ms 〜 数秒、および
warmup 区間の早期サンプル (cache empty, insert path 主体) が collection
に混入する。warmup の重さは両 variant に共通なので、measurement 区間の
差を希釈して **wall-clock を artificial に "tied" に見せる** バイアスが
かかる。

加えて、**短い run (active 5s 弱) は MUX Reliability が低くなる傾向** が
あり、特に L1 / TLB 系 counter が undercount される。本レポートで
`active 9–11s` で取り直したところ MUX 0.914→0.959 に上がり、それまで
見えていなかった orig の TLB pressure (L1 Bound 20.1%) が初めて表面化した。
**MUX < 0.95 の run は memory subsystem の数字を信用しない**、を運用則
として残す。

本レポートは `-start-paused` あり + `ops=180M` (active ~10s) の clean run
のみを採用する。本レポート初版ドラフトでは `-start-paused` 無しで
「tied」「IPC < 1」と読んでいたが、計測 artifact と判明したため結論を
全面改訂してある。

## 観測装置: `bench_vtune` バイナリ

VTune を当てるための自己完結ドライバを `research/src/bin/bench_vtune.rs`
に新設 (CLAUDE.md に役割記載済み)。設計上のポイント:

- **外部 trace ファイル依存ゼロ**。Zipf 列を `senba_research::workload::zipf::ZipfGen`
  で in-process 生成し、measurement loop に入る前に `Vec<u64>` に展開する。
  CDF 二分探索 / RNG が timing 区間に混入しない。
- **third-party cache 依存なし**。比較は `senba::Cache (auto-shard)` と
  `sieve_orig` の 2 点のみ。
- **Intel ITT API (`ittapi 0.5`) で collection 範囲を自動制御**。
  warmup と Zipf trace 生成は `ittapi::pause()` 下、measurement loop
  だけ `ittapi::resume()` 下で走る。VTune 起動時に **`-start-paused`
  必須** (前述)。
- **クロスビルド可能**。`cargo xwin build --release -p senba-research
  --bin bench_vtune --target x86_64-pc-windows-msvc` で `.exe + .pdb`
  が出る。target を `x86_64-unknown-linux-gnu` に変えれば Linux ELF
  にも cross-build 可能 (ITT は VTune 非 attach 時 no-op)。

## 計測条件

```
bench_vtune --variant {senba,orig} \
            --cap 1048576 --keys 4000000 --skew 0.9 \
            --warmup 5000000 --ops 180000000 --seed 42

vtune -collect uarch-exploration -start-paused -- bench_vtune.exe ...
vtune -collect memory-access     -start-paused -- bench_vtune.exe ...
```

- cap = 1M (`Cache::new(1_048_576)` は per-shard ≤ 64 になるよう
  shards = 16,384 を自動選択)
- keys = 4M, skew = 0.9 → Zipf の hot 集合は cap 内におおよそ収まる帯
- warmup 5M op を ITT pause 下で実行、collection は measurement 180M op
  ぶんだけに張り付く
- active ~10s で MUX 0.95+ が得られる run 長

## 結果

### wall-clock: senba +12–16% 遅い

`Elapsed - Paused` (= ITT で collection 区間に切り出した measurement
180M op):

| run | variant | Elapsed | Paused | **Active** | Mops/s | senba/orig |
|---|---|---:|---:|---:|---:|---:|
| uarch | orig | 26.151s | 16.734s | **9.417s** | 19.11 | — |
| uarch | senba | 27.086s | 16.152s | **10.934s** | 16.46 | **1.161×** |
| memaccess | orig | 27.479s | 17.594s | **9.885s** | 18.21 | — |
| memaccess | senba | 60.688s | 49.597s | **11.091s** | 16.23 | **1.122×** |

(senba memaccess の Paused 49.6s は Zipf trace 生成側の wall-clock で、
measurement 区間 (= 11.091s active) には影響しない)

variance の収束履歴:

| ops | uarch gap | memaccess gap | range |
|---|---:|---:|---:|
| 90M | +19.7% | +4.9% | 14.8pp |
| **180M** | **+16.1%** | **+12.2%** | **3.9pp** |

ops 倍増で variance は 14.8pp → 3.9pp に縮み、**senba は orig 比 +12–16%
遅い** が安定結論。Linux/WSL2 で観測した 40% gap よりは小さいが、Windows
native でも構造的 gap が残る。

### Top-Down (Microarchitecture Exploration)

| 指標 | orig | senba | Δ (senba − orig) |
|---|---:|---:|---:|
| Clockticks | 41.36B | 48.16B | +16.5% |
| Instructions Retired | 45.28B | 52.79B | +16.6% |
| **CPI Rate** | **0.913** | **0.912** | -0.1% |
| **IPC** | **1.095** | **1.096** | +0.1% |
| MUX Reliability | 0.959 | 0.980 | — |
| Average CPU Frequency | 4.6 GHz | 4.6 GHz | — |
| Retiring | 18.1% | 18.3% | +0.2pp |
| Front-End Bound | 3.8% | 6.7% | +2.9pp |
| Bad Speculation | 6.4% | 6.8% | +0.4pp |
| Back-End Bound | 71.7% | 68.2% | -3.5pp |
| Memory Bound | 48.9% | 49.8% | +0.9pp |
| **L1 Bound** | **20.1%** | **0.0%** | **-20.1pp** ★ |
| ├ DTLB Overhead | 20.4% | — | ★ |
| ├ Load STLB Hit | 4.5% | — | — |
| ├ **Load STLB Miss (4K)** | **15.9%** | — | ★ |
| ├ Load STLB Miss (2M/1G) | 0.0% | — | — |
| └ L1 Latency Dependency | 8.9% | — | — |
| L2 Bound | 1.9% | 0.5% | -1.4pp |
| L3 Bound | 5.2% | 10.0% | +4.8pp |
| └ L3 Latency | 19.4% | **31.7%** | +12.3pp |
| DRAM Bound | 40.6% | 40.9% | +0.3pp |
| Memory Bandwidth | 39.2% | **59.2%** | +20.0pp |
| Memory Latency | 41.0% | 22.5% | -18.5pp |
| Core Bound | 22.8% | 18.4% | -4.6pp |

読み取り:

- **L1/L2/L3/DRAM の絶対値はほぼ一致** (L1 を除いて ±5pp)。
  `2026-05-08-external-lib-sweep.md` の cacheline dispersion 仮説は
  **反証** (引き続き)。
- **★新発見: orig の L1 Bound 20.1% / DTLB Overhead 20.4% / Load
  STLB Miss 15.9%**。これまで 50M / 90M run では MUX 0.914 で undercount
  されており見えなかった、orig 固有の **TLB pressure** が初めて表面化。
  - orig は `Box<Node>` × 1M の小規模 heap allocation を出していて、
    Windows ucrt heap 上の散らばった 4K page に配置される
  - Zipf working set が steady state に達すると random pointer chase が
    STLB (consumer Intel で 2048 4K entries 程度) を頻繁に miss させ、
    page table を 4 階層 walk する必要が発生
  - **すべて 4K bracket の miss** (2M/1G は 0%)。Windows は senba/orig
    どちらにも large page を割り当てていない。Linux で THP が orig の
    `Box<Node>` を 2M page に promote していれば、ここの 15.9% が大幅に
    減る可能性がある (= Linux 40% gap の OS 由来分の説明候補)
- **senba は L1 Bound 0.0%**。`entries: Vec<Entry>` は 1 個の連続仮想
  mapping (~16–24MB) で確保され、Zipf hot 集合への repeated access が
  TLB hot page を保持しやすい。orig の 1M 個別 allocation のような
  STLB 圧迫は発生しない
- **memory pressure の質的性格は対極**。orig は Memory Latency 41.0%
  > Memory Bandwidth 39.2% で **latency-dominated** (`Box<Node>` arena
  の serial pointer chase + STLB miss の合算)。senba は Memory Bandwidth
  59.2% > Memory Latency 22.5% で **bandwidth-dominated** (SIMD probe +
  entries random access の outstanding load 多数)
- **senba の L3 Latency 31.7% (vs orig 19.4%)** は SIMD 並列 probe による
  L3 内部 queue 圧迫の signal。DRAM uncore は両者 0.5–0.9% で飽和して
  いない (Memory Access run の DRAM Bandwidth Bound) にも関わらず L3
  Latency が伸びるのは、**L3 内部の queue / cross-bar arbitration で
  詰まっている** signal と解釈できる
- **IPC は両者 1.095/1.096 でほぼ完全一致**。「異なる memory subsystem
  のリソース (orig: TLB / senba: L3 queue) で頭打ちになる workload を、
  それぞれ ILP で max まで使い切っている」ことを意味する。senba が遅い
  のは IPC が低いからではなく **instruction footprint が +17% 大きい**
  から
- **Bad Speculation 6.4% / 6.8% でほぼ同等**。前回 (5.2% / 7.5%) より
  orig が増、senba が減。tag scan 早期脱出分岐の predict miss が senba
  固有という前回の解釈は弱まる (orig も 6.4% を出している)

### Memory Access analysis

| 指標 | orig | senba | senba / orig |
|---|---:|---:|---:|
| Loads | 8,916,167,477 | 12,622,878,675 | **1.415×** |
| Stores | 6,064,881,941 | 8,710,061,294 | **1.437×** |
| LLC Miss Count | 70,429,568 | 77,032,340 | **1.094×** |
| DRAM Bandwidth Bound (uncore) | 0.9% | 0.5% | — |

per-op (180M ops):
- orig: 49.5 loads/op, 33.7 stores/op, 0.391 LLC miss/op
- senba: 70.1 loads/op (+42%), 48.4 stores/op (+44%), 0.428 LLC miss/op (+9%)

- **load/store 比率は安定**: senba +42% load / +44% store (90M run と一致)。
  これが instruction +17% の主因
- **LLC miss は +9.4%** に拡大 (90M run では +3.8%)。L3 queue で詰まる量が
  増えた分、いくつかの request が L3 を跳ね返されて DRAM 引きになっている
  可能性
- **DRAM BW Bound 0.5–0.9%**。両者とも uncore DRAM BW は飽和していない。
  Top-Down の "Memory Bandwidth 59.2%" は *Memory Bound 内の BW vs
  Latency 比率* であって、絶対的な DRAM BW saturation ではない

## 仮説の検証結論

### Cacheline dispersion 仮説 (`2026-05-08-external-lib-sweep.md` §仮説)

- 予言: senba は orig の 1.5× の cacheline (3 vs 2) を引くため
  Memory Bound (特に L1/L2) が大幅増、結果として遅い
- 観測: Memory Bound 差は +0.9pp (ほぼ同等)、L2/L3 差は ±5pp 内、LLC
  miss 差は +9.4%。**L1 Bound は orig が 20pp 大きい** (TLB miss 由来、
  cacheline dispersion とは別機構)
- 判定: **反証**。少なくとも Windows native + Zipf cap=1M 帯では、
  shards 散逸による cacheline 引き数増加は memory hierarchy 圧力に
  顕在化していない

### Windows native での gap の正体 (改訂後の解釈)

senba と orig の per-op コスト構造は **異なる memory subsystem リソース
で詰まっているのに、IPC 1.10 でほぼ完全一致**:

- **orig**: HashMap probe + `Box<Node>` deref + linked-list pointer follow。
  - 1M 個の Box allocation が ucrt heap 上で散らばり、STLB miss を起こす
    (DTLB Overhead 20.4%, Load STLB Miss 15.9%)
  - serial 依存チェーンで Memory Latency 41.0% 支配、ILP の余地が無い
  - 命令数は少ない (45.28B)、IPC 1.095
- **senba**: shard dispatch + AVX2 tag scan + 6-bit ID 抽出 + visited
  bit + `entries[id]`。
  - `entries` は連続仮想 mapping なので TLB pressure は無い (L1 Bound
    0.0%)
  - SIMD で多数の outstanding load を並列 issue → L3 内部 queue で
    arbitration が起きて L3 Latency 31.7% / Memory BW 59.2%
  - 命令数は多い (52.79B、+17%)、IPC 1.096

純コスト和: 41.36B vs 48.16B clockticks で **+16.5% の差** = wall-clock
+16.1%。ほぼ説明がつく。

**両者の IPC が偶然 1.10 で揃うのは workload 特性**: cache lookup は
本質的に memory hierarchy のどこかで頭打ちになる工程で、SIMD ILP は
「並列に投げて待ち時間を埋める」までしかできない。orig は TLB / Memory
Latency で、senba は L3 queue で、それぞれ別の壁にぶつかって IPC 1.10
弱に収束している。**senba が orig より速くなるためには、instruction
footprint を縮めるか、L3 queue 圧迫を減らす方向で攻める必要がある**
(IPC を上げる方向には伸びしろが無い)。

### Linux/WSL2 で観測された 40% gap との関係

Windows native でも +12–16% gap が残るので、Linux/WSL2 40% gap の分解は:

- **構造由来 (今回特定): 12–16pp** = SIMD 並列 probe による L3 queue
  圧迫 + instruction footprint +17%
- **OS / 計測環境由来: 24–28pp** = 推定要因として:
  1. **THP (Linux) vs ucrt small page (Windows)**: orig の `Box<Node>`
     1M 個が Linux THP で 2M page に promote されれば STLB miss
     (Windows で 15.9%) が消える。orig の Active を 10–15% 削る効果が
     ある可能性。これは Windows でも見えている orig の弱点を Linux が
     OS 機能で解消している、という構図
  2. **glibc thread arena layout**: 1M Box allocation の virtual
     address 連続性で HW prefetcher / DTLB locality が改善
  3. **WSL2 固有の memory virtualization**: Hyper-V 二段ページング、
     `vmmem` の page backing が senba の shard 散らばった access を
     bare Linux 以上に penalize
  4. **Page allocation pattern**: Linux mmap / brk vs Windows VirtualAlloc

VTune で直接測れないので、bare Linux (非 WSL2) で `perf stat -e
dTLB-load-misses,iTLB-load-misses,L1-dcache-loads,page-faults` を取る
ことで仮説 1 (THP) を直接確認できる。bench_vtune は Linux ELF にも
cross-build 可能なので、検証 path は整っている。

## library 設計への含意

`2026-05-08-external-lib-sweep.md` の §「検証案」3 案を再評価:

1. **`Slot8` (256 ent/shard) ブラケット追加**: cap=1M で shards を 1/4 に
   圧縮する案。**動機が部分的に復活**。Cacheline dispersion 仮説は反証
   されたが、Windows native でも +12–16% gap が残り、その主因は
   instruction footprint (+17%) と L3 queue 圧迫。shards 数を減らせば
   per-op の dispatch 命令と probe 並列度が下がり、L3 queue 圧迫も
   緩和する見込み。実装コスト (テンプレートブラケット + per-shard 上限の
   6-bit ID 制約緩和) は依然高い。**再検討候補**。
2. **shards 上限導入** (`next_pow2(min(ceil(cap/64), MAX_SHARDS))`):
   per-shard を膨らませる方向。同じく動機が部分的に復活。
   **再検討候補**。
3. **bare Linux で `perf stat`** で OS-level 要因を切り分け。Windows
   での構造由来分が +12–16% と分かったので、Linux 40% gap のうち残り
   24–28pp が OS-specific であることを直接確認する方向。**継続**。

新規候補:

4. **prefetch hint の API 化** (`Cache::prefetch(&key)` 等): senba は BW-
   dominated だが Memory Latency も依然 22.5% 残る。caller がループで
   持っている次のキーを使って次 op の `entries[id]` を投機 prefetch
   できれば、L3/DRAM latency の隠蔽が可能。op 1 個分 (~600 cycle @
   4.6GHz × 60ns/op で約 280ns) の lookahead が取れるので memory
   latency を完全に隠蔽できる量。**新規検討**。
5. **instruction footprint 削減**: senba の load +42% / store +44%
   のうち、削れる成分があるかソースレベルで精査。具体的には
   - `Shard::find` の tag scan 後の `entries[id]` deref を 1 cacheline
     アクセスに収められるか
   - visited bit の更新を AVX2 mask 内で完結させて store を削れるか
   - shard dispatch (hash 計算 + shard index 計算) を inline で
     共通化できるか
   命令数を 10pp 削れれば cycle も比例して削れる見込み (両者 IPC 1.10
   で揃っているので)。**新規検討**。

総合: **`Cache::new(cap)` の auto-shard heuristic
(`next_pow2(ceil(cap/64))`) は Windows native でも +12–16% のコストが
あり、再評価対象**。ただし orig 自身も TLB pressure という Windows
native 固有の弱点を抱えており、Linux 環境では THP がこれを解消して
gap が拡大する、という非対称性も新たに判明した。

## 副次成果: bench_vtune バイナリ

`research/src/bin/bench_vtune.rs` は本レポートの観測装置として作成したが、
今後 senba::Cache の μarch 解析を追加する際の再利用可能な土台になる:

- ITT API でバイナリ自身が collection を制御するので、人間タイミングの
  ブレが無く、複数 run の比較が再現性高く取れる (ただし起動側で
  `-start-paused` を必ず付けること)
- 外部 trace 依存なしで Zipf を任意パラメータで生成、cap / keys / skew /
  warmup / ops を CLI で振れる
- `senba` と `orig` を 1 バイナリで切り替えられ、同条件での A/B が容易
- `cargo xwin` で Windows MSVC ABI cross-build、`.pdb` 付き
- target 変えれば Linux ELF にも cross-build 可能 (ITT は no-op fallback)
- **active 10s 以上 (= ops ≥ 180M @ ~17 Mops/s) を取ると MUX 0.95+ が
  得られて L1/TLB 系 counter が信頼できる**、を運用則として残す

## Follow-up

- **bare Linux (非 WSL2) で `perf stat`**: bench_vtune を Linux ELF に
  cross-build し、Linux 物理マシンで `perf stat -e
  dTLB-load-misses,iTLB-load-misses,L1-dcache-loads,page-faults,
  LLC-load-misses` を取り、Linux 40% gap のうち THP 効果 (orig の
  STLB miss 解消) を直接確認。本レポートで予測された仮説 1 の検証
- **GitHub Actions one-shot triage job**: `workflow_dispatch` で
  bench_vtune を ubuntu-latest 上で走らせ、senba vs orig の ratio を
  artifact として記録。bare Linux 物理マシンが手元になくても、少なくとも
  WSL2 ≠ Linux generic を切り分けるサンプル点として使える。CI 環境の
  noise 上限 (CV 10–30%) はあるが、12–16% Windows gap や 40% WSL2 gap
  の判定には十分使える信号
- **VTune Memory Access の "Per-allocation breakdown"**: 本 run では
  集計値しか取っていない。`senba::Shard::tags`、`senba::Shard::entries`、
  `sieve_orig` の `Box<Node>` arena の per-allocation latency 分布を
  見れば、L3 内部 queue 圧迫が本当に `entries[]` access 由来なのか、
  あるいは tags scan の load も寄与しているのかが直接出る
- **Large Page (Windows)** で orig の TLB pressure を消す実験: orig 側に
  カスタム allocator を被せて Windows Large Page (要 SeLockMemoryPrivilege)
  に置けば STLB miss が消える。これで gap が 4pp 程度縮めば、Linux THP
  仮説の有力な傍証になる
- **検証案 4 (prefetch hint API) と 5 (instruction footprint 削減)** の
  実装着手判断: 本レポートの結論で動機が固まったので、設計検討を進める
  価値あり。優先度は (5) > (4) (instruction footprint は IPC 1.10 で
  頭打ちな今、cycle 直減効果が確実)
- **`2026-05-08-external-lib-sweep.md` の §仮説 / §検証案** に本
  レポートへの参照を追記して、cacheline dispersion 仮説が反証された
  こと、Windows native でも構造由来 gap が +12–16% 残ること、検証案
  1/2 が再検討候補に戻ったこと、新規検証案 4/5 を追加したことを明示する
