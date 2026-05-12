# docs/reports インデックス

実験・調査ごとのレポート一覧。新しいレポートを追加したらこのファイルも更新する。

各エントリは **「何の仮説を立てて、何をして、何を得たか」を 1 段落で** の方針。具体の数値・スコープ・反証・図表はリンク先に置く。1 段落 3〜5 行を上限とし、これを超える場合は要約しすぎ・分割しすぎを疑う。

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
| 並行 write contention の道具箱 (hot-key 対策、LongAdder visited) | write-contention-design-space, visited-bitmap, c14s-vtune-write-contention, c16s-design, c16s-results |
| r 系列 (shard 間 routing × thread affinity の解空間) | r1-design, r1-results |
| MT overhead 構造分析 (c/r-series vs lib の絶対値 ceiling) | mt-overhead-vs-lib |
| partitioned baseline (`senba::concurrent::PartitionedCache`, lib surface) | partitioned-design, partitioned-results, partitioned-vtune |
| 5 cluster ベース sweep (cluster006/016/018/019/034) | st-twitter-5cluster |
| ライブラリ化 (`senba::Cache` 公開 API) | senba-sievecache-design, twitter-string-keys, senba-twitter-string-sweep, sieve-cache-shift-on-evict, inline-design-cache-vs-inner, api-comparison-moka-lru → `docs/api-comparison.md` に昇格 |

---

## 一覧 (日付昇順)

### 2026-05-03-sieve-v0-divergence.md
仮説: `sieve_v0` が orig と eviction 列で分岐するのは実装バグだろう。`evict_one` の hand wrap を追跡し、修正して oracle 一致を回復。

### 2026-05-03-realistic-workload-bench.md
ベンチ条件を NSDI'24 論文準拠 (skew / cap sweep, 1M req trace) に整備し直して orig/v0/v1/v2 を再評価。orig が全条件で最速、v1 はむしろ退行、v2 は微改善という結論。

### 2026-05-03-sieve-v3-bench.md
仮説: v1 (bit-parallel scan) と v2 (Option 剥がし) を合流すれば orig を抜けるはず。AB の結果届かず、2-pass 化は Zipf steady state では効かないと結論。

### 2026-05-03-sieve-v3-profile.md
v3 が orig に負ける原因を phase 別に分解 (samply + addr2line)。hit path RMW / eviction bookkeeping / compact が主因で、scan ブロック自体は支配的でないと判明。

### improvement-ideas.md (living doc, 旧 `2026-05-04-improvement-ideas.md`)
日付別レポートではなく改善案の倉庫。本体は `docs/improvement-ideas.md` を参照。

### 2026-05-04-jedi-vs-orig.md
既存 Rust 実装 `jedisct1/rust-sieve-cache` の設計読解。`swap_remove` 由来で SIEVE oracle と一致せず CLOCK 寄りに縮退、と判定して詳細 bench は見送り。

### 2026-05-04-sieve-orig-overhead-analysis.md
仮説: C リファレンスと Rust ポートの差は機械語に出るはず。比較して `Vec<MaybeUninit<Node>>` 化を実装。asm は期待通り改善するが bench はノイズ範囲、HashMap 支配下では埋もれると確認。

### 2026-05-04-sieve-j3-bench.md
仮説: 外部 HashMap を廃止し tag 配列 AVX2 SIMD scan で lookup すれば速くなる。低 cap / 中 skew で orig 超え、cap が増えると線形 scan が破綻すると判明。

### 2026-05-05-sieve-j4-set-associative.md
仮説: j3 を set-associative 化すれば cap スケール問題が解ける。per-shard ≥ 64 で hit ratio tax が消え、特定の cap/skew 帯で orig を上回る勝ち帯が出現。

### 2026-05-05-sieve-j4-crossover-and-shard-sweep.md
j4 の cap 軸 sweep と SHARDS sweep で crossover と最適 shard 数を地図化。throughput と hit ratio が逆向きのトレードオフになることを示す。

### 2026-05-05-sieve-j4-pershard-vs-footprint.md
仮説: per_shard と total footprint のどちらが支配変数か (H1 vs H2: L1d 境界)。3 sweep で切り分け、L1d 境界仮説 (H2) を棄却し per_shard がほぼ単独支配と確定。

### 2026-05-05-sieve-j5-doublehash-ab.md
仮説: j4 の double-hash (shard 選択 + tag 計算で 2 回) を排除すれば常時得。AB で Δ(j5−j4) を定常コストとして定量し、以降の比較基準を j4 → j5 に更新。

### 2026-05-05-j5-pershard-pareto.md
per_shard × cap × skew の直積 sweep で hit ratio と throughput の Pareto を取り、sweet spot per_shard を確定。「shard 細分化で SIEVE が崩れる」直観は反証。

### 2026-05-05-j5-twitter-pareto.md
Twitter trace 3 cluster での j5 vs orig 総決算 sweep。scan-heavy cluster で throughput と hit ratio の二重勝ちが出る、という新利得を示す。

### 2026-05-05-j5-vs-orig-2x-memfair.md
仮説: j5 の `order_cap = 2 × cap` は実は memory hand-out の利得を借りているのでは。orig に 2x cap を渡した worst-case ハンデで再測定し、3 レジームに分離して memory advantage 仮説を切り分け。

### 2026-05-05-sieve-j6-m21-twitter.md
仮説 (M2.1): visited を tag バイトに同居させて Entry padding を消せば throughput も上がるはず。Twitter で AB → inline footprint は改善するが throughput は j5 より退行、事前予想は棄却。

### 2026-05-05-sieve-j7-m23-twitter.md
仮説 (M2.3): tag を u16 化して live + visited + 14-bit hash を同居させれば j6 の劣化を解消できる。Twitter で AB → j5/j6 を支配し、j6 劣化主因が tag bit 数の不足だったと確定。

### 2026-05-05-j7-twitter-pareto.md
j7 vs orig の Twitter 総決算 sweep (j5 sweep と同枠)。Pareto 支配セルが j5 sweep の倍に拡大、per_shard sweet spot が広がり j7 の優位を確認。

### 2026-05-05-sieve-j8-bench.md
仮説 (M5.3 + tag 内 entry_id embed + free_list 廃止): inline footprint を更に削れる。eviction 列は j7 と bit-exact 一致、throughput は j7 より退行するが per_shard=16 で全 cap orig 超え (sweet spot 判明)。退行原因は inner candidate ループの dep chain 延長 + false-match 率増の 2 成分。

### 2026-05-06-j8-candidate-loop-analysis.md
j8 退行を inner candidate ループ単一構造として再解釈。命令レベル最適化 (BLSR×2 + sizeof-aware bit layout) を実装し、最退行 cell で大幅改善・sweet spot で orig 超え。

### 2026-05-06-j8-c-hoist.md
仮説: chunk 先頭 byte pointer を outer に hoist すれば inner ループが軽くなる。j8 に適用、inner から 3 ops 追い出して大半の cell で改善、運用 sweet spot で 3 cap 全て orig を absolute 超え。

### 2026-05-06-j8-twitter-pareto.md
j8 累積最適化 (M5.3 + tag-id embed + free_list 廃止 + BLSR×2 + sizeof-aware layout + c-hoist) を Twitter sweep に流して champion を選定。memory 利得を保ったまま 3/4 cluster で throughput 勝ち。

### 2026-05-06-j8-vs-mini-moka-twitter.md
仮説: SIEVE と W-TinyLFU は workload で勝敗が分かれるはず。`sieve_j8` vs mini-moka 0.10 / moka 0.12 を Twitter trace + Zipf sweep で比較し、Twitter で j8 支配、Zipf で W-TinyLFU が競合、という対比を示す。

### 2026-05-06-c8-design.md
`sieve_c8` (j8 並行版) の設計と第一手実装。**seqlock-via-tag** で reader を lock-free 化し parking_lot Mutex で writer 直列化。1T overhead は事前見積もりを上回るが 4T で線形に近い scaling を確認。

### 2026-05-06-st-twitter-5cluster.md
cluster016 / cluster034 を加えた 5 cluster で orig / j8 / moka / mini-moka を ST 再測定。j8 の利得帯を Zipf ≤ 1.5 に拡張、退行は「Zipf ≥ 1.79 + 高 hit ratio」の構造特性と確定。

### 2026-05-06-c8-vs-moka-thread-sweep.md
c8 / moka 0.12 / mini-moka 0.10 を同一 harness で並列比較。SHARDS=256 で c8 は near-linear scaling、moka 0.12 は thread 増で逆 regress、mini-moka はピーク後微減。c8 の lock-free read + per-shard Mutex モデルが高並列で大幅優位。

### 2026-05-06-senba-sievecache-design.md
publishable な crate API として ST 版 `senba::Cache` を確定する設計ドキュメント (実装着手前)。`SlotSize` sealed trait による padding 自動化、任意 K, V の `remove` 対応、c-hoist trick の保持を確定。並行版・builder 化等は scope 外。

### 2026-05-06-twitter-string-keys.md
`senba::Cache` を String キーで Twitter trace に直接流す経路を bench に追加。HR は pre-hash 版と完全一致、orig-string 比でも大幅高速と確認。

### 2026-05-06-senba-twitter-string-sweep.md
`senba::Cache` を生 String キーのまま Twitter 5 cluster で sweep。全 cell で orig を支配し、scan-heavy cluster019 では HR でも勝つ二重勝ちを再現。

### 2026-05-06-sieve-c-vs-senba-twitter52.md
libCacheSim の C リファレンス SIEVE と senba (`sieve_orig` / `senba::Cache`) を同一 trace・同一 cap で wall-clock 比較。HR は完全一致、wall-clock は senba が大幅速で、gap の大半は cachesim の harness (CSV パース + vtable + glib) 由来と分解。

### 2026-05-06-sieve-cache-shift-on-evict.md
仮説: `compact` 経路と `tail` フィールドを撤去して shift-on-evict 化すれば `tags` 配列を半減できる。`senba::Cache` から撤去 → perf-gate 3 シナリオすべて改善。途中で却下した素朴な in-place reuse が visited bit 絡みで oracle と発散する点もメモ。

### 2026-05-06-api-comparison-moka-lru.md → `docs/api-comparison.md`
**昇格済み**: `senba::Cache` を moka / lru / quick_cache / stretto と公開メソッド単位で横並び比較したドキュメント。欠落 API のチェックリストを兼ねるため living document として `docs/api-comparison.md` に移動。

### 2026-05-07-inline-design-cache-vs-inner.md
仮説: `Inner` を `inner.rs` に切り出した際の perf 退行は `#[inline]` 配置の問題。`Cache::op → Inner::op → helper` の 3 層で配置を整理し、HashMap 流の「公開 API は inline thin wrapper、worker はアトム (non-inline)、内部 helper は inline」が正解と判明、perf-gate で insert_string −4〜−9% 改善。`Inner::*` に `#[inline]` を撒くのは筋が悪い、と原則化。

### 2026-05-08-sieve-c9-design.md
`sieve_c9` (senba::Cache 最新 ST = j8 + shift-on-evict + AlignedTags を per-shard `Mutex<Shard>` wrap) の設計 + bench 比較計画。`V: Clone` で moka / quick_cache / jedisct1 と整合する API 形を取り、c8 (V: Copy + seqlock-via-tag) と別 algorithm として並走。本 spec は P1 (設計) + P2 (sweep 計画) まで、`senba::concurrent::Cache` 昇格はスコープアウト。

### 2026-05-07-aligned-tags-load.md
仮説: `find_avx2` の SIMD load を `Vec<u16>` + `loadu` から `AlignedTags` (`repr(align(32))` + `_mm256_load_si256`) に切り替えれば cache-line split が消える。Twitter trace で u64 −3.35% / String −4.39% (geomean、32 cells)。disasm 比較で命令選択は等価 (LLVM が `vpand m256` に fold) と判明、効果の正体は **glibc malloc 16B 揃えで base mod 64 ∈ {16, 48} の 50% で発生していた cache-line split の解消**。`debug_assert!` で alignment 不変条件を刻んで採択。

### 2026-05-08-find-avx2-frontier.md
解析ノート (実測なし)。仮説: `find_avx2` 内側 (c-hoist 適用済) は最適に近いが、**caller との縫い目** で hit ごとに「`tags[pos]` 再 load + shift round-trip 4 op」が発火している。原因を `find` 返り型 (tag が SSA 的に消える) と `entry_ptr` の bounds check (LLVM が shift 簡約を畳めない) の 2 つに同定し、Tier-S 4 件 + Tier-A (inner unroll ×2 / 4-chunk specialization) + Tier-B (SoA tag split) の着手順を提案。

### 2026-05-08-c8-vs-c9-thread-sweep.md
c8 vs c9 vs moka 0.12 vs mini-moka 0.10 を 4 variant × 5 thread × 3 skew × 2 op-mix で sweep。**結論は明確に c8 ベース**: 1T と低 skew では c9 が +8〜26% 上回るが skew=1.2 / 16T で c8 92.5 vs c9 10.6 Mops と **8.7× 差**、c9 は 8T 以降で逆 scale。HR は完全一致。c10 候補として RwLock per shard / hot shard sub-shard / lock-free senba::Shard 化を §6 で提案。

### 2026-05-08-find-avx2-pdep-pext-revert.md
P3 (PDEP needle) と P2 (PEXT で pair-mask → lane-mask + inner unroll ×2) を投入、**両方 revert**。P3 は asm では 7 → 4 命令 / dep chain 5 → 3 だが perf-gate noise threshold 内、後段 baseline 取り直しで「improvement に見えた偶然の上振れ」と判定。P2 は 3 シナリオで +3.5〜+4.9% regression。原因は (a) LLVM が cmp 越し load 並列化を出さない、(b) per_shard=64 cand 分布で unroll ×2 がほぼ発火しない、の 2 点。再挑戦時の前提条件 (B1 比較先行 / `asm!` で hoist 強制 / calibration 2 回) を §4 に明記。

### 2026-05-08-find-avx2-pext.md
解析ノート (実測なし)。Zen 1/2 PEXT 激遅問題を CPUID family check で迂回する見通しが立ったため、棚上げしていた BMI2 PEXT/PDEP を解禁して机上検討。P2 (PEXT で pair-mask 圧縮 + unroll ×2) が per_shard=64 帯 −3〜−5 cy/scan の本命、P3 (`needle_from_hash` を PDEP 1 命令化) は依存関係ゼロでクリーン採用候補。Tier-S/A とは概ね直交、B1 (SoA tag split) とは P2 が排他。後続の pdep-pext-revert で実測投入され両方 revert された。

### 2026-05-08-single-shard-baseline.md
c10 設計向けに **shard 内側 1 個だけを N thread で叩く** testbed `bench_single_shard` を新設。3 workload × 3 op-mix × 5 threads × 2 variant = 150 trial で baseline 採取。**uniform read-only 16T で c8 352 vs c9 5.25 Mops の 67× 差** / **adv-hot read-only で c8 も 16T で 31 Mops にプラトー** (visited bit ping-pong) / **gim 50/50 では c8 すら 1T < 2T** (writer Mutex coverage 過大) を観測、c10 設計の attack 順位 (visited 分離 → writer lock-free claim → false sharing 排除) を定量化。

### 2026-05-08-c10s-vs-c8-baseline.md
仮説: tags 列を MESI Shared 維持にすれば AVX2 scan が cache miss を被らないはず。c8 から VISITED bit を `Box<[AtomicU64]>` 別領域に分離した `c10s` を testbed で比較し、**read-only zipf-1.0 16T で +102% / uniform で +74%** と reader 経路 2× 改善で仮説支持。ただし **read-heavy zipf 16T で −17〜−21% regress** — visited が 1 line に集中して hot key ping-pong が顕在化。c10s 単独では片側勝ち、c10sw / c10w との合成が次の検証軸と整理。

### 2026-05-08-c11s-conditional-set.md
仮説: x86 `lock or` は値変化に関わらず line を Modified に遷移させるので、`fetch_or` を `if load == 0 { fetch_or }` に変えれば hot key ping-pong が消えるはず。c10s からの単一行 diff で `c11s` を作成、**read-only adv-hot 16T で c10s 比 +999% / zipf-1.2 16T で +121%** と reader 集中軸は圧勝、HR は c8 と完全一致。ただし **read-heavy zipf 16T は c10s の regression を解消できず** (writer Mutex critical section 支配)、c11w (writer CAS claim) との合成が次の必要軸。publishable surface 昇格は c11w 結果待ち。

### 2026-05-08-c12s-cas-slot-claim.md
仮説: writer Mutex を完全排除して `hand: AtomicUsize` + tag CAS + install-at-evicted-pos で SIEVE 等価を保てるはず。実装 → 主仮説 **「install-at-evicted-pos が SIEVE 外部等価」が崩壊**: 新 entry が hand 直前 visited=0 で入る → 即 evict 候補で「保護期間が短い CLOCK 亜種」に変質、oracle 不一致。throughput は read-heavy zipf 16T で c8 比 +223% だが HR は read-only zipf-0.7 で −72%、SIEVE 仕様違反として **棄却**。後続 lock-free 系が同じ罠に落ちないための reference として artifact 残置、次案は per-shard sub-sharding (構造的に SIEVE 等価が自明)。

### 2026-05-08-c13s-sweep.md
c12s 棄却 (SIEVE 等価性破壊) を踏まえた再設計 `c13s`。仮説: Path A (= 既存 key の value 更新、eviction 起こさない経路) は SIEVE state machine を触らないので lock-free 化しても順序保持が自明。VERSION bit (0x4000) を Path A flip で reader seqlock 検出。**read-heavy zipf 16T で c11s 比 +43〜+111% / HR は orig と ±0.005 一致**。ただし uniform read-heavy で c11s 比 −67% (Path A retry 空振り) と adv-hot で HR −0.26 (reader false-miss) が残り、**c14s として再評価する条件付き保留**。

### 2026-05-08-find-avx2-avx512.md
解析ノート (実測なし)。仮説: AVX-512 は server で実態的に ubiquitous なので opt-in path として価値ある。具体勝ち手 3 軸を整理: **V1** (AVX-512 VL + kmask, 256-bit) で `vpmovmskb` + BLSR pair を一掃 / **V2** (zmm 512-bit) で chunk 数半減、downclock 注意 / **V5** (V2 + B1 SoA tag split) で per_shard=64 が outer ループ無し 1 zmm shot。PEXT 系 (P1/P2) は kmask が代替するので AVX-512 経路では不要。配布形態は cargo feature `avx512-vl` / `avx512-zmm` の二段 + runtime detect で AVX2 fallback。

### 2026-05-08-find-avx2-caller-merge.md
`find-avx2-frontier` Tier-S の caller-merge 最適化を 3 試行で詰めて **採択**。第 1 試行 (S1+S2) は sret 化と LLVM の shift round-trip 非畳みで棄却、第 2 試行 (A3+`#[inline]`) は perf gate 内まで改善するも sret 残存、**第 3 試行 (NonZeroU16 + A3 + `#[inline]`) で完全達成**: niche optimization で 16 byte に詰めて sret 解消、const assert で固定化。perf-gate AB は insert_u64 −7.15% / mixed_u64 −10.14% / insert_u32_slot16 −9.24% / insert_string −3.22% の 4 シナリオ大勝、5% gate 違反なし、Twitter cross-check も方向一致で採択維持。学び: sret threshold (16 byte) は ABI のクリフ、niche-bearing 型でのサイズ詰めを積極検討、3 段試行で各制約を独立に閉じる。

### 2026-05-08-mokabench-arc-traces.md
`bench.rs` に **ARC paper trace** (`S3 / DS1 / OLTP / spc1likeread`) の `--source arc` を追加 (dataset / パーサ意味論は mokabench から拝借、load generator 自体は統合せず軽く済ませた)。`.zst` は zstd で on-the-fly 展開、比較対象は overhead を避けるため moka でなく mini-moka。スモーク結果: Zipf-1.0 で HR ±0.4pp 一致 / 速度 ~17.6×、**ARC OLTP cap=4000 で senba HR 51.7% vs mini-moka 45.7%** (SIEVE が DB workload で勝つ既知パターンと整合)、ARC S3 は両者壊滅。OLTP は perf-gate scenario 候補、S3 却下、DS1 / spc1likeread 未検証。

### 2026-05-08-external-lib-sweep.md
ARC paper trace 6 種 + Zipf を senba (auto-shard) / sieve_orig vs mini-moka / moka 0.12 で sweep。**HR は workload と cap で SIEVE / W-TinyLFU が反転** (OLTP cap=8000 で SIEVE +7.5pp、対 DS1 cap=1M で W-TinyLFU +7pp 等)、Throughput は senba が single-thread 公平条件で 2–4×。副次発見: **working set が cap に収まる帯で senba < orig**、ConCat の cap 軸で senba/orig 比が `shards` 数と逆相関で崩れる (111% → 73% → 58%) ため **「hot 集合が uniform hash で shards に分散して cacheline 局在を失う」cacheline dispersion 仮説**を提示。検証案として Slot8 (256 ent/shard) / shards 上限 / `perf stat` LLC miss 直接計測を follow-up に残す (※後続 vtune-windows-orig-vs-senba で本仮説は反証された)。

### 2026-05-08-c14s-design.md
c13s 不採用要因 2 点 (uniform read-heavy regression / adv-hot HR drop) を構造変更ではなく 3 点の実装 tuning で潰す c14s の設計書。(1) find_lockfree の AVX2 化 / (2) Path A MAX_RETRY=1 (CAS 失敗時即 Mutex escalate) / (3) reader bounded retry。SIEVE 等価性は Path A が state machine を触らないため自明保持。c15 (entry-level seqlock) は §7 に粗描き、c14s は実装 tunable レベルでの暫定解と位置付け。**ERRATUM**: §2.3 で reader retry を "true/false miss 区別不可" としたのは誤りで、実装では seqlock-fail + LIVE=0 観測で racing 検出して条件付き retry に変更 (sweep report 参照)。

### 2026-05-08-c14s-sweep.md
c14s sweep 評価。設計書 §2.3 の前提誤りで初版実装は無条件 4× retry が miss-path を 4 倍化、uniform read-only 16T で c11s 比 −75% の壊滅的 regression。samply で leaf 62% を retry ループに同定し、racing 観測時のみ retry する形に修正。**§4 acceptance T2/T3/T4 は pass**、特に adv-hot HR は c14s 0.920 で c13s の SIEVE 意味的劣化 (0.571) を完治。**T1 (uniform read-heavy 16T) は依然 fail** だが c14s/c11s = 0.323 と c13s/c11s = 0.345 がほぼ同水準で、Path B/C Mutex の構造的競合 = c14s 責任外。lock-free Path A の代表は c13s から c14s に更新。

### 2026-05-09-vtune-windows-orig-vs-senba.md
external-lib-sweep の **cacheline dispersion 仮説**を Windows native + VTune で直接検証 (`bench_vtune.rs` を新設、ITT API で collection 範囲をバイナリ自身が制御)。計測方法論の教訓 2 つ: (1) `-start-paused` 必須、(2) active 10s 以上 (ops ≥ 180M) でないと MUX < 0.95 で TLB 系 counter 信頼不可。clean run で **senba は orig 比 +12–16% 遅い** (Linux/WSL2 40% gap よりは小さい)。Top-Down で **cacheline dispersion 仮説は反証** (L1/L2/L3/DRAM 絶対値ほぼ一致)、ただし質的に対極で **orig は latency-dominated + TLB pressure** (DTLB Overhead 20.4%、`Box<Node>` × 1M が STLB を miss)、**senba は bandwidth-dominated** (BW 59.2%) で TLB pressure 無し代わりに **L3 内部 queue 圧迫**。両者 IPC 1.10 で揃い、senba 遅さは **instruction footprint +17%** が cycle +16.5% に直結したもの。Linux 40% gap 内訳は構造由来 12–16pp + OS/環境由来 24–28pp で、後者は Linux THP が orig の Box<Node> を 2M page promote している仮説 (要 bare Linux 検証)。検証案 1 (Slot8) / 2 (shards 上限) は再検討候補に復帰、新規 4 (`Cache::prefetch` API) / 5 (instruction footprint 削減) を追加。

### 2026-05-10-visited-bitmap.md
仮説: VISITED ビットを `Shard::tags[i]: u16` から外して `Shard::visited: u64` per-shard bitmap に出せば、(1) HASH bit 拡張で SCAN 偽陽性半減、(2) `find_evict_pos` が O(1) bit-twiddle 化、(3) tags line dirty を制御 line dirty に逃せる、の 3 点で勝てる。実装 → **perf-gate 6 シナリオ**: get_heavy −7.76% / mixed_lowskew −10.04% / mixed_u64 −3.05% (3 シナリオ improved)、insert_* +1% (gate 内)。Twitter 5 cluster × 3 cap × 3 run sweep で 14/15 セル improvement (平均 −6.3%)、oracle 等価性維持。**採択**。follow-up: caller-merge の HASH bit 化後再評価 / 並行 variant への流用 / Slot8 復活案との併用。

### 2026-05-10-write-contention-design-space.md
並行 SIEVE の write contention 改善策の設計空間を整理。「hot-key だけが本丸」と再フォーカスし、(1) 改善策 4 系統 (直列化 / 空間分散 / decouple / 楽観的 retry) の地図化、(2) **「atomic = 速い」は誤解で実態は MESI で hardware level に直列化される** (lock-free の真の利得は「lock を消す」ではなく「cache line ownership を分散する」)、(3) shard-affinity は moderate Zipf には効くが hot-key には効かない、(4) **LongAdder 流 visited bitmap を 8-shard packing で 8× 圧縮** (HR 副作用ゼロで write contention を構造除去できる候補) の 4 点を導出。進め方は Phase 1 sloppy (5 行) → Phase 2 packed LongAdder → Phase 3 shard-affinity の保険札。Caffeine 流 sieve_cb は senba スコープ外。

### 2026-05-10-c15s-sloppy-visited.md
仮説 (write-contention-design-space §7 Phase 1): reader visited を `1/(2^SAMPLE_BITS)` の TLS-RNG gate でサンプル化すれば atomic write traffic を減らして throughput が伸びる。`c14s` から `c15s` を派生 → skew=1.0 で 1/16 で 0.91× / 1/4 でも 0.97× の **明確な regression**、HR も sample 比例で低下 (Twitter で平均 −2.09pp)。**REJECT**。構造的結論: c11s の conditional load-then-fetch_or 構造下では reader load が ~1 ns に圧縮済で、TLS RNG draw ~3 ns / call が gate 節約コストを上回る。design doc §3 の「atomic load も MESI で 50–200 ns」前提は c11s 構造下では成立しない反例。Phase 2 (packed LongAdder) の動機を writer Path A 側 / shard-affinity 方向に再構成。

### 2026-05-10-c14s-vtune-write-contention.md
write-contention-design-space §3 hot-line 仮説の直接検証。c14s @ 4T / Zipf 1.0 / cap=4096 を Windows native VTune で計測 (新設の自己完結ドライバ `bench_vtune_concurrent.rs`)。**LLC Miss = 0 / DRAM Bound 0.2% / L3 Bound 21.9%** で working set が L3 内に閉じているのに L3 が pipeline 1/5 を食う → **c2c bouncing が単独で wall-clock を削っている** ことが断定。hot line は単一でなく **3 cluster** (writer state cluster 0.416s / visited 0.276s / Mutex word 0.251s) が同格。前回 single-thread 観測の「Mutex は 8.7% で従」を **Mutex word そのものが core 間 bouncing 主因** に修正。read-side は完全に健康 (`scan_evict` Memory Bound 3.8%)。次の variant 設計方針: (a) 3 hot line を 1 cache line に co-locate、(b) writer-side batching、が ROI 上位。

### 2026-05-10-c16s-design.md
c14s-vtune §8.1 の 1-day prototype として c16s を設計。c14s から派生し per-shard struct layout だけを差し替える: `Mutex<WriterState{hand}>` + visited `AtomicU64` + len `AtomicUsize` を `#[repr(C, align(64))] struct ShardHot` に co-locate (3 hot line → 1 cache line)。per-shard ≤ 64 を活用して visited を `Box<[AtomicU64]>` から単一 `AtomicU64` に縮退。残りロジック (Path A lock-free / Path B/C Mutex / AVX2 reader / SHARDS=64) は c14s と同型。合格 gate は (a) Mops +5% (T=4 c14s 比) と (b) VTune の 3 hot line のうち 2 つ以上で abs mem stall −30% の同時成立。reader bouncing 仮説が崩れたら次は A2 layout (writer/reader 分離) を c17s 候補で持ち越し。

### 2026-05-10-c16s-results.md
c16s の Step 1–5 計測結果。**採用確定**、合否表 (a)✓/(b)✓ 着地。Step 1 oracle PASS、Step 2 Mops AB T=4 で **+7.9%** (gate +5% PASS、CV<0.013)、Step 3 thread sweep で T=2,4,8,16 全帯 c14s を上回り、Step 4 VTune memory-access で 3 hot line 全て −30% gate 超過 (writer state −39% / visited −65% / Mutex −49% で当初想定 2/3 を上回る 3/3)、Step 5 uarch で Memory Bound 23.0% → 16.5% / CPI 0.526 → 0.449 / Retiring 29.1% → 35.7% と全方向改善、LLC Miss=0 のままなので transfer 削減は L3 内で完結。副次効果: `writer_find` mem stall −50%、`find_get_avx2` −25% (reader 側悪化なし、仮説 2 が定量確認)。次の design space は §8.2 writer batching が筆頭。

### 2026-05-10-c13s-c16s-path-a-cas-back.md
c13s/c14s/c15s/c16s 共通 flake (`concurrent_invariants_under_zipf` の "shard X で id 重複") の root cause と修正。Path A 最終 store-back が unconditional だったため、Path C shift loop が `tags[pos]` を別 id で上書きした後に Path A の遅延 store が VERSION 反転 tag で再上書き → tag 重複が発生。修正: store を CAS (`EMPTY → T_a ^ VERSION`) に変更、CAS 失敗時は visited も含めて何もしない (entries[id] 更新は shift 後の position 経由で残る)。debug soak: c16s 5/134 → 0/200、c13s 2/60 → 0/100、c14s 1/60 → 0/100、c15s 0/60 → 0/100。別件として Path A vs writer_update_in_place の `entries[id]` 2 重書き race が残存 (id 重複は引き起こさないので本テストでは検出不可、別 test 設計が要る)。

### 2026-05-11-lru-vs-minimoka-vs-senba-pareto.md
仮説: 直近の senba 改良 (cache-line co-location 等) 後の単スレ pareto を `lru` クレートを baseline に追加して取り直す。SIEVE は小 cap で W-TinyLFU 超えると予想。ARC P1..P14 + Zipf {0.8,1.0,1.2} 全帯で計測 → **throughput は senba 一強** (Zipf 1.3–1.5× lru / 2–3× mini_moka_unsync、ARC でも同比率)。**HR は分裂**: Zipf では予想通り senba ≥ mini_moka > lru、しかし **ARC 小 cap では mini_moka_unsync が senba を上回る** (P6 cap=20k 0.169 vs 0.064 等) で仮説部分的に棄却、scan-heavy で SIEVE が勝つ予想も P3/P6 で反証。副次: bench データ置き場を `docs/benchmark/<topic>/` 上書き運用に移行し、履歴は git に寄せる方針へ。

### 2026-05-11-c17s-design.md
仮説 (G2-α-1): c14s/c16s で残る adv-hot read-heavy 退行 (find_get の EMPTY-lane SIMD overhead) は tag 兼任構造由来なので、同期通知を `Entry::version: AtomicU32` に逃がせば構造除去できる。tag VERSION bit を削除 (HASH 8→9 bit、c11s 同等) して Path A は tag 完全不変 + entry version 偶奇 flip で reader を seqlock、reader は 2-tier seqlock (tier 1 = entry version、tier 2 = tag re-load)。Path C false-miss は ShardHot に追加する `path_c_epoch` で coarse 補償。Slot32 化 (Entry 16B → 32B) のコストと 2-tier seqlock 固定 cost を gate (b) で許容範囲確認する設計。

### 2026-05-11-c17s-results.md
c17s の Step 1–4 計測結果 + §11 tuning pass。**条件付き採用** (条件付き採用、合否表 (a)✓/(b)✗ で着地)。Step 1 oracle PASS、Step 2 Mops AB adv-hot read-heavy 16T で v1 +21.3% → v2 (tier 2 削除) で **+27.4%**、Step 3 skew=1.0 gim T=4 で v1 −4.5% → v2 **−3.7%**、Step 4 thread sweep で adv-hot read-heavy が **T=8 +18.8% / T=16 +29.1%** と super-linear scaling、HR は c16s 0.9249 → c17s 0.9280 と高 T で +0.3pp 構造改善。§11.2 で試した「epoch_before も hit から skip」optimization は **doubled find_get on miss path** で skew=1.0 gim T=4 で −14.3% に悪化、revert (epoch_before は ShardHot 同居なので find_get の len.load で L1-hot、削除効果ほぼゼロという教訓を artifact 記録)。後続候補: G2-α-2 (versions 別配列で Slot16 維持)、G2-β SoA state。

### 2026-05-11-cseries-string-baseline.md
仮説: c17s の gate (b) 退行は Slot32 化由来のはずだから、Slot32 自然形ワークロード (`(u64, String)`) で測れば「imposed Slot32 vs natural Slot32」を分離できる。c14s/c16s/c17s × {u64, String} × T∈{4,8,16} × {gim@1.0, read-heavy@1.4} の 12 セル sweep を計測。u64 read-heavy で c17s vs c16s が **T=16 +21.7%、HR +0.34pp** と c17s-results.md を再現、u64 gim は thread 帯依存 −3.2%〜+1.8% で構造退行ではないと再評価。**最大の発見は別軸**: c14s/c16s が `V=String` で `free(): unaligned chunk` SIGABRT を 8/12 セルで起こす (seqlock-via-tag の ptr::read 前 escape を持たないため、半上書き `ManuallyDrop<String>` の drop 経路で free 破壊)、c17s は entry-level seqlock の早期 escape で全 12 セル稼働。c17s を **G2-α-1 確定採用** に強化、c14s/c16s は `V: Copy` constraint 付きの research artifact 扱い、library 候補は c17s 系統一択になった。次は `(u64, [u8;24])` で String clone cost と Slot32 stride cost を分離する `--value bytes24` を予定。

### 2026-05-12-c18s-design.md
仮説 (G2-α-2): c17s の gate (b) 退行 (skew=1.0 gim T=4 −3.7%) の主因は Slot32 化 (Entry 16B → 32B) 由来の entries footprint 倍増のはずだから、`Entry::version` を別配列 `versions: [AtomicU32; 64]` に逃がして Slot16 復帰 + entries 半減すれば構造解消できる。同時に `path_c_epoch` を ShardHot から新 ReaderState block に移動し、reader-only cluster (epoch + versions、5 cache line) として writer-hot cluster (Mutex/visited/len) と完全分離する設計。

### 2026-05-12-c18s-results.md
c18s の Step 1–4 計測結果。**REJECT** (合否表 (a)✓ but c17s 比劣後 / (b)✗)。Step 1 oracle PASS (4 構成 diff=0)、Step 2 gate (a) adv-hot read-heavy 16T で c18s +6.5% は通過するが c17s **+23.8% から大幅縮退**、Step 3 gate (b) skew=1.0 gim T=4 で **c18s −8.5%、c17s −4.7% より更に悪化**、Step 4 thread sweep adv-hot で **全 T で c17s に劣後 (T=16 で −14.2%)**。設計仮説 (entries footprint が支配 cost) は反証され、root cause は **reader cache line touch +2 (versions 別 line + path_c_epoch 別 line) のコストが entries 半減の利得を上回った**こと。c17s では version が entries[id] と同 line で MESI/transfer ゼロ追加だったのを別 line にしたため、per-hit cache fetch が 1→2 に増えた構造。教訓: naive な field split は逆効果、SoA するなら all-in (G2-β) でないと意味がない。c17s 据え置き、次の天井は write 側 Mutex (D.4 G3 系) と確定。

### 2026-05-10-shard-layout-s3-capacity-removal.md
仮説 (improvement-ideas §B.1 S3 + observation): `Shard` を `#[repr(C)]` 化してフィールド再配列、`capacity: usize` field を削除 (`entries.len()` と恒等、`#[inline] capacity()` 経由) すれば read hot path 4 フィールド (`tags.ptr` / `entries.ptr` / `len` / `visited`) を cache line 1 に閉じ込められる。実装後、`std::mem::offset_of!` const-eval で `tags@0, entries@24, len@48, visited@56, hand@64` を契約化し、asm verify で sizeof 112B → **104B**、`len` load `[r8+64]` 線2 → `[r8+48]` 線1 を確認。perf-gate (criterion ×2) は geomean −1.8% (`get_heavy_u64 −4.32% p=0.01`)、Twitter 60 cells は perf-neutral (+0.11%、HR 完全一致)、ARC OLTP cap=2000 で −2.65%、**ARC DS1 (3 cap × 9 trials × ~5s/trial) で 3/3 cells improved -1.5〜-3.3%、geomean −2.19%**。短い trace では noise floor 下で見えないが、長尺・大 cap workload で構造的利得が wall-clock に出ることを確認。**採択**。

### 2026-05-12-c17s-step1-len-load-removal.md
仮説 (c18s-results §9.1 残候補): c17s reader hot path の (1) `find_get` / `find_lockfree_for_path_a` の `hot.len.load(Acquire)` + `pos < len` 分岐は TOCTOU 安全に削除可能、(2) `path_c_epoch` を ShardHot から独立 64B line に分離すれば Path A `visited.fetch_or` の MESI invalidate を解放できる。**Step 1 採用、Step 2 reject**。Step 1 は paired AB で gate (a) -1.6% / gate (b) +0.9% と noise band 内 (perf neutral) だが disasm で atomic load -1 + branch -1 が確実に消えており code が cleaner なので採択。perf 動かない理由は get_by_hash の path_c_epoch.load が直前で ShardHot を L1 prefetch しているため len.load が元から L1-hot で free だったこと。Step 2 は gate (b) -0.5% / 低 T gim sweep で系統的 -1〜-2%、c18s §9.3 の「reader が touch する line +1 のコストが writer 干渉低下を上回る」原則が path_c_epoch 単独でも成立することを追加検証 (revert)。副次 learning: thread sweep の old c17s T=16 が thermal throttle (trial 0,1 p99 ~430-490ns) で +15.8% の見かけ改善幻覚を産んだ、5-trial controlled で -0.4% (neutral) に着地、WSL2 単発計測は ±10% を簡単に作るので p99 を必ず見る。

### 2026-05-12-r1-design.md
c-series (単シャード並行制御) を c18s reject で saturation と判断、shard 間 routing 側に解空間を移す新シリーズ **r-series** の 1st variant 企画書。r1 は c17s の単シャード構造を完全継承し、`shard_of_hash(hash) → idx` を `shard_of(hash, tls_id) → idx` に 2 引数化、SHARDS=64 の 6 bit を `(set_bits, way_bits)` に分解して set は hash、way は TLS thread-id で選ぶ hard 分割 scheme。trilemma (i) bouncing-free / (ii) HR 等価 / (iii) probe O(1) のうち hard は (i)(iii)、soft は (i)(ii)、c17s は (ii)(iii) という構造整理。affinity 同定子は CPU-id (rseq) を一度検討した上で TLS-id を採択 (oversubscription / migration regime で strict に強い)。主成果物は (T × WAYS) sweep の HR vs Mops 曲線 / pareto frontier で、c-series 流の単一 acceptance gate は使わない。perf-gate は WAYS=1 sanity に格下げ。reject 時は soft hybrid (r2s) が次の variant 候補。

### 2026-05-12-r1-results.md
r-series 初期 baseline sweep。前 sweep (cluster52 単独 / T=16 偏り / value=u64 のみ) の評価を訂正。計測: T∈{1,2,4,8,16} × WAYS∈{1,2,4,8} × {Zipf 0.8/1.0/1.4 (gim/read-heavy), Twitter cluster006/016/018/019/034 (OSDI Yang 形式), ARC OLTP/DS1/S1/S3/P1/P8/ConCat/MergeP} × {u64, string}、trials=3、ops=4M。結果: **部分採用 — 採用領域 cell 31 / 520** で前 report (2/132) から大幅拡大。最強 cell は `twitter_cluster019` u64 T=16 w=8 で Mops **+77.7%** / HR drop 2.31pp、`cluster034` w=4 で +30.7% / 4.65pp、ARC `MergeP` T=8 w=4 で +31.2% / 0.46pp (HR-preserving zone)。**Zipf 1.4 read-heavy では value=string で +30.5% / 2.16pp (u64 +13.6% から 2.2x 増幅)** — Drop<String> が c17s で writer bouncing と co-located の cross-core contention 源だった構造を直接検出。Scalability knee は cluster019 で T=4 から w≥4 が分離、T=16 で w=8 が explosive。cluster006 (HR drop 28pp) / ARC OLTP (21pp) / Zipf 1.0-0.8 / ARC S1/S3 は全 cell 採用領域外で、workload class 別の適性が明確に分かれる。lib 化は引き続き保留 (per-trace adaptive WAYS の必要性が確認された)。Follow-up は trials=5 再計測と adaptive WAYS prototype。

### 2026-05-12-mt-overhead-vs-lib.md
仮説 (r1-sweep 中に user 提起): c17s/r1 系は MT 対応の構造的 overhead を払っていて、senba::Cache (lib) と単スレ比較すると相当差があるはず。`bench --variant senba` (lib) と `bench_concurrent --threads 1 --variant c17s` (MT 系) で apples-to-apples (cap, ops, HR 一致) で計測: c17s T=1 は **read-dominant で ~4-5x、miss-heavy で 最大 15.7x** の ns/op overhead を払う (cluster019 で lib 29.3 ns/op → c17s 460.8 ns/op、HR 0.316 bit-for-bit 一致)。Δ は +33〜+430 ns/op の範囲で workload 依存、原因は Mutex acquire ではなく **reader fast path で touch する atomic Acquire load の累積** (`path_c_epoch` / `AlignedTags` / `visited` 等 ~3 本 × ~3-5 ns)。これが c/r-series の単スレ ceiling を ~25 ns/op = ~40 Mops に規定し、T=16 で perfect scaling なら 640 Mops aggregate が天井。現状 T=16 c17s Zipf 1.4 gim u64 = 159.81 Mops は ceiling の **25-27%** で、まだ 3-4 倍の伸びしろがあるが大半は memory-order の構造的下限に近づく。Action items: (1) VTune memory-access で Acquire load 累積を直接観測、(2) Acquire→Relaxed 化可否の case-by-case 検証、(3) cluster019 を perf-gate の固定 workload に追加、(4) lock-free writer protocol 再挑戦 (epoch-based eviction defer) を c19s 候補として検討。c-series / r-series の競争相手は lib ではなく moka / mini-moka / Mutex<lib> であり、perf-gate の役割は二分割 (lib の単スレ性能 vs MT cache 同士の競争) で再定義する。

### 2026-05-12-partitioned-design.md
仮説: mt-overhead-vs-lib で出した「lib の単スレ性能を T 倍積めるなら ceiling は 640 Mops」を直接ベンチマークできる baseline として、`senba::Cache` を N 個並べて thread-id でルーティングするだけの `senba::concurrent::PartitionedCache` を lib に新設する。実装は `Box<[Mutex<Cache>]>` + TLS counter routing で ~150 行、依存追加ゼロ (`std::sync::Mutex` のみ)。`bench_concurrent` に `--variant partitioned --partitions N` を新軸として追加し、**T と N を独立に sweep する (T × N = 5 × 7 cell × workload)** ことを設計契約に明記。期待: HR-tolerant (cluster019 / MergeP) で partitioned 圧勝、HR-sensitive (ARC OLTP / cluster006) で完敗、その領域マップが成果物。本書は企画 + 実装着手前段で、sweep / 結果は後続レポートで切る。

### 2026-05-12-partitioned-results.md
仮説検証: partitioned design の (T × N × workload) 1215 trial sweep を実施 (parking_lot::Mutex 採用版)。**設計 gate cell (Zipf 1.4 read-heavy T=16 N=16) は採用ライン +50% に対し −43% で失格**: partitioned 75.4 Mops vs c17s 133.1 Mops、T=1 ですら partitioned が負ける (c17s の AVX2 + epoch fast path が hot-key 帯で per-op 7.5 ns まで落ちており、partitioned の mutex+lib 30 ns/op で勝てない)。一方 real trace では partitioned 圧勝: Twitter cluster019 **+390% (HR drop 2.6 pp)**、cluster034 +206%、ARC OLTP +109% (HR drop 28 pp)、ARC DS1 +383% (HR drop 0.2 pp)。accept zone (HR drop ≤5pp & Mops gain ≥+20%) 44/225 = 19.6% で「100 cell」目標未達だが scan-heavy 帯に連続。設計の reject 条件には機械的に該当するが、領域分割が鮮明で lib 価値は独立にあるため keep/move 判断は人間に委ねる。

### 2026-05-13-partitioned-vtune.md
仮説判定 (partitioned-results §Scaling 物理層 3 仮説): VTune memory-access + uarch-exploration を 9 cell (cap ∈ {4096, 1024} × T=N ∈ {1,8,16}) で。**memory BW 律速は却下** (cap=4096 で LLC Miss = 0, DRAM Bound ≤0.1%、データは 20 MiB L3 に完全 fit)。**L3 latency が主犯**: cap=4096 T=8 で L3 Bound 41.7% / Memory Bound 44.8%、cap を 4096→1024 (per-partition 128 KiB → 32 KiB、L1d fit) に縮めるだけで **L3 Bound 41.7→26.9% / aggregate Mops 50.8→133.7 (+163%)**。**SMT pair L1d 共有が副犯**: cap=4096 T=8→T=16 で L1 Bound +9.8 pp、cap=1024 で +2.5 pp に縮む。E-core は memory に詰まっておらず drag 寄与小。**cap-tune が partitioned scaling の支配因子** であり、設計書 reject 条件 (Zipf 1.4 read-heavy で c17s 同等以下) は cap=4096 前提だったため再評価が必要 — cap=1024 では T=16 partitioned 157.5 Mops ≥ c17s 133.1 Mops。HR との trade-off は cap=1024 で bench_concurrent sweep を取り直すのが次の最重要項目。
