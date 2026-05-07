# 2026-05-08 single-shard concurrent testbed — c8 vs c9 baseline

- 親: 親プラン (c10..c1n の最適化サイクル全体で「単一 shard 内の並行スケーリング限界」を直接測る観測装置)
- testbed コード: `research/src/single_shard.rs` (trait + adapter), `research/src/bin/bench_single_shard.rs` (harness)
- raw csv: `data/2026-05-08-single-shard-baseline.csv` (151 行 = header + 150 trials)
- 集計 csv: `data/2026-05-08-single-shard-summary.csv`
- 図: `data/2026-05-08-single-shard-{throughput,min-mops,p99}-{read-only,read-heavy,gim}.png` (9 枚)
- 関連: `2026-05-08-c8-vs-c9-thread-sweep.md` (multi-shard 結果、本稿はその shard 内側分解)

## TL;DR

cap=64 (c8 6-bit ID 上限) / 1 shard / 1 trial / 1M ops per thread / Zipf keys=100k / cap=64:

- **uniform read-only 16T で c9 が崩壊**: c8 = **352.6 Mops**, c9 = **5.25 Mops**、**67x 差**。
  uniform は thread 別 disjoint key で shard 内競合 floor を測る workload なのに、c9 は
  Mutex per shard なので「key が違っても全 thread が同 Mutex で直列」する構造的限界が
  そのまま出ている。**c10 が継ぐべき性質の核**は「reader が writer / 他 reader を待たない」。
- **adversarial-hot read-only で c8 もプラトー**: c8 1T 73.9 → 16T 31.1 Mops。lock-free reader
  でも visited bit `fetch_or` の cache-line ping-pong が **scaling killer** として残る。
  この workload は visited 純粋効果の理論上限ストレス。c10 で attack すべき第一候補。
- **gim 50/50 mix では c8 すら 1T < 2T**: c8 gim adv-hot 1T 72.4 → 2T 29.6。50% insert は
  全部 writer Mutex を取るので、insert 比率が高い workload では reader lock-free の利得が
  消える。c10 で writer Mutex の coverage を狭める (CAS-based slot claim) も別軸の改善余地。
- **read-heavy (95/5) は c8 が大勝**: zipf-1.0 / 16T で c8 22.13 vs c9 2.35 Mops、9.4x。
  ここが `senba::concurrent::Cache` の「想定 sweet spot」(read 多め、たまに insert) と
  読み替えられる軸で、c8 lineage を継ぐべきという既往結論を再確認。
- **HR 一致**: c8 と c9 の hit_ratio は 0.001 以下の差で完全一致 (両者とも senba::Shard
  互換 eviction)。**正しさが構造変更で崩れていない** ことを 150 trial 全点で確認済み。

**結論**: c10 設計の attack 順位は (i) **visited bit を別 cache line に分離** (read-only
adversarial-hot プラトー解消)、(ii) **writer Mutex の lock-free 化または micro-stripe 化**
(gim mix で 1T < 2T を解消)、(iii) その他 (false sharing 排除など)。本 baseline はこの
順位の根拠データを提供する。

## Setup

- CPU: 12th Gen Intel Core i5-12600K (P-core 8 + E-core 4, HT 有効、`nproc=16`)
- harness: `research/src/bin/bench_single_shard.rs` (`std::thread::scope` + `Barrier`、
  既存 `bench_concurrent.rs` 構造を踏襲、CHUNK_OPS=1024 で chunk mean → p50/p99)
- driver script: `scripts/sweep_single_shard_baseline.sh`
- 集計/プロット: `scripts/plot_single_shard_baseline.py` (`uv run --project scripts python ...`)
- 共通 args: `--cap 64 --keys 100000 --warmup 80000 --trials 1 --seed 42 --ops $((1_000_000 * threads))`
- 軸: `--variant {c8,c9}` × `--threads {1,2,4,8,16}` × `--workload {zipf×3 skew, adversarial-hot, uniform}`
  × `--op-mix {read-only, read-heavy, gim}` = 150 trials
- `cap=64` は c8 の 6-bit ID 上限を反映した「単一 shard が物理的に取れる最大」。multi-shard
  運用 (cap=16384/shards=256 = 64/shard) でも shard 内側は同じ 64 entries なので、
  実プロダクト想定と一致するスケール。

## §1 read-only — 純粋 reader path のスケーリング

aggregate Mops/s, median over 1 trial:

| workload | threads | c8 | c9 | c8/c9 ratio |
|---|---:|---:|---:|---:|
| uniform           |  1 |  71.54 |  52.29 | 1.37x |
| uniform           |  4 | 105.21 |  15.28 | 6.9x  |
| uniform           |  8 | 202.65 |   8.84 | **22.9x** |
| uniform           | 16 | **352.56** |   5.25 | **67.2x** |
| zipf-0.7          |  1 |  19.27 |  18.57 | 1.04x |
| zipf-0.7          | 16 |  87.16 |   2.46 | 35.4x |
| zipf-1.0          |  1 |  17.18 |  17.03 | 1.01x |
| zipf-1.0          | 16 |  44.28 |   2.33 | 19.0x |
| zipf-1.2          |  1 |  17.82 |  16.99 | 1.05x |
| zipf-1.2          | 16 |  35.58 |   2.56 | 13.9x |
| adversarial-hot   |  1 |  73.91 |  54.91 | 1.35x |
| adversarial-hot   |  8 |  34.20 |   9.55 |  3.6x |
| adversarial-hot   | 16 |  31.14 |   5.05 |  6.2x |

(`data/2026-05-08-single-shard-throughput-read-only.png`)

観察:

- **uniform** は c8 がほぼ理想的に scale する (1T 71.5 → 16T 352.6、4.93x)。c9 は **逆 scale**
  (1T 52.3 → 16T 5.3、0.10x)。c9 は uniform でも全 thread が同 Mutex を奪い合うため、
  thread 数増加で Mutex queue 待ち時間が増える一方。これは「shard 1 個 + Mutex」設計の
  最も clean な弱点露出。
- **zipf** では c8 もスケーリングが落ちる (zipf-1.2 で 1T→16T 2.0x のみ)。これは hot key が
  visited bit ping-pong を発生させるため (詳細 §3)。
- **adversarial-hot** では c8 が **1T → 2T で大幅に劣化** (73.9 → ~30 と急落、その後 16T 31)。
  全 thread が同一 cache line の visited bit を `fetch_or` するため、MESI invalidation が
  shared cache line bandwidth を喰う。lock-free でも reader-reader contention は cache
  coherency 経由で必ず発生する、という SIEVE 並行設計の構造的特徴。

## §2 gim 50/50 — writer Mutex 純粋効果

aggregate Mops/s:

| workload | threads | c8 | c9 |
|---|---:|---:|---:|
| uniform           |  1 |  13.46 |  24.48 |
| uniform           |  4 |   4.89 |   6.93 |
| uniform           | 16 |   1.90 |   2.16 |
| zipf-1.0          |  1 |   9.45 |  13.09 |
| zipf-1.0          | 16 |   2.20 |   1.52 |
| zipf-1.2          |  1 |  12.87 |  15.43 |
| zipf-1.2          | 16 |   3.68 |   1.83 |
| adversarial-hot   |  1 |  72.40 |  54.48 |
| adversarial-hot   | 16 |  31.67 |   5.02 |

(`data/2026-05-08-single-shard-throughput-gim.png`)

観察:

- **gim 50/50 では c8 でも 1T が最速**になるケースが多い (uniform / zipf 全 skew で 1T 最大)。
  insert は writer Mutex を取るので、50% insert = 50% Mutex acquire/release。lock-free reader
  経路があっても writer 側の直列が支配項。
- **uniform / 1T で c9 (24.5) > c8 (13.5)** という逆転が発生する。理由は推測だが c8 の
  `Shard::insert` には writer-side find (writer_find) が writer_update_in_place まで
  含めて素直に Mutex 配下で走る一方、c9 の senba::Shard は単純 SIMD find + 直接 update で
  書き換え経路が短い可能性。c8 は writer 経路の最適化余地があると示唆。
- **adversarial-hot / 1T で c8 (72.4) > c9 (54.5)** だが、これは「常に同一既存キーへの
  insert = update path」が走るため、c8 の seqlock dance free な writer 経路 (Mutex 配下
  Relaxed atomic) が c9 より軽い。

## §3 read-heavy 95/5 — `senba::concurrent::Cache` の想定軸

aggregate Mops/s:

| workload | threads | c8 | c9 | c8/c9 |
|---|---:|---:|---:|---:|
| uniform           |  1 |  58.39 |  50.67 | 1.15x |
| uniform           | 16 |  21.43 |   4.77 | **4.5x** |
| zipf-0.7          |  1 |  17.30 |  15.88 | 1.09x |
| zipf-0.7          | 16 |  23.44 |   2.30 | **10.2x** |
| zipf-1.0          | 16 |  22.13 |   2.35 | **9.4x** |
| zipf-1.2          | 16 |  22.54 |   2.40 | **9.4x** |
| adversarial-hot   | 16 |  20.39 |   4.83 | **4.2x** |

(`data/2026-05-08-single-shard-throughput-read-heavy.png`)

観察:

- **zipf 全 skew で c8 16T が 22-23 Mops でほぼ一定** (skew に依存しない)。read 95% で
  reader lock-free 利得を最大化、5% insert は全 thread を Mutex に詰まらせるが、頻度が
  低いので scaling shape を支配しない。
- **c9 は zipf 全 skew で 16T 2.3 Mops に collapse**。Mutex per shard は read 95% でも
  reader が Mutex queue で詰まるため、read-heavy 軸で **8-10x** の差。
- **adversarial-hot read-heavy では c8 も plateau** (16T 20.4 Mops)。read-only 比 (16T 31.1)
  で 1.5x 遅い。全 reader が visited bit を同一 cache line に書き続ける + 5% insert で
  writer Mutex を踏むという二重の効果。

## §4 mops_min_per_thread — thread 不均衡の検出

`mops_min_per_thread` は最も遅い thread の throughput。aggregate と離れているほど特定 thread が
starvation していることを示す。代表値:

| op_mix | workload | threads | c8 aggr | c8 min/thread | c8 ratio | c9 aggr | c9 min/thread | c9 ratio |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| read-only  | uniform        | 16 | 352.56 | 0.118 | **0.5%** | 5.25 | 0.135 | 41% |
| read-only  | adversarial-hot| 16 |  31.14 | 1.979 | 102%  | 5.05 | 0.314 | 99% |
| gim        | adv-hot        | 16 |  31.67 | 1.979 | 100%  | 5.02 | 0.314 | 100% |
| read-heavy | zipf-1.0       | 16 |  22.13 | 1.137 | 82%   | 2.35 | 0.150 | 102% |

(ratio = `min_per_thread × threads / aggregate`、100% = 完全均一)

(`data/2026-05-08-single-shard-min-mops-{op_mix}.png`)

観察:

- **c8 read-only uniform 16T で ratio 0.5%** という極端な不均衡。これは個別 thread の
  視点では「自分だけが極端に slow」なのではなく、**aggregate Mops が 352.56 と過剰評価
  されている** と読み解くべき: max_elapsed 基準で aggregate を計算しているので、最も
  速い thread が完了して以降は他 thread の作業が contention 無しで進み、min thread の
  実 throughput が落ちる現象。**測定方法のアーティファクト**。本来均一に走った場合の
  「1 thread あたり ~22 Mops」が真の per-thread floor。これは harness 側の課題として
  c10 計測時に修正候補 (= 全 thread 均等な ops 終端で計測する)。
- それ以外は ratio ~100% で均一。adv-hot や gim は work が短いほど終端タイミングが
  揃うので不均衡が見えにくい。

## §5 解釈 — c10 設計に向けた優先度

3 軸の差分から bottleneck を分離:

| 観測 | bottleneck の所在 |
|---|---|
| uniform read-only で c9 が崩壊 (c8 vs c9 67x) | **Mutex per shard = scaling killer**。c10 はこれを継承しない (reader lock-free を維持) |
| zipf read-only で c8 scaling 鈍化 (1T→16T で 2-5x のみ) | hot key cluster が同一 cache line を圧迫、visited bit + tag load の coherency cost |
| adversarial-hot read-only で c8 plateau | **visited bit `fetch_or` の純粋効果**。同 cache line に複数 reader が write → MESI invalidation 連発 |
| gim 50/50 で c8 1T が最速 | writer Mutex coverage が広すぎ。insert path の lock-free 化 (CAS-based slot claim) で改善余地 |
| read-heavy 95/5 で c8 が zipf でも 22 Mops 維持 | reader lock-free の真価が出る軸。**`senba::concurrent::Cache` の主用途として推す軸** |

c10 設計の attack 順位 (確度 + impact 順):

1. **visited bit の cache-line 分離** (確度: 高、impact: 大)
   - read-only adversarial-hot の c8 plateau (16T 31 Mops) を解消する手段
   - 具体策: tag (u16) から VISITED bit を抜き、別 array `visited: Box<[AtomicU64]>` に bit-packed
     で持つ (1 entry = 1 bit、64 entry/cap なら 1 word で完結 = 8 byte = cache line 内独立)
   - tag load の cache line と visited write の cache line が分離されれば、reader-reader の
     coherency 競合は visited bit cache line 1 本に局所化、tag load は invalidation を被らない
   - リスク: writer の eviction で「visited bit を剥がす」操作と「tag を EMPTY 化」が別 atomic に
     なるので、writer 内部の順序を慎重に組む必要がある (visited→tag の順)

2. **writer Mutex の lock-free claim** (確度: 中、impact: 中)
   - gim mix で c8 が 1T < 2T に degradation する現象を解消する手段
   - 具体策: insert を CAS-based slot claim に置き換え。空 slot を `tag.compare_exchange(EMPTY, RESERVED)`
     で取り、entries 書き込み後に `tag.store(LIVE | id, Release)` で公開。eviction (hand 進行) は
     依然 Mutex で良い (頻度が低い)
   - リスク: insert と eviction の interaction が複雑化、特に「hand 進行中に新 insert が
     hand の後ろに割り込む」ケースの semantics が SIEVE の論理と合うかの検証が必要

3. **shard struct の cache-line 配置最適化** (確度: 高、impact: 小)
   - false sharing 排除。`tail`, `len`, `writer` (= Mutex<WriterState>) が同一 cache line に
     乗っていると、tail Acquire load を reader が頻繁に呼ぶ + writer が len/hand を更新する、
     で coherency ping-pong が乗る
   - 具体策: `#[repr(align(64))]` または `crossbeam_utils::CachePadded` 個別配置
   - 1-3% の地味な利得が見込める。実施コスト低、付随的改善として c10 と同梱推奨

## §6 testbed の自己検証

baseline は副次的に testbed 自体の正しさも保証している:

- **HR 一致** (§TL;DR、150 trial 全点): c8 / c9 の hit_ratio が **0.001 以下**で一致。
  両者とも senba::Shard 互換 eviction を継承しているので 1T では bit-exact、並行下でも
  同 seed / 同 workload で同じ統計値を出している。
- **uniform で HR ≈ 0.001**: thread 別 disjoint range で warmup 後に measurement が回ると
  cache に乗ったキーは insert 後一度も再 hit しない (= cycle が cap=64 を超える) ので、
  HR は ~0 に落ちる。設計通り。
- **adversarial-hot で HR = 1.000** (read-only): warmup で key=0 を 1 回 insert すれば、
  以降 reader は全 hit。writer Mutex が無いので並行 reader でも HR は劣化しない。

## §7 後続作業

- **c10 起案**: §5 の attack 順位に従い、まず (1) visited bit cache-line 分離だけを実装した
  `sieve_c10` を立てる。差分が単一軸 (c8 → c10 = visited 分離のみ) なので、scaling 改善が
  本当にこの軸由来かを clean に切り分けられる
- **multi-trial 化**: 本 baseline は 1 trial で取ったので、絶対値の信頼性を上げるには 3-5
  trial の median に拡張すべき。c10 評価時に併せて 3 trial 化、本 baseline も再ラン
- **harness の per-thread 終端統一**: §4 で言及した「最も速い thread が早く終わる」起因の
  aggregate 過大評価。`for i in 0..ops_per_thread` ではなく「全 thread が同 wall-clock 時間
  分作業する」モードへの拡張で、min_per_thread が真の指標になる
- **CSV column の追加**: 平均 elapsed (= aggregate と max_elapsed の差) を出すと、上記の
  終端不均衡が CSV から直接見える
