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
| 外部実装比較 (W-TinyLFU) | j8-vs-mini-moka-twitter |
| 設計アイディア集 (living doc) | `../improvement-ideas.md` (旧 `2026-05-04-improvement-ideas.md`、`docs/` 直下に移動) |
| j3 系列 (Map なし SIMD) | sieve-j3-bench |
| j4 系列 (set-associative j3) | sieve-j4-set-associative, sieve-j4-crossover-and-shard-sweep, sieve-j4-pershard-vs-footprint |
| j5 系列 (j4 の double-hash 排除) | sieve-j5-doublehash-ab, j5-pershard-pareto, j5-twitter-pareto, j5-vs-orig-2x-memfair |
| j6 系列 (M2.1: visited を tag に同居) | sieve-j6-m21-twitter |
| j7 系列 (M2.3: tag を u16 化、visited + 14-bit hash) | sieve-j7-m23-twitter, j7-twitter-pareto |
| j8 系列 (M5.3 + tag 内 ID embed + free_list 廃止) | sieve-j8-bench, j8-candidate-loop-analysis, j8-c-hoist, j8-twitter-pareto, find-avx2-frontier, find-avx2-pext |
| c8 系列 (j8 並行版: read lock-free + write per-shard Mutex) | c8-design, c8-vs-moka-thread-sweep |
| c9 系列 (senba::Cache 並行版: per-shard Mutex<Shard> wrap、V: Clone) | c9-design, c8-vs-c9-thread-sweep |
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
