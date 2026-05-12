# `senba::concurrent::PartitionedCache` VTune 診断 — scaling 律速の特定と cap-tune の発見

## 仮説

`2026-05-12-partitioned-results.md` §Scaling と物理層のボトルネック で立てた 3 仮説の判定を VTune で取りに行く。partitioned N=T は TLS 経由で thread↔partition が 1:1 固定なので cross-partition contention は workload 非依存にゼロ、scaling drop は純粋に microarch 起因。設計 gate cell (Zipf 1.4 read-heavy, T=16, N=16) で T=1→T=8 per-thread が 4.2× 落ち、T=8→T=16 は +15% しか出ないカーブを物理層レベルで説明する。

候補 (`partitioned-results.md` から):

1. **memory BW 飽和** — DRAM へ抜けるトラフィックがピーク BW を食っている
2. **L1d / L2 capacity overflow + SMT pair 共有** — per-partition working set が L1d を溢れて L2/L3 へ落ちる、SMT 有効化で更に縮む
3. **E-core 混入 (T > 6)** — Alder Lake 12600K で T=16 が SMT + E-core を含む、E-core の低 IPC が drag

## 実施したこと

`research/src/bin/bench_vtune_concurrent.rs` に `Variant::Partitioned` + `--partitions N` + `PartitionedWrapper` を追加 (`build_with_partitions` を `ConcCache` trait に escape hatch として導入、ITT bracket は既存と同じ)。`cargo xwin` で Windows MSVC ABI に cross-build:

```bash
cargo xwin build --release -p senba-research \
    --bin bench_vtune_concurrent --target x86_64-pc-windows-msvc
```

Windows 機 (Intel i5-12600K, 6 P-core + 4 E-core, DDR4-3200, L1d 48 KiB / L2 1.25 MiB per P-core / L3 20 MiB) で VTune 2024 を `-collect memory-access` / `-collect uarch-exploration` で 9 cell:

- (cap, T, N) ∈ {4096, 1024} × {(1,1), (8,8), (16,16)} — 6 cell × memory-access
- (cap, T, N) = {4096, 1024} × (1,1), (8,8), (16,16) — 6 cell × uarch (cap=1024 は T=8 のみ追加)
- 共通: skew=1.4, keys=100k, warmup=400k, ops=100M, seed=42

`cap=1024` cell は仮説 2 検証用。per-partition working set 128 KiB → 32 KiB に縮め、L1d (48 KiB) fit にすると L3 Bound と Mops がどう動くかを見る。

## 結果

### Mops と per-op (Elapsed − Paused を測定窓に取る)

| cap | T | N | meas (s) | aggregate Mops | per-thread Mops | per-op (ns) |
|---|---|---|---|---|---|---|
| 4096 | 1 | 1 | 2.287 | 43.7 | 43.7 | 22.9 |
| 4096 | 8 | 8 | 1.969 | 50.8 | 6.4 | 157 |
| 4096 | 16 | 16 | 1.244 | 80.4 | 5.0 | 199 |
| **1024** | 1 | 1 | 1.859 | **53.8** | 53.8 | 18.6 |
| **1024** | 8 | 8 | **0.748** | **133.7** | **16.7** | **60** |
| **1024** | 16 | 16 | **0.635** | **157.5** | **9.8** | **102** |

cap=1024 で T=8 aggregate **+163% (50.8 → 133.7 Mops)**、T=16 で **+96% (80.4 → 157.5 Mops)**。

scaling 構造の変化:

```
cap=4096:  T=1 → T=8: ×1.16   T=8 → T=16: ×1.58
cap=1024:  T=1 → T=8: ×2.49   T=8 → T=16: ×1.18
```

T=1→T=8 で cap=4096 が「scaling 死」していたのが、cap=1024 では素直に 2.5× 伸びる。T=8→T=16 は逆に cap=1024 で頭打ち (SMT pair 飽和が見えてくる、後述)。

### 仮説 1: memory BW 律速 — **却下**

| | cap=4096 T=8 | cap=4096 T=16 | cap=1024 T=8 | cap=1024 T=16 |
|---|---|---|---|---|
| LLC Miss Count | **0** | **0** | 550,231 | **0** |
| DRAM Bound | 0.0% | 0.1% | 0.2% | 0.1% |
| DRAM BW Bound | 0.0% | 3.3% | 5.3% | 0.0% |

cap=4096 T=8/T=16 で **LLC Miss = 0** — 全データが 20 MiB L3 に乗っており、DRAM へ抜けるトラフィックそのものが存在しない。DRAM BW は構造的に律速になり得ない。
cap=1024 T=8 で LLC Miss が 550K 出るのは 7.8B loads の 0.007% で測定誤差域。DRAM Bound 0.2% と合わせて無視できる。BW 仮説は **データの形からして却下**。

### 仮説 2: L3 latency 主犯 — **確定**

memory-access top-down 比較:

| Memory Bound 内訳 | cap=4096 T=8 | cap=1024 T=8 | Δ |
|---|---|---|---|
| Memory Bound (P-core) | 44.8% | 30.3% | **−14.5 pp** |
| **L3 Bound** | **41.7%** | **26.9%** | **−14.8 pp** |
| L1 Bound | 14.1% | 12.1% | −2.0 pp |
| L2 Bound | 0.3% | 1.4% | +1.1 pp |
| DRAM Bound | 0.0% | 0.2% | 微小 |

per-partition working set を 128 KiB → 32 KiB に縮めただけで L3 Bound が 41.7% → 26.9% (−14.8 pp)、Memory Bound 全体は 44.8% → 30.3% (−14.5 pp)。 **L3 Bound 落ち幅と Mops +163% が直接対応** している。データは L3 (capacity) には乗っているのに **L3 latency (≈40 cyc)** が backend stall の中心を占めていた、というのが正しい絵。

`senba::Cache cap=4096 / Slot32` ≈ 128 KiB は P-core L2 (1.25 MiB) には収まるが、SMT pair で 2 thread が同 L2 を共有すると 2 partition = 256 KiB に加えて Zipf tail の cold key が冷えるため、L2 hit rate が崩れて L3 round trip が頻発する。cap=1024 (≈32 KiB) は L1d (48 KiB) 単独で完結するので **per-thread に閉じる**。

### 仮説 3: SMT pair L1d 共有 — **副犯 (cap で大半解消)**

| | cap=4096 T=8 | cap=4096 T=16 | cap=1024 T=8 | cap=1024 T=16 |
|---|---|---|---|---|
| L1 Bound | 14.1% | **23.9%** | 12.1% | 14.6% |
| Δ (T=8 → T=16) | — | **+9.8 pp** | — | **+2.5 pp** |

cap=4096 で T=16 にしたときの L1 Bound +9.8 pp は SMT pair 直撃の指標 (SMT 有効化は T=12 以降、6 P-core × 2 hw thread が L1d 48 KiB を奪い合う形)。これが cap=1024 では +2.5 pp に縮む — **working set が L1d に収まれば SMT pair の害そのものが消える**。
SMT off (P-core only T≤6) で取り直す価値はあるが、cap-tune だけで大半解消できることが分かったので優先度は下がる。

### uarch top-down (cap=4096 T=8 vs cap=1024 T=8)

| | cap=4096 T=8 | cap=1024 T=8 |
|---|---|---|
| CPI (P-core) | 1.746 | **0.608** |
| Retiring (P-core) | 13.6% | **44.0%** |
| Front-End Bound | 6.9% | 10.8% |
| Bad Speculation | 10.0% | 5.5% |
| Back-End Bound | 69.5% | **39.7%** |
| **Retiring (E-core)** | 8.4% | **30.2%** |
| Load Bound (Info) | 0.539 | 0.343 |

cap=1024 T=8 の Retiring 44.0% は **cap=4096 T=1 baseline の 46.7% にほぼ復帰** — per-thread の execution profile が単スレ時とほぼ同じになっている。CPI 0.608 は T=1 cap=4096 の 0.378 比で 1.6× 劣化のみで、8 core 同時稼働の cost としてはほぼ ideal。
E-core Retiring が 8.4% → 30.2% (3.6×) に跳ねるのが面白い: cap=4096 では E-core スレッドが共有 L3 stall に巻き込まれて事実上ほぼ進めていなかったのが、cap=1024 では本来の E-core IPC で仕事を回し始めている。

### 仮説 3 補足: E-core 混入 — **寄与小**

cap=4096 T=16 で E-core Bad Speculation 62.9% / Machine_Restart 11.6% は前報告書で「E-core が drag」の根拠にしたが、memory-access 側で見ると E-core Memory Bound は 0.0–0.1% で安定、つまり E-core は **memory に詰まっているのではなく、Bad Spec で命令を捨てているだけ**。aggregate Mops に対する寄与は P-core の memory stall に比べて小さい。

## 学び

### cap-tune が partitioned scaling の支配因子

最も意外なのは、partitioned の scaling が library 設計ではなく **per-partition working set が L1d に収まるか** で 2.5× 動くこと。Slot32 entry なら per-partition cap ≈ 1024 (entry 領域 32 KiB) が sweet spot、tags/meta 込みでも 48 KiB 未満で L1d 単独完結する。

設計ガイドライン化するならこう書ける:

> partition 数 N を選ぶときは、`cap / N` が L1d size / sizeof(entry) を超えないようにする。Slot32 で L1d 48 KiB の機械なら `cap / N ≤ 1024` を目安にする。これを超えると per-thread の access が L3 round trip に支配されて scaling が壊れる。

### 設計書の reject 条件は cap=4096 前提だった

`2026-05-12-partitioned-design.md` §Acceptance / Reject は「Zipf 1.4 read-heavy u64 で c17s 同等以下 → reject」。cap=4096 sweep では partitioned 75.4 Mops vs c17s 133.1 Mops (−43%) で機械的 reject だった。
本書の cap=1024 単点測定では partitioned T=8 が 133.7 Mops、T=16 が 157.5 Mops で、**c17s 133.1 Mops とほぼ並ぶか上回る**。reject の前提 (cap=4096) が崩れたので、再評価をかけるべき領域に入った。

ただし cap=1024 N=16 = per-partition 64 entries は Zipf 1.4 keys=100k で HR を確実に削る (cap=4096 N=16 で HR 0.865)。本 VTune run は HR を出力していない (Mops 計測専用)。HR と Mops の trade-off は `bench_concurrent` の sweep で取り直さないと採否は決められない — これが follow-up の最重要項目。

### Mutex は dominant ではない

cap=4096 T=8 uarch で Lock Latency 6.7% を観測したが、Memory Bound 44.8% / L3 Bound 41.7% に比べて脇役。Store Bound は memory-access では全 cell 0.0%。`parking_lot::Mutex` 採用判断は妥当で、Mutex 内側 (Cache 単スレ) と Mutex 自体に手を入れても scaling は変わらない。

### 残存の L3 Bound 26.9% (cap=1024) は何由来か

仮説 (定量は要追加実験):

1. **Zipf 列 Vec<u64>** = per-thread 12.5M × 8B = 100 MB の streaming read。HW prefetch で大半吸収できるが Zipf 列はランダム pattern なので prefetch 効率は低い
2. **partition Mutex の cacheline**: parking_lot Mutex は uncontended でも acquire で 1 line を modified にする → SMT pair 間で coherence traffic
3. hash 計算 (XXH3) の constant table 192 B (微少)

候補 1 が最有力。bench_vtune の単スレ synthetic (値を Zipf 列に依存させない) で切り分けられるが scope 外。

## 今後

### 直近 (経験的検証、採否確定のために先に回す)

1. **cap=1024 で `bench_concurrent` の (T × N × workload) sweep を取り直す**: HR と Mops の trade-off マップが本研究の最大の宿題。`2026-05-12-partitioned-results.md` の領域分割 (Zipf 完敗 / real trace 圧勝) が cap=1024 でどう動くか。real trace (Twitter cluster019, ARC DS1) は元々 HR-tolerant なので cap 縮小で更に partitioned 有利になる仮説、検証する。**partitioned 採否を確定させる sweep**。
2. **memory-object group-by で partition Mutex の cacheline を直接観測**: 残存 L3 Bound 26.9% のうち Mutex line の寄与を分離。`vtune -report hotspots -group-by memory-object` を既存 `r_part_cap1024_T8_N8_mem` に対して回せばコスト 0 で取れる。
3. SMT off (T=6 P-core affinity) cell: cap-tune で大半消えたので優先度低、但し partitioned の物理ピーク値を knowledge として持っておく価値はある。
4. cap=512 / cap=256 を加えた cap sweep: L1d fit 帯のさらに下で writer (eviction + insert) 比率が増え、Mutex contention が初めて支配的になるはず。partitioned の reject ライン側を見にいく。

### 計算量増を許して L3 を緩める algorithmic 案 (採否確定後)

cap-tune は library の自由度として大きいが、ユーザーに sizing を強いる解。algorithmic に同じ効果を狙えれば lib として価値が上がる。本書の VTune 結果に基づき、計算量を多少増やしてでも L3 Bound を削る方向に何が刺さるかを整理しておく。

**まず効かない方向の棄却**: tag を u8 → u4 等に縮める案は割が悪い。理由は tags 4 KiB がそもそも sequential SIMD scan で L1d 在留しており、L3 Bound 41% を吐いているのは **entries 96 KiB/partition のランダム touch** だから。tag false positive 率を 1/256 → 1/16 に劣化させると entry cacheline の余計な touch が増えて逆効果になる。tag 側を弄っても entries 側の working set には影響しない。

候補 (期待値順):

| 案 | 概要 | 期待 perf | コスト / risk |
|---|---|---|---|
| **two-tier (hot/cold split)** | partition 内側に小 hot tier (cap=128 ≈ 4 KiB、L1d 完全 fit) + 大 cold tier。Zipf 1.4 で hot tier が hit の 85%+ を捌けば、その op は L3 完全回避 | **+100% 候補 (cap-tune と独立、相乗あり)** | 新 variant `partitioned_tiered` を作るだけ、既存 `Cache` × 2 で実験可。promotion path の race-free 性のみ要設計 |
| **entry fingerprint 化** | entry の full key (u64 = 8B) を 32-bit fingerprint に置換。per-partition 96 KiB → 64 KiB に縮む | +50–80% (L3 Bound 41% → 25% 見込み) | slot layout 全書き換え、`Iter` で原 key を返せなくなる API 影響あり |
| **prefetch batch API** | `get_many(&[K])` を生やし、batch 内で `keys[i+k]` の tag/entry line を `_mm_prefetch` で先読み | batch 限定で +30–50%、単発 get には効かない | additive (`get` 互換維持)、実装コスト最小 |
| inline shard (1 cacheline / shard) | tag+entry を 1 cacheline 内に inline、SoA → AoS 寄せ | 単体小、上記と組み合わせ前提 | layout 全書き換え、SIMD scan の vector 化が崩れる |
| direct-mapped variant | SIEVE 廃して hash 直撃、collision は問答無用に上書き | speed +、HR − (real trace で沈む) | accept zone が狭い、HR-sensitive workload で失格 |

**実装着手の優先案: two-tier**。理由は (a) cap-tune の延長線で「ユーザーが cap を選ばなくても hot tier が勝手に L1d fit する」絵が library として綺麗、(b) 既存 `senba::Cache` を 2 段に組むだけで研究実装は最小、(c) VTune で L3 Bound が 41% → 15% に落ちれば一発で診断確定 (外れても情報量大)。entry fingerprint 化は API 影響が広く、partitioned 採否が確定してから着手するほうが投資判断しやすい。prefetch batch API は **public API 拡張として独立に価値がある** ので、algorithmic 系列とは別軸で進められる。

順序としては「直近の sweep で採否確定 → two-tier 試作 → fingerprint 検討 → batch API 追加」が無駄が少ない。algorithmic 案を先に走らせると、partitioned の reject 判定が変わった場合に試作が空振りになる。

#### fingerprint 化の設計空間 (MemC3 との比較、oracle 緩和の話)

fingerprint key は senba の独自発明ではなく、**MemC3** (Fan, Andersen, Kaminsky, NSDI'13) が memcached 互換実装で同じ「index を L1d/L2 在留させる」目的で採用した古典手法。ただし memcached は **純 KV store** で `get(K)` が K と無関係な V を返すのは仕様違反なので、MemC3 は fingerprint match 後に payload arena の full key と verify する。これに対し senba は **cache (任意 eviction 許容)** なので、verify を払うか省くかが新たな設計自由度になる:

| 設計 | hit path | 誤 hit 時 | 衝突確率 (32-bit fp) |
|---|---|---|---|
| A. full key 保持 (現状 `senba::Cache`) | tag → entry (key+val) → key eq → val | 起きない | 0 |
| B. MemC3 流 (verify あり) | tag → fp → fp eq → arena (key+val) → key eq | 起きない、verify cost あり | 0 |
| C. **verify 省略 (senba 新自由度)** | tag → fp → fp eq → val (line touch 最小) | 間違った V を返す、ただし "誤 hit → 自然な evict" として cache 挙動に吸収 | ~2.3 × 10⁻¹⁰ |

C の誤 hit は cap=4096 で 100M ops 走らせて期待 0.023 件。application から見ると「miss だったが別 key の値が返る」形で、cache 上の値の妥当性を application が独立に確認している大半のユースケースでは区別不能。

**oracle 整合の問題と、その上での位置づけ**: C を採ると `sieve_orig` との eviction sequence bit-exact 一致は崩れる (fingerprint 衝突で evict 対象が変わる)。これは厳しい contract に見えるが、現実には senba の **set-associative variants (j3+) が既に strict SIEVE から外れている** — 公開 oracle (`research/tests/oracle.rs`) も j3/j8 については `cap = per_shard` (= 1 shard) 構成でしか一致を取っていない。multi-shard 配置で運用する c-series / r-series / partitioned は **既に「SIEVE 意味論を per-shard でしか保たない」近似** で、global eviction order は原 SIEVE と異なる挙動を許容する流れにある。fingerprint-relaxed (C) はその系譜の自然な延長 (= 「SIEVE 厳格性をどこまで希釈するか」連続スペクトル上の更に 1 歩) であって、新規の劣化ではなく、既にある trade-off の延伸として位置づけられる。

公開 API としての切り分け方:

- 研究系列の variant (`senba-research::experimental::sieve_X`) は C 路線を遠慮なく踏んで perf 上限を取りに行く
- `senba::Cache` (公開 lib) は A を保つか B を採るかの 2 択 — `Cache` の contract に「fingerprint 衝突で誤値を返さない」を残すかどうかは別判断

この区分は CLAUDE.md の "senba (publishable) / senba-research (experimental)" 二層構造とも整合的。oracle 緩和は research crate 内で完結し、publish 面の信頼性は守られる。

## 関連レポート

- `2026-05-12-partitioned-design.md` — sweep 設計、accept/reject 基準の出所
- `2026-05-12-partitioned-results.md` — cap=4096 sweep 結果、本書の出発点 (仮説 3 種を提示)
- `2026-05-12-mt-overhead-vs-lib.md` — ceiling 推定の出所、c17s 単スレ overhead 構造
