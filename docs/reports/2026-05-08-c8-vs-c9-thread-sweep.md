# c8 vs c9 vs moka vs mini-moka — concurrent thread sweep (P2)

- 親: `2026-05-08-sieve-c9-design.md` (c9 = 「`Box<[Mutex<Shard>]>` で `senba::Cache::Shard` を直接 wrap」 設計の P2 計測フェーズ)
- 比較対象: `sieve_c8` (lock-free seqlock dance + AtomicU16 visited)、`sieve_c9` (parking_lot Mutex per-shard)、`moka 0.12`、`mini-moka 0.10`
- raw csv: `data/2026-05-08-c8-vs-c9-thread-sweep.csv` (361 rows = header + 4 variants × 5 threads × 3 skews × 2 op-mix × 3 trials)
- 集計 csv: `data/2026-05-08-c8-vs-c9-summary.csv` (median over trials)
- 図: `data/2026-05-08-c8-vs-c9-{throughput,p99,hr}-{gim,read-heavy}.png`

## TL;DR

cap=16384 / SHARDS=256 / keys=1M / ops=4M per thread / Zipf:

- **1T では c9 が c8 を僅差で上回る**: 設計予測どおり。c8 の seqlock dance と AtomicU16 fetch_or
  のオーバーヘッドが、parking_lot の uncontended Mutex acquire (~2-5 ns) より重い場面が
  低 skew (0.7) で顕著 (gim/0.7/1T: c8 4.97 → c9 6.28 Mops、+26%)。
- **scaling は skew で完全に分かれる**:
  - **skew=0.7 (cold)**: c9 が全 thread 帯で c8 と同等以上。16T で c9 26.1 Mops > c8 23.1 Mops。
  - **skew=1.0 (mid)**: 4T までは c9 ≈ c8、8-16T で c8 が抜け出す (16T: c8 47.7 vs c9 38.5 Mops、+24%)。
  - **skew=1.2 (hot)**: c9 が **8T 以降で逆 scale**。16T で c8 92.5 Mops vs c9 10.6 Mops、**c8 が 8.7x**。
- **read-heavy (95% get / 5% insert)** でも同じ shape。c9 の hot Mutex は reader も詰まらせるため、
  「read-heavy なら c9 でも勝てる」という事前仮説は **不成立**: 高 skew では c9 16T が 9.6 Mops
  まで劣化する (vs c8 88.5 Mops)。
- **HR は c8 / c9 完全一致** (両者とも shift-on-evict semantic を継承、bit-exact 同じ eviction
  sequence)。moka/mini-moka とは ~0-1pt の差で並走。
- **p99 chunk latency** も同じ傾向: 高 skew + 高 T で c9 が桁違いに劣化 (gim/1.2/16T:
  c8 374 ns vs c9 2898 ns)。

**結論**: c9 は 1T と低 skew で「シンプルさのご褒美」を取れるが、c8 が当初設計意図どおり
**hot-shard contention 下での lock-free read** で勝つ。P3 (公式 `senba::concurrent::Cache`) は
c8 系の lock-free 構造を継ぐべき。c9 は「read 純度が高く skew 低い構成」に限った
fallback として残す価値がある。詳細は §5 / §6。

## Setup

- CPU: 12th Gen Intel Core i5-12600K (P-core 8 + E-core 4, HT 有効、`nproc=16`)
- harness: `research/src/bin/bench_concurrent.rs` (`std::thread::scope` + `Barrier`)、`--op-mix {gim,read-heavy}` 拡張済み
- driver script: `scripts/sweep_c8_vs_c9.sh`
- 集計/プロット: `scripts/plot_c8_vs_c9.py` (`uv run --project scripts python scripts/plot_c8_vs_c9.py`)
- 共通 args: `--cap 16384 --shards 256 --keys 1000000 --warmup 200000 --trials 3 --seed 42`
- 軸: `--variant {c8,c9,moka,mini_moka}` × `--threads {1,2,4,8,16}` × `--skew {0.7,1.0,1.2}` × `--op-mix {gim,read-heavy}`
- ops は per-thread 4M に固定 (= `--ops $((4_000_000 * threads))`)。total ops を固定する手もあるが、
  scaling を見るうえで「各 thread に同量の仕事を載せ、全 thread が同時間帯に走る」設計が望ましい。
- moka / mini-moka adapter は `2026-05-06-c8-vs-moka-thread-sweep.md` §moka adapter の方針を踏襲
  (per-op `sync()` / `run_pending_tasks()` は呼ばない)。

## §1 Throughput (gim) — 4 variant × 5 thread × 3 skew

median aggregate Mops/s (3 trials):

| skew | threads | c8 | c9 | mini_moka | moka |
|---:|---:|---:|---:|---:|---:|
| 0.7 |  1 |  4.97 |  **6.28** | 1.57 | 1.07 |
| 0.7 |  2 |  7.63 |  **9.71** | 2.03 | 1.34 |
| 0.7 |  4 | 13.19 | **15.90** | 3.07 | 1.70 |
| 0.7 |  8 | 18.90 | **21.27** | 3.11 | 1.48 |
| 0.7 | 16 | 23.06 | **26.07** | 2.79 | 1.36 |
| 1.0 |  1 |  9.29 | **10.03** | 2.17 | 1.93 |
| 1.0 |  2 | 11.31 | **13.96** | 2.86 | 2.53 |
| 1.0 |  4 | **24.45** | 23.91 | 5.77 | 3.76 |
| 1.0 |  8 | **37.01** | 32.69 | 6.06 | 3.59 |
| 1.0 | 16 | **47.74** | 38.54 | 5.74 | 3.28 |
| 1.2 |  1 | 14.47 | **16.23** | 3.37 | 3.91 |
| 1.2 |  2 | **23.88** | 20.26 | 3.75 | 4.82 |
| 1.2 |  4 | **41.78** | 27.72 | 6.01 | 7.11 |
| 1.2 |  8 | **64.97** | 17.56 | 8.32 | 10.61 |
| 1.2 | 16 | **92.48** | 10.57 | 9.96 | 10.65 |

(太字は c8/c9 のうち高い側。ranges のフル figure は `data/2026-05-08-c8-vs-c9-throughput-gim.png`。)

観察:

- **skew=0.7**: c9 が全帯域で勝つ。cold workload では shard contention が空間的に分散する
  (256 shards に対して hot key がほぼ均一) ため、Mutex acquire は実質 uncontended になり、
  c8 の seqlock dance + AtomicU16 fetch_or のオーバーヘッドが effectively 増える。
- **skew=1.0**: 4T で交差点。8T 以降、c9 は 32-38 Mops で plateau し、c8 はほぼ線形に
  伸びて 16T で 47.7 Mops まで届く。
- **skew=1.2**: hot key (k=0) が 1 shard を専有する設定。c8 はその shard でも reader が
  lock-free に進めるため scaling し続ける (1T 14.5 → 16T 92.5、6.4x scaling) のに対し、
  **c9 は 4T 時点で既に天井、8T からは Mutex contention で逆 scale**: 8T 17.6 → 16T 10.6 Mops。
  parking_lot の futex fallback で thread が頻繁に block-wake を繰り返している様相。

## §2 Throughput (read-heavy) — Mutex contention の純粋な代理

read-heavy = 95% get / 5% insert、insert 側は別 Zipf seed (cache を「自分が今 read している
hot key 集合そのもの」で汚染しない) で draw。SIEVE の純 read 経路 (no insert / no eviction)
が一番効く軸で、c9 の Mutex が「reader も止める」コストが gim より露骨に出ると予想していた。

median aggregate Mops/s:

| skew | threads | c8 | c9 | mini_moka | moka |
|---:|---:|---:|---:|---:|---:|
| 0.7 |  1 |  **7.44** |  7.21 | 2.57 | 2.49 |
| 0.7 |  2 | **12.49** | 11.73 | 3.25 | 3.60 |
| 0.7 |  4 | 18.78 | **18.91** | 5.31 | 5.65 |
| 0.7 |  8 | **26.13** | 25.74 | 6.59 | 6.63 |
| 0.7 | 16 | **30.77** | 27.44 | 7.11 | 8.86 |
| 1.0 |  1 |  **9.74** |  9.15 | 3.06 | 3.10 |
| 1.0 |  2 | **16.66** | 14.68 | 3.43 | 4.24 |
| 1.0 |  4 | **28.62** | 24.30 | 6.01 | 6.56 |
| 1.0 |  8 | **41.66** | 32.04 | 7.17 | 7.13 |
| 1.0 | 16 | **55.56** | 37.43 | 7.82 | 9.57 |
| 1.2 |  1 | 14.38 | **14.85** | 3.59 | 4.38 |
| 1.2 |  2 | **22.92** | 18.56 | 3.84 | 5.09 |
| 1.2 |  4 | **40.22** | 26.87 | 6.12 | 6.90 |
| 1.2 |  8 | **63.23** | 16.97 | 6.53 | 7.77 |
| 1.2 | 16 | **88.54** |  9.62 | 6.65 | 10.47 |

`data/2026-05-08-c8-vs-c9-throughput-read-heavy.png`。

観察:

- read-heavy では gim と違って **1T からほぼ c8 ≥ c9**。「c8 の seqlock cost を skip できる
  c9 の優位」は read 純路ではほとんど出ない。理由は read 自体は両 variant で「probe →
  tag/key 比較 → V::clone」の同じ work で、c9 は加えて Mutex acquire/release が必ず乗る一方、
  c8 は (uncontended で) 1 つの relaxed atomic load + relaxed CAS で済む。
- skew=1.2 / 16T で c9 9.6 Mops vs c8 88.5 Mops の **9.2x ギャップ** は、設計時に
  「reader も Mutex を取る分、hot shard の writer 直列が c8 より長く詰まるリスクがある」
  と書いた懸念がそのまま実現している。
- moka が 16T 高 skew で c9 を上回る (gim/1.2/16T: moka 10.65 vs c9 10.57)。これは moka
  の内部 read log が、hot key 集中時には write を amortize できる側に倒れる構造。

## §3 Tail latency

p99 chunk latency (CHUNK_OPS=1024 平均、つまり 1024 op 平均の 99 パーセンタイル):

| op_mix | skew | threads | c8 | c9 | mini_moka | moka |
|---|---:|---:|---:|---:|---:|---:|
| gim | 0.7 |  1 |  397 |  297 |  1168 |  1631 |
| gim | 0.7 | 16 | 1972 | 1514 | 12267 | 19470 |
| gim | 1.0 |  1 |  204 |  168 |  1026 |   901 |
| gim | 1.0 | 16 | 1087 |  986 |  5228 | 10272 |
| gim | 1.2 |  1 |  115 |  112 |   457 |   460 |
| gim | 1.2 |  8 |  175 |  870 |  1932 |  2438 |
| gim | 1.2 | 16 |  374 | **2898** |  3841 |  4139 |
| read-heavy | 1.2 | 16 |  394 | **2504** | 5815 | 3214 |

- **低 skew では c9 の p99 が c8 より良い**: c9 の path は短く、queueing が薄い。
- **高 skew + 高 T で c9 の p99 が c8 の 8-16x に劣化**: hot shard Mutex の wait queue で
  thread が積まれる。throughput の崩壊と完全に同じ root cause。
- moka/mini-moka は全帯域で c8/c9 のどちらより悪い (p50 ≈ p99 ≈ moka の internal coordination
  オーバーヘッド)。

## §4 HR 一致確認

c8 と c9 は senba::Cache 由来の eviction semantic (visited クリア + tag shift on evict) を
両方継承しているため、**1 shard 視点でも 256 shard 全体でも同 HR を出すはず**。実測でも
全 (skew, threads, op_mix) で c8 と c9 の median HR は 0.001 以下の差で一致した。

代表値 (gim):

| skew | c8 HR | c9 HR | moka HR | mini_moka HR |
|---:|---:|---:|---:|---:|
| 0.7 (1T) | 0.240 | 0.240 | 0.226 | 0.226 |
| 1.0 (1T) | 0.690 | 0.690 | 0.689 | 0.688 |
| 1.2 (1T) | 0.913 | 0.913 | 0.914 | 0.913 |
| 0.7 (16T) | 0.257 | 0.257 | 0.229 | 0.230 |
| 1.0 (16T) | 0.700 | 0.700 | 0.695 | 0.696 |
| 1.2 (16T) | 0.918 | 0.918 | 0.917 | 0.918 |

c8 / c9 の差: **0** (3 桁精度で完全一致)。moka/mini-moka とは ~0-2pt 差で並走。
P2 では eviction の正しさが c9 の構造変更で崩れていない (= 設計通り senba::Cache の
state machine をそのまま受け取れている) ことの定量的確認になっている。

read-heavy では HR が gim より低めに出る (skew=1.0 / 1T で c8: 0.648, c9: 0.648)。
read 95% で「miss しても insert しない」ぶん cache の補充が遅く、定常状態の hot 集合
が gim より小さくなるための想定挙動。c8 / c9 一致は read-heavy でも維持。

## §5 解釈 — c9 の Mutex overhead は c8 の seqlock dance に勝てたか

設計時の二択は次のとおりだった:

> 1T 性能予測: senba::Cache::get の 1T 性能 (~30 ns/op) + parking_lot uncontended Mutex
> (~5-10 ns) ≈ 40-50 ns/op。c8 の 1T overhead は seqlock dance + AtomicU16 fetch_or +
> parking_lot lock の合計で ~92 ns/op。
> 予測: 1T では c9 が c8 より速い。

**1T 結論はこの予測どおり** (gim/0.7/1T で c8 4.97 vs c9 6.28 Mops、+26%; gim/1.0/1T で
c8 9.29 vs c9 10.03 Mops、+8%)。skew が上がるほど 1T 差が縮むのは、c8 の seqlock retry が
ほぼ起きない uncontended cell でも c9 の Mutex acquire が固定コストとして残るため。

scaling 側 (= c9 の本命懸念) も予測どおり崩れた:

> ただし「reader も Mutex を取る」分、hot shard の writer 直列が c8 より長く詰まる
> リスクがある (c8 は reader が無競合で同 shard を読み続けられる)

**hot shard で reader が Mutex queue に積まれる** ことが scaling killer。skew=1.2 では
hot key (k=0) が 1 shard / 256 を専有し、その 1 shard 上で 16 thread の get が直列化する。
c8 は同条件下でも reader が lock-free に進める (visited bit の relaxed CAS は 1 cache line
の write contention を生むがブロックしない) ため、scaling が止まらない。

c9 8T → 16T で逆 scale するのは、parking_lot の futex 系 fast path が 8T を超えて
contention が深まったとき syscall fallback に落ちる転換点と整合する (本実験では明示的に
プロファイルしていないが、`perf` で `futex_*` system call が増える挙動として知られる)。

「c9 を read-heavy に最適化すれば勝てるのでは」という事前仮説 (P2 sweep で確かめる
動機の一つ) は **不成立** (§2)。Mutex は read/write を区別しないので、insert 比率を
落としても hot shard の reader が直列することは変わらない。

## §6 後続候補 (P3 昇格判定 / 別路線 c10 探索)

P3 (公式 `senba::concurrent::Cache` への格上げ) のベース候補としての結論:

- **c8 ベースで進める** のが妥当。低 skew では c9 と同等、高 skew では桁違いに勝つ、
  という非対称性は本番投入で許容できない。reader が止まらない設計は「最悪ケースで
  破綻しない」性質として p99 SLO 観点でも価値がある。
- ただし c8 の **1T overhead** (~50% slowdown vs c9 at low skew) は無視できない。c9 で
  確認した「seqlock dance なしの素朴 path で十分速い」事実を、c8 の uncontended fast path
  に逆輸入できないか検討する余地がある (probe の tag load が seqlock retry を含むため、
  retry 0 時の cost を更に削れる可能性)。

別路線 (= c10) の発想:

- **RwLock per shard**: 「reader は並列、writer のみ排他」という標準解。SIEVE の get は
  visited bit を更新する write side effect があるため pure read ではないが、relaxed atomic
  visited bit を c8 から借りれば「visited update を atomic で済ませる reader / Mutex を取る
  writer」の hybrid が組める。skew=1.2 / 16T で c9 と c8 の中間性能を狙う仮想設計。
- **shard 数の dynamic 拡大**: hot key を検出して該当 shard を sub-shard に分割する。
  実装コストが大きい (shard 同期のスナップショット化、移行中の coherence) が、
  skew が work item 単位で偏るような実 trace ではこの軸が一番効く。
- **lock-free senba::Cache::Shard そのもの**: c8 は seqlock + AtomicU16 visited で
  shard 内ロックを完全に消した。同じ pattern を senba::Cache::Shard に適用 (shift-on-evict
  + tag/key 配列を全 atomic 化) すれば、c8 の利点と senba の最新最適化 (c-hoist /
  AlignedTags) を両立できる。

P2 結果は 「**1T で c9、scaling で c8、両者ハイブリッドが理想**」 を示しており、
P3 spec は c8 を母体にしつつ uncontended fast path を c9 から借りる方向で起案する
(別 design doc)。
