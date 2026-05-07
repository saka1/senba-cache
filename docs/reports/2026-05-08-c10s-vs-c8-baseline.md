# 2026-05-08 sieve_c10s vs c8 — visited 分離 single-shard baseline

- 親: `2026-05-08-single-shard-baseline.md` (c8/c9 baseline、本稿の比較相手)
- variant 実装: `research/src/experimental/sieve_c10s.rs` (c8 から visited を分離)
- raw csv: `docs/reports/data/2026-05-08-single-shard-baseline.csv`
  (c8/c9/c10s × 5 workloads × 3 op-mixes × 5 threads × 1 trial = 225 行)
- 集計 csv: `docs/reports/data/2026-05-08-single-shard-summary.csv`
- 図: `docs/reports/data/2026-05-08-single-shard-{throughput,min-mops,p99}-{read-only,read-heavy,gim}.png`
- driver / plot: `scripts/sweep_single_shard_baseline.sh`、`scripts/plot_single_shard_baseline.py`

## TL;DR

c10s は c8 から **VISITED bit を `tags: Box<[AtomicU16]>` の外に出して `Box<[AtomicU64]>`
に bit-packed (pos 単位)** で持つ単一軸の差分。reader hit 時の `fetch_or` が tags 列の
cache line を invalidate しない構造になり、

- **read-only zipf-1.0 16T で c8 45.3 → c10s 91.5 Mops (+102%)**、**uniform 16T で
  342 → 597 Mops (+74%)** と clean に 2x 近い改善。仮説「tags 列を MESI Shared 維持
  にすれば AVX2 scan が cache miss を被らない」が定量的に支持された。
- **gim adversarial-hot 16T で 31.8 → 53.0 Mops (+67%)** も顕著。reader 経路が支配項の
  軸では visited 分離が一貫して効く。
- **read-heavy zipf 16T では逆に -17〜-21% に regress** (zipf-1.0 で 21.1 → 17.6)。
  visited を 16 byte の単一 cache line に集中させたため、hot key が同 word に並ぶと
  ping-pong が深刻化、**tag scan 清浄化の利得を上回る** ことが原因。
- HR は c8 と完全一致 (zipf 全帯で ±0.001、adversarial-hot read-heavy では c10s が
  +11pp 高い = visited セマンティクス修正後の挙動正常化、§3 参照)。

**結論**: c10s は read-only / scan-bound 軸では明確な勝者だが、`senba::concurrent::Cache`
の主想定軸 (read-heavy zipf) では **visited 列の集中度** が新しい bottleneck になる。
report の attack 順位 (1) は片側だけ正しい仮説で、続く c10sw (visited を per-entry
CachePadded で散らす) または writer 経路の改善 (c10w) が必要。

## §1 結果 — 数値ハイライト

aggregate Mops/s, median 1 trial。c10s vs c8 の差分にフォーカス。

### read-only (clean win)

| workload | threads | c8 | **c10s** | 倍率 |
|---|---:|---:|---:|---:|
| uniform | 1 | 77.5 | 88.5 | 1.14x |
| uniform | 16 | **342.2** | **597.3** | **1.74x** |
| zipf-0.7 | 16 | 93.6 | 133.2 | 1.42x |
| zipf-1.0 | 1 | 18.2 | 19.4 | 1.06x |
| zipf-1.0 | 4 | 31.3 | 68.5 | 2.19x |
| zipf-1.0 | 16 | **45.3** | **91.5** | **2.02x** |
| zipf-1.2 | 16 | 35.7 | 64.7 | 1.81x |
| adversarial-hot | 1 | 72.6 | 75.3 | 1.04x |
| adversarial-hot | 16 | 34.4 | 50.5 | 1.47x |

(`docs/reports/data/2026-05-08-single-shard-throughput-read-only.png`)

観察:
- **zipf 系で 1.4〜2x 近いゲイン**。zipf-1.0 は thread 数増加と共に倍率が伸び続ける
  (1T 1.06x → 4T 2.19x → 16T 2.02x)、scaling 改善が真の効果軸。
- 1T では差が小 (1.04〜1.14x)。これは visited 分離の利得が **並行 reader 同士の coherency
  競合解消** 由来のため、単スレッドでは現れない。逆に 1T で c10s が極端に劣化していない
  ことが「visited 分離は単スレッド性能を犠牲にしていない」証左。
- adversarial-hot 16T (50.5 Mops) は **改善はあるが c8 比 1.5x のみ**。仮説どおり、visited
  自体の ping-pong (= 全 reader が key=0 の同 visited bit を `fetch_or`) は依然残るため
  上限が visited 1 cache line の bandwidth で制限される。

### gim 50/50 (mostly improvement)

| workload | threads | c8 | **c10s** |
|---|---:|---:|---:|
| adversarial-hot | 16 | 31.8 | **53.0** (+67%) |
| zipf-1.0 | 4 | 5.4 | 4.6 (-15%) |
| zipf-1.0 | 16 | 2.2 | 2.1 (噪音) |
| uniform | 16 | 1.86 | 1.68 |

(`docs/reports/data/2026-05-08-single-shard-throughput-gim.png`)

観察: gim は writer Mutex が支配項なので絶対値が低く差分は小さいが、reader 経路が
混じる軸 (adversarial-hot) では c10s が大幅勝。zipf 系は writer 律速で c10s 利得が消える。

### read-heavy 95/5 (regression on zipf)

| workload | threads | c8 | **c10s** | 倍率 |
|---|---:|---:|---:|---:|
| adversarial-hot | 16 | 19.8 | **23.8** | 1.20x |
| uniform | 16 | 22.0 | 19.7 | 0.90x |
| zipf-0.7 | 16 | 21.3 | 16.8 | **0.79x** |
| zipf-1.0 | 4 | 27.8 | 33.6 | 1.21x |
| zipf-1.0 | 8 | 27.4 | 24.9 | 0.91x |
| zipf-1.0 | 16 | 21.1 | 17.6 | **0.83x** |
| zipf-1.2 | 16 | 21.4 | 17.6 | 0.82x |

(`docs/reports/data/2026-05-08-single-shard-throughput-read-heavy.png`)

観察: zipf 系 16T で **15-21% の regression**。1T〜4T では c10s が勝つ (zipf-1.0 4T は
+21%) が、8T 以降で逆転する。**c10s は thread 数のある区間までは scaling し、その後
visited line ping-pong に頭打ちされて落ちる** という形。これは c8 が tag 4 line に
fetch_or を分散していたのに対し、c10s は visited 16 byte (= 1 line) に集中させたことの
直接的副作用。

## §2 解釈 — なぜ仮説が片側だけ正しかったか

c8 の reader hit 時 `tags[pos].fetch_or(VISITED)` には **2 つの coherency cost** が混在:

1. **同 cache line を AVX2 scan で読んでいる他 reader を invalidate** (= scan 路汚染)
2. **同 cache line を別 hit reader が `fetch_or` しに来る → MESI Modified の取り合い**

c10s は visited を 16 byte の独立 Box に分離することで (1) を解消したが、(2) は
**むしろ密度が上がった**: c8 では fetch_or が tags 4 cache line に分散していたのに、
c10s では visited 1 cache line に集中する。

軸ごとの帰結:
- **read-only**: writer 経路が走らないので tag scan 路の clean 化 (1) が支配項 → c10s 大勝
- **read-heavy**: 5% insert で writer Mutex も走り、reader hit の fetch_or 集中 (2) と
  writer Mutex contention が複合 → 16T 規模で c10s のメリットが食いつぶされる
- **gim 50/50**: writer Mutex 律速で reader 経路の差はあまり見えない (ただし adversarial-hot
  だけは reader hit が 100% なので reader 側で勝てる)

## §3 HR (正しさ確認)

c8 と c10s の hit_ratio は基本的に ±0.001 で一致 (= SIEVE eviction semantics 不変)。
1 つの例外:

| op_mix | workload | threads | c8 HR | c10s HR | 差 |
|---|---|---:|---:|---:|---:|
| read-heavy | adversarial-hot | 16 | 0.754 | **0.864** | +11.0 pp |

c10s が高い HR = reader miss が **少ない**、つまり writer の EMPTY 窓中に reader が
seqlock-fail で空 hit する頻度が下がっている。c10s の `writer_update_in_place` を c8 と
比較すると、c10s では visited bit 操作を **EMPTY 窓の外** に出した結果、EMPTY 窓そのものが
短くなった (後述の実装上の偶発利得)。

その他軸 (read-only / gim / read-heavy zipf) は ±0.001 一致。

## §4 実装上の落とし穴 (発見記録)

c10s の **第一稿** (実装直後) は同 read-heavy adversarial-hot 16T で HR が 0.624 と
**c8 (0.763) より低かった**。原因: 当初 `writer_update_in_place` で「更新後の visited
は 0 にリセット」と実装したが、

- **sieve_orig**: `node.freq = 1` (= update で visited を 1 に SET)
- **c8**: `new_tag = old | VISITED` (= update で visited を 1 に SET)
- **c10s 第一稿**: `visited.fetch_and(!b)` (= update で visited を 0 に reset) ← **誤**

oracle の挙動と食い違っていた。`matches_sieve_orig_externally_1shard` test は scrambled
trace の最終状態が偶然一致したため pass していたが、eviction sequence が正しくなく、
read-heavy adversarial-hot のような「同 hot key への update 多発」軸で hot key の survival
率が下がる形で表面化した。

修正:
1. `writer_update_in_place` を **visited SET (`fetch_or`)** に変更 (oracle 一致)
2. visited 操作を **EMPTY 窓の外** に移動 (reader の seqlock-fail miss を最小化)
3. eviction-order 回帰 test (`update_existing_key_sets_visited_like_oracle`) を追加 —
   この test は誤実装を直接捕まえる (cap=2、insert(1), insert(2), insert(1)= update,
   insert(3) → 期待 evict=2、誤実装は evict=1)

教訓: oracle 比較 test (`matches_sieve_orig_externally_1shard`) は **trace 全体の
最終状態** を見るので、**中間 eviction 順序の差を取りこぼす** ケースがある。SIEVE 系の
visited 操作のように細かい semantics 違いは、最小ケース (cap=2 の visited 不変条件) を
直接書く方が捕まえやすい。

## §5 c10 lineage の次の一手

本 baseline で c10s が「片側勝ち」だと判明したので、attack 順位を再構成:

1. **c10sw (visited を per-entry CachePadded で散らす)**: 確度 中、impact 中
   - `Box<[CachePadded<AtomicU8>; cap]>` (cap=64 で 4 KB/shard) に置換
   - read-heavy zipf の regression を解消する仮説検証用。memory cost は multi-shard
     (256 shard) で 1 MB と非自明だが、library scope 内なら許容できる範囲
   - 仮に**改善が見えない**場合は、hot key が key=0 単独 (adversarial-hot) のような
     ケースでも visited line 集中は 1 entry に閉じるので、**ping-pong は per-entry
     padding で解消できない別現象** (ex. tags scan も AVX2 chunk 内 hot lane で
     coherency 拾う) という別仮説に進む

2. **c10w (writer Mutex の lock-free claim)**: 確度 中、impact 中
   - read-heavy で writer Mutex 競合が見えている (c8 でも c10s でも 16T で 21〜18 Mops に
     収束)。CAS-based slot claim で writer coverage を狭める
   - c10s と直交軸なので **c10sw + c10w の合成** = c10sww として最終形にする見込み

3. **c10p (false sharing 排除)**: 確度 高、impact 小
   - shard struct 内 tail/len/writer の CachePadded 配置。地味に 1-3% を狙う

c10s 自体は **publishable surface に乗せるかは保留**。read-heavy zipf で c8 比劣化なので、
`senba::concurrent::Cache` を c10s lineage に切り替えるのは時期尚早。c10sw か c10w で
read-heavy も改善できることを確認してから昇格判断。

## §6 testbed の自己検証

baseline 同様、testbed の正しさも本 sweep で再確認:
- **HR 一致** (§3): c8 と c10s が 225 trial 全点で ±0.001 一致 (例外 1 件は §3 説明済み)
- **HR が trace に依存しない**: `matches_sieve_orig_externally_1shard` (10000 ops trace)
  と `matches_j8_externally_1shard` (同) はどちらも pass、c8 と c10s で同 trace 同 final state
- **新規 invariant test** (`reader_hit_does_not_modify_tag`): c10s の構造的不変条件
  「reader hit が tag を変更しない」を直接検証
- 並行 invariants test (`concurrent_invariants_under_zipf`): 4 thread × 50000 ops Zipf 後の
  per-shard live count / id 集合 / value 一致を確認 (miri 抑制)

## §7 後続作業

- c10sw 起案 + 実装 (visited per-entry padding)、本 testbed で c10s と直接比較
- 実装に同梱した eviction-order 回帰 test (`update_existing_key_sets_visited_like_oracle`)
  を c8 / c9 にも mirror して書く (現状 c10s のみに存在、oracle 比較の死角を埋める意味で
  全 variant に同等 test があるべき)
- multi-trial 化 (3-5 trial median): c10s で見えた -17% regression が trial 1 のみだと
  noise 帯にぎりぎりかかる。3 trial 化で確度を上げるべき
- harness の per-thread 終端統一: §4 baseline 報告の `mops_min_per_thread` ratio 0.5%
  問題は本稿でも未対処
