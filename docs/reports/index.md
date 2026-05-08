# docs/reports インデックス

実験・調査ごとのレポート一覧。新しいレポートを追加したらこのファイルも更新する。

各エントリは「何について書かれているか + さらっと結論」のみ。具体の数値・スコープ・
反証・図表はリンク先に置く方針。

## テーマ別マップ

| テーマ | 関連レポート |
|---|---|
| ベンチ条件の整備 | realistic-workload-bench |
| v0〜v3 系列 (linked-list + array) | v0-divergence, realistic-workload-bench, v3-bench, v3-profile |
| orig 改善 (MaybeUninit) | sieve-orig-overhead-analysis |
| 外部実装調査 | jedi-vs-orig |
| 外部実装比較 (W-TinyLFU) | j8-vs-mini-moka-twitter, mokabench-arc-traces, external-lib-sweep |
| 設計アイディア集 (living doc) | `../improvement-ideas.md` (旧 `2026-05-04-improvement-ideas.md`、`docs/` 直下に移動) |
| j3 系列 (Map なし SIMD) | sieve-j3-bench |
| j4 系列 (set-associative j3) | sieve-j4-set-associative, sieve-j4-crossover-and-shard-sweep, sieve-j4-pershard-vs-footprint |
| j5 系列 (j4 の double-hash 排除) | sieve-j5-doublehash-ab, j5-pershard-pareto, j5-twitter-pareto, j5-vs-orig-2x-memfair |
| j6 系列 (M2.1: visited を tag に同居) | sieve-j6-m21-twitter |
| j7 系列 (M2.3: tag を u16 化、visited + 14-bit hash) | sieve-j7-m23-twitter, j7-twitter-pareto |
| j8 系列 (M5.3 + tag 内 ID embed + free_list 廃止) | sieve-j8-bench, j8-candidate-loop-analysis, j8-c-hoist, j8-twitter-pareto, find-avx2-frontier, find-avx2-pext, find-avx2-pdep-pext-revert, find-avx2-avx512, find-avx2-caller-merge |
| c8 系列 (j8 並行版: read lock-free + write per-shard Mutex) | c8-design, c8-vs-moka-thread-sweep |
| c9 系列 (senba::Cache 並行版: per-shard Mutex<Shard> wrap、V: Clone) | c9-design, c8-vs-c9-thread-sweep |
| 単一 shard testbed (c10/c11/c12/c13 設計の出発点) | single-shard-baseline, c10s-vs-c8-baseline, c11s-conditional-set, c12s-cas-slot-claim-design, c12s-cas-slot-claim, c13s-sweep |
| 5 cluster ベース sweep (cluster006/016/018/019/034) | st-twitter-5cluster |
| ライブラリ化 (`senba::Cache` 公開 API) | senba-sievecache-design, twitter-string-keys, senba-twitter-string-sweep, sieve-cache-shift-on-evict, inline-design-cache-vs-inner, api-comparison-moka-lru → `docs/api-comparison.md` に昇格 |

---

## 一覧 (日付昇順)

### 2026-05-03-sieve-v0-divergence.md
`sieve_v0` が `sieve_orig` と eviction 列で分岐する条件の調査。`evict_one` の hand wrap
バグとして特定して修正、oracle 一致を回復。

### 2026-05-03-realistic-workload-bench.md
ベンチ条件を NSDI'24 論文準拠 (skew / cap sweep, 1M req trace) に整備し直して
orig/v0/v1/v2 を再評価。orig が全条件で最速、v1 はむしろ退行、v2 は微改善という結論。

### 2026-05-03-sieve-v3-bench.md
v1 (bit-parallel scan) と v2 (Option 剥がし) を合流した v3 の AB。orig には届かず、
2-pass 化は Zipf steady state では効かないと結論。

### 2026-05-03-sieve-v3-profile.md
v3 が orig に負ける原因の phase 別プロファイル分解 (samply + addr2line)。hit path RMW /
eviction bookkeeping / compact が主因で、scan ブロック自体は支配的でないと判明。

### improvement-ideas.md (living doc, 旧 `2026-05-04-improvement-ideas.md`)
日付別レポートではなく改善案の倉庫。本体は `docs/improvement-ideas.md` を参照。

### 2026-05-04-jedi-vs-orig.md
既存 Rust 実装 `jedisct1/rust-sieve-cache` の設計読解。`swap_remove` 由来で
SIEVE oracle と一致せず CLOCK 寄りに縮退、と判定。

### 2026-05-04-sieve-orig-overhead-analysis.md
C リファレンスと Rust ポートの差を機械語レベルで分析し、`Vec<MaybeUninit<Node>>` 化を実装。
asm は期待通り改善するが bench はノイズ範囲、HashMap 支配下では埋もれると確認。

### 2026-05-04-sieve-j3-bench.md
外部 HashMap を廃止し tag 配列 AVX2 SIMD scan で lookup する `sieve_j3` の初回ベンチ。
低 cap / 中 skew で orig 超え、cap が増えると線形 scan が破綻すると判明。

### 2026-05-05-sieve-j4-set-associative.md
j3 を set-associative 化した `sieve_j4` の初回ベンチ。per-shard ≥ 64 で hit ratio tax が消え、
特定の cap/skew 帯で orig を上回る勝ち帯が出現。

### 2026-05-05-sieve-j4-crossover-and-shard-sweep.md
j4 の cap 軸 sweep と SHARDS sweep で crossover と最適 shard 数を地図化。
throughput と hit ratio が逆向きのトレードオフになることを示す。

### 2026-05-05-sieve-j4-pershard-vs-footprint.md
「per_shard と total footprint のどちらが支配変数か」を 3 sweep で切り分け。
L1d 境界仮説 (H2) を棄却し、per_shard がほぼ単独支配と確定。

### 2026-05-05-sieve-j5-doublehash-ab.md
j4 の double-hash (shard 選択 + tag 計算で 2 回) を排除した `sieve_j5` の AB。
Δ(j5−j4) を定常コストとして定量し、以降の比較基準を j4 → j5 に更新。

### 2026-05-05-j5-pershard-pareto.md
per_shard × cap × skew の直積 sweep で hit ratio と throughput の Pareto を取り、
sweet spot per_shard を確定。「shard 細分化で SIEVE が崩れる」直観は反証。

### 2026-05-05-j5-twitter-pareto.md
Twitter trace 3 cluster での j5 vs orig 総決算 sweep。scan-heavy cluster で
throughput と hit ratio の二重勝ちが出る、という新利得を示す。

### 2026-05-05-j5-vs-orig-2x-memfair.md
j5 の `order_cap = 2 * cap` を「memory hand-out のおかげ説」と疑い、orig に 2x cap を渡した
worst-case ハンデで再測定。3 レジームに分離して memory advantage 仮説を切り分け。

### 2026-05-05-sieve-j6-m21-twitter.md
M2.1 (visited を tag バイトに同居させて Entry padding を消す) の `sieve_j6` を Twitter で AB。
inline footprint は改善するが throughput は j5 より退行、hit-path 改善の事前予想は棄却。

### 2026-05-05-sieve-j7-m23-twitter.md
M2.3 (tag を u16 化して live + visited + 14-bit hash を同居) の `sieve_j7` を Twitter で AB。
j5/j6 を支配し、j6 の劣化主因が tag bit 数の不足だったと確定。

### 2026-05-05-j7-twitter-pareto.md
j7 vs orig の Twitter 総決算 sweep (j5 sweep と同枠)。Pareto 支配セルが j5 sweep の倍に拡大、
per_shard sweet spot が広がり j7 の優位を確認。

### 2026-05-05-sieve-j8-bench.md
M5.3 + tag 内 entry_id embed + free_list 廃止の `sieve_j8` 初回検証。eviction 列は j7 と
bit-exact 一致、throughput は j7 より退行するが per_shard=16 で全 cap orig 超え (sweet spot
判明)。退行原因は inner candidate ループの dep chain 延長 + false-match 率増の 2 成分。

### 2026-05-06-j8-candidate-loop-analysis.md
j8 退行を inner candidate ループ単一構造として再解釈した解析ノート。命令レベル最適化
(BLSR×2 + sizeof-aware bit layout) を併せて実装し、最退行 cell で大幅改善・sweet spot で
orig 超え。

### 2026-05-06-j8-c-hoist.md
chunk 先頭 byte pointer を outer に hoist する最適化 (c-hoist) を j8 に適用。inner ループから
3 ops を追い出して大半の cell で改善、運用 sweet spot で 3 cap 全て orig を absolute 超え。

### 2026-05-06-j8-twitter-pareto.md
j8 累積最適化 (M5.3 + tag-id embed + free_list 廃止 + BLSR×2 + sizeof-aware layout + c-hoist)
を Twitter sweep に流して champion を選定したスナップショット。memory 利得を保ったまま
3/4 cluster で throughput 勝ち。

### 2026-05-06-j8-vs-mini-moka-twitter.md
`sieve_j8` と外部 W-TinyLFU 実装 (mini-moka 0.10 / moka 0.12) を Twitter trace + Zipf sweep
で比較。Twitter では HR・ns/op どちらも j8 が支配、Zipf では W-TinyLFU が SIEVE と競合。
「W-TinyLFU は Zipf-like で強く、non-Zipf 実 trace で脆い」という対比を示す。

### 2026-05-06-c8-design.md
`sieve_c8` (j8 並行版) の設計と第一手実装。**seqlock-via-tag** で reader を lock-free 化し
parking_lot Mutex で writer 直列化。1T overhead は事前見積もりを上回るが 4T で線形に近い
scaling を確認。

### 2026-05-06-st-twitter-5cluster.md
cluster016 / cluster034 を加えた 5 cluster で orig / j8 / moka / mini-moka を ST 再測定。
j8 の利得帯を Zipf ≤ 1.5 に拡張、退行は「Zipf ≥ 1.79 + 高 hit ratio」の構造特性と確定。

### 2026-05-06-c8-vs-moka-thread-sweep.md
c8 / moka 0.12 / mini-moka 0.10 を同一 harness で並列比較。SHARDS=256 で c8 は near-linear
scaling、moka 0.12 は thread 増で逆 regress、mini-moka はピーク後微減と整理。c8 の
lock-free read + per-shard Mutex モデルが高並列で大幅優位。

### 2026-05-06-senba-sievecache-design.md
publishable な crate API として ST 版 `senba::Cache` を確定する設計ドキュメント (実装着手前)。
`SlotSize` sealed trait による padding 自動化、任意 K, V の `remove` 対応、c-hoist trick の
保持を確定。並行版・builder 化等は scope 外。

### 2026-05-06-twitter-string-keys.md
`senba::Cache` を String キーで Twitter trace に直接流す経路を bench に追加。HR は pre-hash
版と完全一致、orig-string 比でも大幅高速と確認。

### 2026-05-06-senba-twitter-string-sweep.md
`senba::Cache` を生 String キーのまま Twitter 5 cluster で sweep。全 cell で orig を支配し、
scan-heavy cluster019 では HR でも勝つ二重勝ちを再現。

### 2026-05-06-sieve-c-vs-senba-twitter52.md
libCacheSim の C リファレンス SIEVE と senba (`sieve_orig` / `senba::Cache`) を同一 trace
・同一 cap で wall-clock 比較。HR は完全一致、wall-clock は senba が大幅速で、gap の大半は
cachesim の harness (CSV パース + vtable + glib) 由来と分解。

### 2026-05-06-sieve-cache-shift-on-evict.md
`senba::Cache` から `compact` 経路と `tail` フィールドを撤去し shift-on-evict 化した refactor
記録。`tags` 配列を半減、perf-gate 3 シナリオすべてで改善。途中で却下した素朴な in-place
reuse が visited bit 絡みで oracle と発散する点もメモ。

### 2026-05-06-api-comparison-moka-lru.md → `docs/api-comparison.md`
**昇格済み**: `senba::Cache` を moka / lru / quick_cache / stretto と公開メソッド単位で
横並び比較したドキュメント。欠落 API のチェックリストを兼ねるため living document として
`docs/api-comparison.md` に移動。

### 2026-05-07-inline-design-cache-vs-inner.md
`Inner` を `inner.rs` に切り出した際の perf 退行をきっかけに、`Cache::op → Inner::op
→ helper` の3層で `#[inline]` をどこに置くべきか整理。HashMap 流の「公開API は inline
の thin wrapper、その奥の worker をアトム (non-inline)、アトム内部の小さい helper は
inline」が正解で、perf-gate でも insert_string -4〜-9% の改善を確認。`Inner::*` に
`#[inline]` を撒くのはコード肥大方向の bias で筋が悪い、という設計原則も明文化。

### 2026-05-08-sieve-c9-design.md
`sieve_c9` (senba::Cache の最新 ST 実装 = j8 + shift-on-evict + AlignedTags を、
per-shard `Mutex<Shard>` で wrap した並行版) の設計 + bench 比較計画。`V: Clone` で
業界主流 (moka / quick_cache / jedisct1) に整合する API 形を取り、c8 (V: Copy +
seqlock-via-tag) とは別アルゴリズムとして並走させる。本 spec は P1 (c9 設計確定) +
P2 (bench harness 拡張 + sweep 計画確定) まで。正式版 `senba::concurrent::Cache`
への昇格 (P3) はスコープアウト。

### 2026-05-07-aligned-tags-load.md
`Shard::find_avx2` の SIMD load を `Vec<u16>` + `loadu` から `AlignedTags`
(`Vec<TagsChunk>` + `repr(align(32))`) + `_mm256_load_si256` に切り替えた記録。
Twitter trace で u64 -3.35% / String -4.39% (geomean、32 cells)。disasm 比較で
LLVM が両 intrinsic を `vpand ymm, ymm, m256` に fold するため命令選択そのものは
等価と判明、効果の正体は **glibc malloc の 16B 揃えで base mod 64 ∈ {16, 48} の
50% で起きていた cache-line split の解消**。criterion `insert_string` だけは +5%
退行 (シナリオ固有のヒープレイアウト依存 noise)、Twitter で逆方向に改善するため
adopt。`debug_assert!` で alignment invariant を docs としてコードに刻むのと、
将来 LLVM が memory-operand fold をやめた場合の保険として aligned intrinsic も
維持。

### 2026-05-08-find-avx2-frontier.md
`find_avx2` 関数内側 (`j8-c-hoist` で BLSR×2 + bit layout + chunk hoist 適用済み) は最適に
近いが、生 asm を読み直すと **caller との縫い目** で hit ごとに「`tags[pos]` の再 load
+ shift round-trip 4 op」が発火していると判明。原因は (1) `find` が `Option<usize>` (=
pos のみ) を返すため tag が SSA 的に消える、(2) `entry_ptr` が `entries[id]` の bounds
check 経由で LLVM が `((tag & MASK) >> SHIFT) << SHIFT == tag & MASK` を畳めない、の
2 つ。`find` を `Option<(pos, tag)>` 化 + `entry_ptr` を raw pointer 算術化で hit path
−5〜−7 cy の机上見込み。同時に Slot16 monomorph 限定で `vpbroadcastd` がループ内
再構築されている点と `Shard::len` が 2nd cache line に落ちている点も観測。本稿は解析
ノート (実測なし) で、Tier-S 4 件 + Tier-A (inner unroll ×2 / 4-chunk specialization)
+ Tier-B (SoA tag split) の着手順を提案。

### 2026-05-08-c8-vs-c9-thread-sweep.md
P2: c8 (lock-free seqlock + AtomicU16 visited) vs c9 (per-shard `Mutex<Shard>` wrap)
vs moka 0.12 vs mini-moka 0.10 を 4 variant × 5 thread (1〜16) × 3 skew (0.7/1.0/1.2)
× 2 op-mix (gim / read-heavy 95-5) で sweep。**結論は明確に c8 ベース**: 1T と低 skew
では c9 が +8〜26% 上回る (uncontended Mutex < seqlock dance) が、scaling 側は skew で
完全に分かれ、skew=1.2 / 16T では c8 92.5 Mops vs c9 10.6 Mops と **8.7x の差** が出る
(c9 は 8T 以降で逆 scale)。read-heavy でも同形 (hot Mutex は reader も詰まらせる)。
HR は c8/c9 完全一致 (両者 senba::Cache の shift-on-evict を継承)。p99 chunk latency も
高 skew で c9 が c8 の 8-16x に劣化。P3 (`senba::concurrent::Cache`) は c8 を母体に、
1T fast path を c9 から逆輸入する方向で別 design doc に起案。c10 候補として RwLock per
shard / hot shard sub-shard 分割 / lock-free senba::Shard 化を §6 で提案。

### 2026-05-08-find-avx2-pdep-pext-revert.md
`find-avx2-pext.md` の **P3 (PDEP needle 構築)** と **P2 (PEXT で pair-mask →
lane-mask + inner unroll ×2)** を順に投入し、両方とも採用基準を満たさず revert
した試行 + 教訓レポート。P3 は asm レベルでは 7 → 4 命令 / dep chain 5 → 3 に短縮
できたが perf-gate は 6 シナリオすべて criterion "within noise threshold"、後段の
baseline 取り直しで同機械が ±2-3% 揺れることが判明し「improvement に見えた偶然
の上振れ」と再評価。P2 は 3 シナリオで +3.5〜+4.9% の明確な regression。asm 確認
で **(a) LLVM が `cmp1 → load p2` の hoist を出さず load 並列化が実現していない、
(b) PEXT prelude を毎 chunk 払うが per_shard=64 の cand 分布上 unroll ×2 がほぼ
発火しない**、の 2 点が原因と判明。前報机上見積が overestimate だった理由 (PEXT
毎 chunk コスト / N_cand を全 chunk 平均と取り違え / LLVM hoist 期待) を §3 教訓
として整理。再挑戦するなら B1 (SoA tag split) との比較を先、ないし `asm!` 直書きで
LLVM hoist 限界を自前で潰してから perf-gate に進む順序を §4 に明記。code は HEAD
に revert、本稿のみが本セッションの artifact。

### 2026-05-08-find-avx2-pext.md
`find-avx2-frontier.md` §C2 で「Zen 1/2 で PEXT 激遅、non-portable 前提なら別議論」と
棚上げした BMI2 PEXT/PDEP 採用案を、Zen 1/2 (2017〜2019) のシェア低下と CPUID family
check 1 個で fast path 専用に焼ける見通しが立ったことから解禁、机上検討まで進めたもの。
hot path に効くのは 2 軸: **P2** (PEXT で pair-mask → lane-mask 圧縮 + inner unroll ×2)
が per_shard=64 帯で −3〜−5 cy/scan、Path A の load 並列化と Path B の BLSR ×1 を同時に
取れる本命。**P3** (`needle_from_hash` を PDEP 1 命令化) は依存関係ゼロでクリーン採用、
call ごと −2〜−3 cy。runtime 検出は CPUID vendor + family check (AMD family ≥ 0x19 で
fast PEXT) を推奨、起動コストゼロ。前報 Tier-S/A とは概ね直交、B1 (SoA tag split) とは
P2 が排他で prototype 比較で決着。本稿は解析ノート (実測なし)、推奨着手順は
S1/S2/S3 → P3 → A2 → P2 → (B1 vs P2 prototype 比較)。

### 2026-05-08-single-shard-baseline.md
新 testbed `bench_single_shard` (c8/c9 の **shard 内側 1 個だけ** を N thread で叩く) の
baseline sweep + c10 設計向け方向付け。`SingleShard` trait + closure-based read を
`research/src/single_shard.rs` に新設、c8 内部 `Shard` を pub 化して adapter wrap、c9 は
`with_shards(cap, 1)` で単一 shard 化。3 workload 軸 (zipf×3 skew / adversarial-hot /
uniform) × 3 op-mix × 5 threads × 2 variant = 150 trial。**uniform read-only 16T で c8
352 Mops vs c9 5.25 Mops の 67x 差** (Mutex per shard は contention 無くても scaling
殺す)、**adversarial-hot read-only で c8 も 16T で 31 Mops にプラトー** (visited bit
`fetch_or` の cache-line ping-pong)、**gim 50/50 では c8 すら 1T < 2T** (writer Mutex
coverage が広すぎ) など、c10 設計の attack 順位 (visited bit cache-line 分離 → writer
lock-free claim → false sharing 排除) を定量化した baseline。HR は c8/c9 全 150 点で
0.001 以下の差で一致、testbed 自体の正しさも自己検証済み。

### 2026-05-08-c10s-vs-c8-baseline.md
c10 lineage 第一案 `sieve_c10s` (= c8 から VISITED bit を tags 配列の外に分離して
`Box<[AtomicU64]>` の bit-packed 別領域に出した shard variant) を testbed で c8 と直接
比較。**read-only zipf-1.0 16T で c8 45 → c10s 92 Mops (+102%)、uniform で 342 → 597
(+74%)** と reader 経路は 2x 改善 (仮説「tags 列を MESI Shared 維持にすれば AVX2 scan が
cache miss を被らない」を支持)。ただし **read-heavy zipf 16T では -17〜-21% に regress** —
c8 では fetch_or が tags 4 cache line に分散していたのが、c10s では visited 16 byte の
1 line に集中するため hot key の ping-pong が顕在化、tag scan 清浄化の利得を上回る。
HR は c8 と ±0.001 一致 (1 例外 = read-heavy adv-hot 16T で c10s が +11pp 高い、これは
EMPTY 窓を短縮した副次効果)。実装上の落とし穴 1 件 (update 時 visited を reset でなく SET
する仕様 = sieve_orig の `freq=1` 一致) と回帰 test を実装に同梱。c10 lineage の attack
順位は (1) 単独では片側勝ちと判明したので、c10sw (visited per-entry padding) または
c10w (writer Mutex CAS-claim) との合成が次の検証軸。

### 2026-05-08-c11s-conditional-set.md
c10 lineage の追撃 `sieve_c11s` — c10s からの単一行 diff (reader hit を
`fetch_or` から `if load == 0 { fetch_or }` に変える conditional visited set) を
testbed で c8/c10s と並べて比較。x86 の `lock or` は値変化に関わらず必ず line を
Modified に遷移させるので、zipf hot key のように visited=1 が定常状態の slot で
全 reader が同 cache line を取り合う ping-pong が c10s の bottleneck だった、
というのが仮説。**read-only adversarial-hot 16T で c8 35 → c10s 54 → c11s 594
Mops (c8 比 +1587%、c10s 比 +999%)、read-only zipf-1.2 16T で 36 → 64 → 143 (c10s
比 +121%)、read-only zipf-1.0 16T で 50 → 97 → 130 (c10s 比 +34%)** と、reader
集中軸は c10s に対しても圧勝。1T overhead は adv-hot で +19% (load+branch <
uncontested `lock or` cost)、zipf-1.2 で -10% の混在で許容範囲。HR は c8 と完全一致
(zipf 全帯 ±0.01)、oracle 順序保持。**read-heavy zipf 16T は c10s の regression を
解消できず** (c8 比 -18%、c10s 比 -8%) — reader 改善ではなく writer Mutex critical
section が支配項であることを定量確認、c11w (writer CAS claim) との合成が次の必要軸。
publishable surface 昇格は c11w 結果待ち。

### 2026-05-08-c12s-cas-slot-claim.md
c11s 報告 §5「writer Mutex 律速を c11w / CAS-based slot claim で解く」の実装記録。
**writer Mutex 完全排除** + `hand: AtomicUsize` + tag CAS で pos 所有権を確保 +
`install-at-evicted-pos` で compaction を構造的に廃止する c12s variant。設計文書
(`c12s-cas-slot-claim-design.md`) の主仮説 **「install-at-evicted-pos は SIEVE 外部
等価」が崩壊**: 新 entry が hand 直前の pos に visited=0 で入る → 次 hand wrap で
即 evict 候補 → 「保護期間が短い CLOCK 亜種」に変質し SIEVE algorithm そのものでは
ない。oracle test (`research/tests/oracle.rs`) で eviction stream / cache contents
双方が divergent。throughput 軸では **read-heavy zipf 16T で c8 比 +223% / c11s 比
+269%** (c8 21.1 → c11s 18.4 → c12s 68.0 Mops)、p99 tail latency も c8/c11s の 1/3
以下。一方 HR は SIEVE algorithm 喪失の signature として劣化 (read-heavy zipf-1.0
で -10%、read-only zipf-0.7 で -72%)、これは tuning や workload 選択で救えない。
**判定は不採用**: senba (= NSDI'24 SIEVE library) の仕様違反であり、throughput 3x
は "fast non-SIEVE cache" であって "fast SIEVE cache" ではない。research artifact
として `senba-research::experimental::sieve_c12s` に永続化、後続 lock-free 系変種が
install-at-evicted-pos に手を出さないための reference として残す。次の attack
vector は §5 (3) per-shard sub-sharding (構造的に SIEVE 等価が自明)。

### 2026-05-08-c13s-sweep.md
c12s 不採用 (install-at-evicted-pos が SIEVE 等価性破壊) を踏まえた再設計
`sieve_c13s`。base を senba::Cache lineage (shift-on-evict) に切り替え、**Path A
(= 既存 key の value 更新、eviction を起こさない経路) のみ lock-free CAS 化**、
Path B/C (= warmup install / evict) は writer Mutex 配下に残す。Path A は SIEVE
state machine を一切触らないので並行化しても順序保持が構造的に保証される。
新 VERSION bit (0x4000) を Path A ごとに flip して reader seqlock cycle 検出。
read-heavy 95/5 zipf 16T の主軸で **c11s 比 +43% (zipf-1.0)、+111% (zipf-1.2)**、
HR は sieve_orig と ±0.005 一致。ただし **uniform read-heavy で c11s 比 -67%** の
serious regression (Path A retry 空振り問題、新規 key write が必ず MAX_RETRY=4 を
消費して escalate)、**read-heavy adversarial-hot で HR -0.26** (16T で hot key への
Path A cycle が高頻度発生 → reader seqlock false-miss が API 表面で観測される) と
2 つの不採用要因が残る。両方とも構造的問題ではなく実装 tunable で対処可能 (Path A
MAX_RETRY=1 + find_lockfree AVX2 化 + reader bounded retry)。**判定は条件付き保留、
c14s として再評価**。本変種は「Path A だけ lock-free で SIEVE 等価が両立する」初の
正例として `senba_research::experimental::sieve_c13s` に永続化、c11s + Path A 単独
の効果を切り分ける baseline として活用。

### 2026-05-08-find-avx2-avx512.md
`find-avx2-frontier.md` Tier-C で「non-portable」と棚上げした AVX-512 を、server 用 CPU
での実態的 ubiquity (Intel Xeon Skylake-X+ / Sapphire Rapids+、AMD EPYC Zen 4+) を踏まえ
opt-in 経路として再評価。具体的な勝ち手 3 軸: **V1** (AVX-512 VL + kmask, 256-bit 幅)
で `vpcmpeqw_mask` 直結により `vpmovmskb` (5 cy) と BLSR pair を一掃、per_shard=16 で
−0.8 ns / per_shard=64 で −3 ns、**downclock 無し**。**V2** (zmm 512-bit) で per_shard=64
が 4 chunks → 2 chunks に半減、更に −2 ns、ただし Skylake-X / Cascade Lake で
downclock 懸念のため cargo feature `avx512-zmm` で opt-in。**V5** (V2 + B1 SoA tag split)
で 64 u8 lane を 1 zmm shot 比較、per_shard=64 が outer ループ無しで完結、AVX-512 と
B1 の最も強い相乗。PEXT 系 (P1/P2) は kmask が初めから lane-mask 形式なので **AVX-512
経路では不要**、P3 (PDEP needle) と Tier-S (S1/S2/S3) は SIMD path 非依存で温存。
配布形態は cargo feature `avx512-vl` / `avx512-zmm` の二段で、AVX-512 が無い CPU は
ランタイム detect で AVX2 path に自動 fallback。本稿は解析ノート (実測なし)、推奨
着手順は S1/S2/S3 + P3 → V1 → A2 → V2 と V5 の二択を prototype で詰める。

### 2026-05-08-find-avx2-caller-merge.md
`find-avx2-frontier.md` Tier-S の caller-merge 最適化を 3 試行で詰めて
**採択した実測ノート**。**第 1 試行 S1+S2 は棄却** (find_avx2 が
target_feature 制約で inline されず tuple 返りが sret 化、かつ LLVM が
shift round-trip を畳まないため get_heavy_u64 +5.11%)。**第 2 試行
A3+`#[inline]`** で `entry_ptr_from_tag(tag) = entries + (tag & ID_MASK)`
を hit path 専用ヘルパとして追加し source 側 fold、asm 上 byte offset が
1 op の `and rXd, 2016` に圧縮、perf も get_heavy +1.51% (gate 内) まで
改善 — ただし sret は stable Rust の構造制約 (`#[inline(always)]` +
`#[target_feature]` が E0658 で禁止) で残存。**第 3 試行 NonZeroU16 + A3
+ `#[inline]` で完全達成・採択**: live tag は LIVE bit (0x8000) 立ち
⟹ 非ゼロを利用、`find` 返り型を `Option<(usize, NonZeroU16)>` に変更して
niche optimization で 16 byte に抑え sret 解消。const assert
`size_of::<Option<(usize, NonZeroU16)>>() == 16` を anchor。asm は
`mov rdi, rbx; call find_avx2; test dx, dx; je miss; ...; and edx, 2016;
add rax, rdx` で完全予測通り (sret なし、niche `test dx, dx`、shift
round-trip なし)。perf-gate AB: insert_u64 −7.15% / mixed_u64 −10.14% /
insert_string −3.22% / insert_u32_slot16 −9.24% の 4 シナリオ大勝、
get_heavy_u64 +0.86% (noise 域) / mixed_lowskew_u64 +1.38% で 5% gate
違反なし。Twitter trace cross-check (cluster016/018/019 × cap 4096/16384/32768)
も実施済: HR 9 セル全完全一致、cluster016 (scan-heavy) で −2.82〜−9.25%
の大勝、cluster018/019 で gate 内 +0.4〜+1.6% 退行 (perf-gate の
get_heavy/lowskew と同根)、5% gate 違反なし。perf-gate と Twitter で
方向が揃って採択維持。学び:
sret threshold (16 byte) は ABI 設計のクリフ、niche-bearing 型での
サイズ詰めは積極検討、const assert で固定化、3 段試行で各制約を
独立に閉じる。

### 2026-05-08-mokabench-arc-traces.md
`research/src/bin/bench.rs` に **ARC paper trace** (`S3 / DS1 / OLTP / spc1likeread`)
を扱う `--source arc` を追加。dataset / パーサ意味論の出典は **mokabench**
(<https://github.com/moka-rs/mokabench>) と `cache-trace` submodule。mokabench
本体の load generator は統合せず trace 形式だけ拝借し (理由: mokabench は cache
実装を compile-time feature flag で差し替える構造で senba を載せるには fork が
必要、既存 `bench.rs` driver で全 variant が直接 ARC を食えるほうが軽い)、
`arc_from_path` で `start len` 行を `start..start+len` に展開、`.zst` は zstd で
on-the-fly 展開。比較対象は **moka でなく mini-moka** (moka は background thread +
adaptive window sizing + tokio runtime の overhead が乗り single-thread bench で
速度差が水増しされるため)。スモーク結果: Zipf-1.0 で HR ±0.4pp 一致 / 速度 ~17.6×、
**ARC OLTP cap=4000 で senba HR=51.7% vs mini-moka 45.7% (SIEVE が DB workload で
W-TinyLFU を上回る既知パターンと整合)**、ARC S3 は cap=4000/16000 共に HR <1% で
**両者とも壊滅** (working set が cap を遥かに超える scan-heavy では admission
policy 差は誤差の範囲) — Twitter cluster 単独では見えない事実。**OLTP は perf-gate
scenario 候補、S3 は signal が無く却下、DS1 / spc1likeread は未検証** (spc1likeread
は split zst 連結処理が要追加工事)。

### 2026-05-08-external-lib-sweep.md
`mokabench-arc-traces` 基盤の上で **ARC paper trace 6 種 (OLTP / S3 / P3 / DS1 /
ConCat / MergeP) + Zipf skew=1.0** を **senba::Cache (auto-shard via
`Cache::new(cap)`) / sieve_orig (oracle) vs mini-moka / moka 0.12 (W-TinyLFU)**
で一気に sweep。`Cache::new` は per-shard を 32–64 (= AVX2 batch サイズ ≒
6-bit ID 上限) に収まるよう shards を自動選択するので senba::Cache に
capacity ceiling は無く、auto は orig と HR ±0.3pp 一致。**HR は workload と
cap で SIEVE / W-TinyLFU が反転**: OLTP cap=8000 で SIEVE +7.5pp / MergeP
cap=1M で +5pp、対して DS1 cap=1M で W-TinyLFU +7pp / P3 cap=32k で +8.5pp /
S3 cap=400k で +13pp、Zipf は ±0.4pp tie。**Throughput** は senba が
single-thread 公平条件 (`mini_moka_unsync`) で 2–4× — `mini_moka::sync` /
`moka` は multi-thread 用途の overhead (sync()/background thread/tokio) が
乗るので single-thread 比較からは除外。
副次発見: **working set が cap に収まる帯 (Zipf cap=32k / ConCat cap=1M /
OLTP cap=8000) では senba < orig**。ConCat を cap 軸で並べると `senba/orig`
比が `shards` 数 (2k → 8k → 16k) と逆相関で崩れる (111% → 73% → 58%) ので、
**hot 集合が uniform hash で shards に分散して cacheline 局在を失う**のが
仮説の主因 (orig 2 cacheline vs senba 3 cacheline / op)。検証案は `Slot8`
(256 ent/shard) で shards 数を 1/4 に圧縮 + perf stat で LLC miss 直接計測。
caller-merge (main 38d39f3) 後でも hit-heavy 帯の優劣は変わらず、miss-heavy 帯
(DS1/P3/S3/MergeP) は senba +5〜15%。
前リビジョンの「`senba_n128 cap=256` で HR collapse」は per-shard=2 強制の
artifact で、`Cache::new(cap)` 経由なら起きない (ユーザは shards を選ばなくて良いし
選ぶべきでもない)。次は OLTP/MergeP/Zipf 3 点 perf-gate 候補が follow-up。
