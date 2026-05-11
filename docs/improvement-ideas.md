# SIEVE 実装 改善アイディア (living doc)

このドキュメントは date-stamped レポートではなく、**現時点で未検証のまま残っている改善案の一覧** を保持する場所。日付つきの実験レポートは `docs/reports/` を、横並び比較は `docs/api-comparison.md` を参照。

各案には参照レポートを併記し、検証 / 採択 / 棄却が出たら本ドキュメントから「§I. 棄却・実装済 (履歴)」へ移動する運用。**ここに残っているもの = まだ実測 or 実装で潰せていない案**、と読んでよい。

---

## §0. 現状サマリ (〜2026-05-12)

### ST 路線

| 系列 | 位置付け | 主レポート |
|---|---|---|
| `sieve_orig` | NSDI'24 著者参照の忠実ポート。**oracle (eviction 列の正解) 専用**。速度比較対象としては陳腐化 | `sieve-orig-overhead-analysis` |
| `sieve_v0`〜`v3`, `j3`〜`j7` | 連結リスト → array → set-associative → tag u16 化の研究系列。**いずれも j8 / senba::Cache に吸収済み**、artifact として残置 | `v3-bench`, `sieve-j7-m23-twitter` ほか |
| `sieve_j8` | M5.3 + tag 内 ID embed + free_list 廃止 + BLSR×2 + sizeof-aware layout + c-hoist。**inline 20 B/cap で orig (25) を抜きつつ Twitter 5 cluster 中 4 cluster で throughput 勝ち** | `j8-twitter-pareto`, `j8-c-hoist`, `st-twitter-5cluster` |
| **`senba::Cache`** (`src/lib.rs`) | j8 を `Cache<K, V, SlotSize, SHARDS, BuildHasher>` として抽象化したライブラリ surface。`SlotSize` で任意 `sizeof(Entry)`、`Borrow<Q>`、`peek` / `iter` / `retain` / `stats`。**shift-on-evict 化済 / AlignedTags 32B 化済 / find caller-merge (NonZeroU16 + A3 + #[inline]) 済 / visited bitmap 化済** | `senba-sievecache-design`, `sieve-cache-shift-on-evict`, `aligned-tags-load`, `find-avx2-caller-merge`, `visited-bitmap` |

### 並行路線

| 系列 | 位置付け | 主レポート |
|---|---|---|
| `sieve_c8` | j8 並行版 (read lock-free + write per-shard Mutex、seqlock-via-tag)。`V: Copy` 制約。**cap=16384 / SHARDS=256 / T=16 で moka 0.12 比 15.4×** | `c8-design`, `c8-vs-moka-thread-sweep` |
| `sieve_c9` | senba::Cache を per-shard `Mutex<Shard>` で wrap。1T と低 skew で c8 を上回るが skew=1.2 / 16T で c8 比 1/8.7 に崩壊 | `c9-design`, `c8-vs-c9-thread-sweep` |
| `sieve_c10s`〜`c14s` | 単一 shard testbed で reader/writer の天井を順に剥がした研究系列。c11s で reader visited ping-pong を構造除去、c14s で Path A lock-free + adv-hot HR 修復まで到達 | `single-shard-baseline`, `c11s-conditional-set`, `c14s-design`, `c14s-sweep` |
| `sieve_c15s` | reader hot path に sloppy visited (TLS-RNG gate)。**reject** — c11s の conditional load-then-fetch_or が既に reader を MESI Shared に置けており、TLS RNG コストが利得を上回る | `c15s-sloppy-visited` |
| `sieve_c16s` | c14s の per-shard hot 4 line を `#[repr(C, align(64))] ShardHot` に co-locate (Mutex word + visited u64 + len + hand)。T=4 skew=1.0 で c14s 比 +7.9%、3 hot line すべて abs mem stall -39% / -65% / -49%、Memory Bound 22.0% → 15.9%、CPI 0.526 → 0.449。**ただし `V: Copy` 限定** (seqlock-via-tag の `ManuallyDrop<V>` ptr::read 前 escape がないため `String` で SIGABRT) で research artifact 扱い | `c14s-vtune-write-contention`, `c16s-design`, `c16s-results`, `cseries-string-baseline` |
| **`sieve_c17s`** | G2-α-1 entry-level seqlock。Path A は `Entry::version` 偶奇 flip で reader を seqlock、tag は `LIVE\|ID\|HASH` のみで Path A 中不変。`find_get` の EMPTY-lane SIMD 検出を削除し、Path C false-miss は `path_c_epoch` の coarse seqlock + bounded retry で構造的に処理。**adv-hot read-heavy T=16 で c16s 比 +27.4%、`V: Clone` 健全** (= library 候補は c17s 系統一択)。代償は skew=1.0 gim T=4 で c16s 比 −4.7% (受容)。tuning: Step 1 (`len.load` 削除) **採用** / Step 2 (`path_c_epoch` 独立 line) **reject** | `c17s-design`, `c17s-results`, `cseries-string-baseline`, `c17s-step1-len-load-removal` |
| `sieve_c18s` | G2-α-2 (`versions: [AtomicU32; 64]` を別配列に逃して Slot16 復帰 + `path_c_epoch` も ReaderState 独立 line に分離)。**REJECT**: gate (a) は通過するが c17s 比 −14%、gate (b) −8.5% で c17s より悪化。**reader cache line touch +2 が entries 半減の利得を上回った**ことが root cause。教訓: naive な field split は逆効果、SoA するなら all-in (G2-β tags/versions/visited 全 array 分離 + AVX-512 統合) でないと意味がない可能性 | `c18s-design`, `c18s-results` |

ベースラインは ST が `senba::Cache`、並行が `sieve_c17s` (V: Clone soundness が確定して以降 library 候補)。以降の改善は基本的にこの 2 つを起点に評価する。c16s は `V: Copy` 限定の research artifact として保存、c17s が乗らない workload (= skew=1.0 mixed 帯) の比較相手として残置。

### 残っている構造的天井

| 課題 | 出典 | 状況 |
|---|---|---|
| (i) 並行 T1 (uniform read-heavy 16T) で Path B/C writer Mutex 競合 | c14s-sweep §4.1, c16s-design | c16s 採用後も未解決。G3 系 (sub-shard / install-at-hand+cap/2) 待ち |
| ~~(ii) seqlock-via-tag 由来の false-miss と read-only adv-hot −22% 退行~~ | c14s-sweep §4.2, improvement-ideas §G2-α | **c17s 採用で構造解消**: entry-level seqlock で tag を Path A 中不変化、`find_get` の EMPTY-lane SIMD 検出を削除可能化。adv-hot read-heavy T=16 で c16s 比 +27.4%。残る false-miss は `path_c_epoch` coarse seqlock + bounded retry で structural に処理 |
| (iii) cap-fits 帯 senba < orig (Windows native でも +12–16%) | vtune-windows-orig-vs-senba | instruction footprint +17% / L3 queue 圧迫が主因。Slot8 / shards 上限 / prefetch / footprint 削減が候補 |
| (iv) Path A vs `writer_update_in_place` の `entries[id]` 2 重書き race | c13s-c16s-path-a-cas-back §3.3 | id 重複は引き起こさないので既存 test 不可。別 test 設計要 |

---

## §1. 推奨優先度マップ

「(a) 効きが大きい × (b) 実装複雑度が低い × (c) 既存資産を壊さない」の 3 軸での主観評価。

### 短期で perf に乗せやすい (1 日〜数日)

| 優先 | 案 | 効く課題 | 出典 |
|---:|---|---|---|
| 1 | **§B.1 S4** Slot16 monomorph の `vpbroadcastd` chunk 内再構築の reg-alloc 是正 | Slot16 限定 −4 cy | find-avx2-frontier §S4 |
| 2 | **§B.1 A2** `len == MAX_PER_SHARD` 専用 4-chunk specialize | chunk 間 branch 3 個削減 | find-avx2-frontier §A2 |
| 3 | **§D.4 G3-ζ** sub-sharding | T1 uniform read-heavy 16T fail を 1/K 化 | improvement-ideas §G3-ζ |
| 4 | **§E.5 α** warmup 分岐に `unlikely` + `#[cold]` outline | hot path layout straight-line 化 (constant fold は出ない、footprint 中立) | improvement-ideas §E.5 |
| 5 | **§F.2 calibration runs** | perf-gate 自己分散の常時計測 (revert 教訓 T4) | find-avx2-pdep-pext-revert §3 |

(S3 = Shard フィールド並び替え + capacity field 削除は **採択済**、§I 参照: shard-layout-s3-capacity-removal)

### 中規模 (1 週間程度)、構造改修だが ROI 期待大

| 案 | 効く課題 | 出典 |
|---|---|---|
| **§B.2 V1** AVX-512 VL + kmask | per_shard=16/64 で −0.8/−3 ns、no downclock | find-avx2-avx512 §V1 |
| **§B.3 B1** SoA tag split (8-bit hash + 8-bit meta) | per_shard=64 で chunk 半減 | find-avx2-frontier §B1 |
| **§D.3 writer batching** | per-shard 単位で transfer cost を amortize | c14s-vtune §8.2, c16s-results §9 |
| **§D.5 packed LongAdder visited** | reader visited contention を 8× 圧縮 (HR 副作用ゼロ) | write-contention-design-space §6 |
| **§E.1 Slot8 ブラケット** | 大 cap で shards を 1/4 圧縮 → instruction footprint / L3 queue 圧迫の直撃緩和 | external-lib-sweep §検証案1, vtune-windows §6 |
| **§E.5 γ** full-state outlined specialization (`insert_full` / `find_evict_pos_full`) | const fold cascade で hot path 全域から `len` 依存 load / 分岐除去 (机上 5–15%) | improvement-ideas §E.5 |

### 賭け / 大改修 (2 週間〜)

| 案 | 出典 |
|---|---|
| **§B.2 V2 / V5** AVX-512 zmm (512-bit) / V2 + B1 合成 (per_shard=64 を 1 chunk shot に) | find-avx2-avx512 §V2 / §V5 |
| **§D.2 G2-β** SoA state (slots: `Box<[AtomicU32]>` の 3rd 配列) | improvement-ideas §G2-β |
| **§D.4 G3-δ / ε** install-at-(hand+cap/2) / free-list 経由 lock-free install | improvement-ideas §G3-δ / §G3-ε |
| **§D.6 hot-key replica tier** | Count-Min Sketch + lock-free read-only replica | write-contention-design-space §5 |

### メソドロジー先行 (今後の判断軸を作るが perf 数字には直結しない)

`§F` 参照。bare Linux perf stat / GitHub Actions one-shot triage / VTune per-allocation breakdown / 大 cap perf-gate scenario 追加 / Twitter cross-check for c16s。

---

## §2. ライブラリ surface (`senba::Cache`)

`docs/api-comparison.md` が moka / lru / quick_cache / stretto との API 横断比較 + 進捗トラッカーを担う (living document)。本章は「API 拡張の優先度判断」と「surface 内部の構造的最適化」に限定する。

### A.1 公開 API の残ギャップ

`api-comparison.md` §9 の「欠けているもの」のうち、本 crate スコープでの追加候補:

- **`drain()`**: consume 系の唯一埋めるべきギャップ (値所有 `IntoIterator` は `drain().collect()` の sugar 以上にならない)
- **`pop_oldest()` 相当**: SIEVE は LRU/MRU を持たないが、フラット配列の `tags[0]` が `sieve_orig` の "tail (oldest)" と対応するので構造上 1 件露出は可能
- **`get_or_insert_mut` / `try_get_or_insert*`**: 既存 `get_or_insert_with` の拡張
- **`resize(new_cap)`**: per-shard で trim する形だと SIEVE の hand 不変条件との接続が論点

スコープ外 (意図的に見送り):

- TTL / TTI、weighted size、eviction listener: thin SIEVE crate の路線から外れる
- builder パターン: 現状ジェネリック軸 (SlotSize / SHARDS / BuildHasher) で吸収
- `FromIterator<(K, V)>`: senba の `capacity` は eviction policy のパラメータで「全部入れる箱」のサイズではない、`size_hint` 起点の容量推定は誤誘導
- async API: 並行 surface 化 (§A.3) の派生

### A.2 surface 内部の構造的最適化

shift-on-evict 化と AlignedTags 32B 化と visited bitmap 化を経て構造が変わったため、再評価が要るもの:

- **A.2.1 c-hoist trick の再確認**: `tag & ID_MASK = id × S::SIZE` は SLOT 単位で保たれている (`sieve-cache-shift-on-evict` で perf-gate 改善)。が、shift で `tags` 配列が `cap` まで縮んだ事による SIMD scan 窓の縮小 (旧 j8 比 半分) が inner ループの cycle 配分にどう影響したかは詳細プロファイル未実施。
- **A.2.2 tags 配列の prefetch / chunk overlap**: `_mm_prefetch` で次 chunk を投げる、または 2 chunk を交互に走らせて port を分散する案。Path A (見つかった) と Path B (見つからない) の cycle バランスが c-hoist で 14:5 になっている現状から、Path A 側を更に縮める余地。
- **A.2.3 `hand` の管理**: shift-on-evict 化で `hand = if pos < last { pos } else { 0 }` の分岐自体の cost と、複数 evict が連続するときの hand 動線について未検証。samply での再プロファイルが必要。
- **A.2.4 large V (`[u8; 256]` 等) での hot/cold split**: `Slot32` default では V が大きい型は上ブラケット (`Slot64`) に逃すため tags scan は L1 残留する。`entries` の cache miss が支配的になる帯域は未測定。

### A.3 並行 API の library surface 化

c16s 採用後に、`senba::Cache` と二系統のままにせず **`&self` で動く並行 API を本 crate に取り込む** 道を別スペックで議論予定 (`senba-sievecache-design` §scope 外として明記)。`SlotSize` ジェネリックと seqlock-via-tag (or G2-α/β) の同居が論点。

### A.4 perf-gate の維持

`research/benches/sieve_cache_perf.rs` は library surface の安定契約。新 API / 内部最適化追加時は **必ず 6 シナリオ全てで baseline 比較**、5% を超える regression は調査対象。±2-3% の自己分散は revert 教訓 T4 (calibration を最低 2 回取って drift を測る) を運用則として持つ。

---

## §B. ST 路線で残る最適化 — `find_avx2` 周辺

### B.1 Tier-S / A の残り (caller-merge 後の未着手分)

`find-avx2-caller-merge.md` で第 3 試行 (NonZeroU16 + A3 + `#[inline]`) が **採択済み**。Tier-S / A のうち未着手は以下:

| ID | 内容 | 期待 (机上) | リスク | 出典 |
|---|---|---|---|---|
| ~~**S3**~~ | ~~`Shard` フィールド並び替え (`len` を 1st cache line に上げる、cold field を後ろ)~~ | ~~cold-self 1 line/call 削減~~ | ~~ほぼゼロ~~ | **採択済** → §I, `2026-05-10-shard-layout-s3-capacity-removal` |
| **S4** | Slot16 monomorph で `vpbroadcastd .LCPI` がループ内再構築されている件の reg-alloc 是正 | Slot16 限定 −4 cy | 中 (LLVM reg-alloc 説得が要、原因特定先行) | frontier §S4 |
| **A1** | inner candidate ループ unroll ×2 (Path A の `tag1 load → cmp+load` と `tag2 load → cmp+load` を独立 dep chain に) | per_shard=64 で大、ps=16 で小 | **中〜高** (LLVM が cmp 越し load hoist を出さない既知ハマり、`asm!` or `black_box` 強制要) | frontier §A1, pdep-pext-revert §3-T3 |
| **A2** | `len == MAX_PER_SHARD` 専用 4-chunk specialized find_avx2_full | chunk 間 branch 3 個削減 ≈ −3〜−6 cy/scan | 低 | frontier §A2 |
| **A4** | VISITED set を `find_avx2` 内で byte-store 化 (caller の RMW を消す)。**ただし visited-bitmap 採用後は visited が tag から離れて per-shard u64 に集約済みなので前提が変わっている** — 再評価が要 | memory dep chain −1 | 中 (peek 系との semantic 衝突、const generic で 2 版に分岐) | frontier §A4 |

着手前に **OQ-1 / OQ-2 / OQ-3 (frontier §7)** の確認を asm レベルで済ませる。

### B.2 AVX-512 (V1 / V2 / V5)

`find-avx2-avx512.md` で server share と downclock 影響を整理済。`avx512-vl` / `avx512-zmm` の二段 cargo feature で opt-in、AVX-512 が無い CPU は runtime detect で AVX2 path に自動 fallback。

| ID | 内容 | 期待 (机上) | 必要 CPU | 出典 |
|---|---|---|---|---|
| **V1** | AVX-512 VL + kmask (256-bit 維持、no downclock)。`vpcmpeqw_mask` で `vpmovmskb` (5cy) + BLSR pair (2cy) を一掃 | per_shard=16 −0.8 ns / per_shard=64 −3 ns | Skylake-X+ Intel server / Zen 4+ | avx512 §V1 |
| **V2** | 512-bit zmm 幅で chunk 数半減 (per_shard=64 で 4 chunks → 2 chunks) | 更に −2 ns/op | downclock 要注意 (Skylake-X / Cascade Lake)、Sapphire Rapids+ / Zen 4+ では非問題 | avx512 §V2 |
| V3 | masked vpgather で chunk 内全 candidate を一発 fetch | per_shard=64 高 N_cand 帯で本領発揮 | u32 / u64 K 限定 | avx512 §V3 |
| V4 | `vpcompressw` で matched lane を front-pack | candidate 数が確定して loop 回数が定まる効果 | VBMI2 (Ice Lake server / Zen 4+) | avx512 §V4 |
| **V5** | **V2 + B1 合成**: per_shard=64 を **outer ループ無し / 1 zmm shot** で完結 (64 u8 lane / zmm) | per_shard=64 で −7 cy/scan | avx512 + B1 改修 | avx512 §V5 |
| V6 | `vmovdqu16_mask` で partial chunk の predicated tail load | 限定的 | (低優先) | avx512 §V6 |

着手順 (avx512 §5 推奨): **S3 → S4 → P3 → V1 → A2 → V2 / V5 prototype 比較**。

### B.3 構造改修 Tier-B

| ID | 内容 | 出典 |
|---|---|---|
| **B1** | SoA tag split: `tags_scan: Vec<u8>` (LIVE+7-bit hash) + `tags_meta: Vec<u8>` (id+aux)。`_mm256_cmpeq_epi8` で 32 lane/ymm = chunk 数半減。AVX-512 V5 と最強相乗 | frontier §B1, avx512 §V5 |
| **B2** | u32/u64 K 限定の 32-bit 拡張 tag (上位 16-bit に hash 16 bit 載せて inner key 比較を消す)。false-match 率 ≈ 2^-24 ≈ 6e-8 | frontier §B2 |
| B3 | AVX2 `_mm256_i32gather_epi32` で複数 candidate 一括 gather。Skylake では遅い、Ice Lake+ で改善 | frontier §B3 |
| B4 | tags + entries を 1 alloc に co-locate。実利は限定的、構造改修は重い | frontier §B4 |
| (P3) | PDEP で `needle_from_hash` を 1 命令化 — revert 済みだが「priority 低、SIMD 本体最適化が頭打ち後に戻る」と整理 | pdep-pext-revert §4 |

`find-avx2-pdep-pext-revert.md` 教訓: P2/P3 再挑戦時は **B1 prototype と先に比較** + **`asm!` 直書き or `black_box` で LLVM hoist 限界を自前で潰す** + **calibration 2 回**を前提条件として課す。

### B.4 shift-on-evict 周辺

- **C.2 SIMD memmove**: `tags.copy_within(pos+1..len, pos)` は per evict O(shard 内残数)。per_shard ≤ 64 で絶対量は小さいが、頻度が高い steady-state で測ると効く可能性。SIMD memmove / unrolled copy / 「shift 方向反転で常に短い側を動かす」等。
- **C.3 `Borrow<Q>` 経路の codegen 確認**: `get<Q>` で hash 計算は `H<Q>` で 1 回のみだが、generic 化で codegen が膨らんでいる可能性は要 objdump 確認。

### B.5 j8 退行帯の解消

`st-twitter-5cluster` で **Zipf ≥ 1.79 かつ cap ≥ 16384** の高 hit ratio 帯で +0.5〜+4.9 ns 退行が出ている。`j8-candidate-loop-analysis` の Δ単一式 `5cy × N_cand + 7cy × ΔN_false` の右辺を縮める手は per_shard 縮小 (16) で実施済 (`sieve-j8-bench` §8) だが、cap=65536 のような実用大 cap では per_shard=16 にすると SHARDS=4096 まで膨らむため別軸必要。**visited-bitmap で HASH_MASK が 14→15 bit に拡張済**なので退行帯の baseline は更新が必要。

---

## §C. 並行版 SIEVE — c17s 以降

### C.1 c8 派生変種 (V: Copy 制約の解消系)

c17s が entry-level seqlock で `V: Clone` soundness を獲得して以降、`V: Copy` 限定の c8 lineage は library 候補から外れているが、research 軸として残価値:

| ID | 内容 | 動機 | 優先度 |
|---|---|---|---:|
| `c8a` | `V: Clone` 化のため内部 `Arc<V>` wrap、reader は Arc clone (refcount inc) のみで生 V に触れない | c17s と異なる approach として比較対象。c17s は seqlock + V::clone mid-flight UB 残 (Arc<V> は構造的回避) | 中 |
| `c8e` | `crossbeam_epoch` で writer の drop を遅延、reader は raw 参照保持 | `V: Clone` 任意で zero-copy read。c17s V::clone soundness 限界の代替案として | 中 (複雑度高) |
| `c8s` | SIMD scan を Rust メモリモデル上完全合法な形 (`portable-simd` or `AtomicU16` 配列を 16 回 `load(Relaxed)` 相当) に書き換え | 現状 c8 の灰色領域を解消 | 中 |

### C.2 並行 sweep の残軸

`c8-vs-moka-thread-sweep` / `c8-vs-c9-thread-sweep` で SHARDS=256 / cap=16384 / T={1,2,4,8,16} は地図化済。残軸:

- **SHARDS sweep**: ∈ {8, 32, 64, 256} × per-shard contention 飽和点 (※c 系新規 variant は SHARDS=64 固定方針なので、この軸は base variant 評価向け)
- **skew sweep**: ∈ {0.6, 0.8, 1.0, 1.2, 1.4} で hot key 集中度を変えた scaling 形状
- **read-heavy 比 sweep** (95% read / 5% write): production cache の典型分布
- **96-core / NUMA 機での scale 上限** (現状 12-core i5 が上限)
- **`concurrent_invariants_under_zipf` long-running soak** (Path A CAS-back 修正後の不変条件を threads ↑ ops ↑ で確認)
- **c16s での `bench_vtune_concurrent` AB**: c14s 採取済、c8 / c9 でも回せば Mutex word bouncing 仮説の追加検証 (c14s-vtune §10)

### C.3 並行 API の library surface 化 (再掲)

§A.3 と統合。

---

## §D. c15 以降の方向 — CAS state の再設計と writer lock-free 化 (旧 §G の再掲)

c10s 〜 c14s 〜 c16s の単一 shard sweep で見えた 2 つの構造的天井に対する攻め筋を living section として整理する。

### D.0 背景 — なぜ c14s/c16s で頭打ちになったか (c17s でどこまで解けたか)

c14s は Path A (= 既存キーの value 更新) のみを lock-free 化し、Path B/C (= warmup install / evict) は writer Mutex 配下に残した。c16s は per-shard hot 4 line を 1 cache line に co-locate して transfer cost を圧縮したが、§4 acceptance T1 (uniform read-heavy 16T) は依然 c11s 比 0.323 で fail のまま。これは **c14s/c16s 固有のバグではなく Path B/C の構造的競合**で、両者のスコープでは解けない。

加えて c14s は false-miss を bounded retry (racing 観測時のみ) で実装内に隠したが対症療法。seqlock-via-tag 構造を取る限り Path A の VERSION flip は reader scan に副作用を落としつづける。read-only adv-hot 16T で c13s 比 −22% の退行は EMPTY-lane 検出 SIMD overhead の固定コスト由来。

これら 2 つを構造的に解くためには、**(i) tag が抱え込んでいる責務の分離**、**(ii) writer の lock-free 化と SIEVE 等価性の両立**、の 2 軸の再設計が要る。

**進捗**: 軸 (i) は c17s (G2-α-1) で構造解消済 (adv-hot read-heavy T=16 で +27.4%、`V: Clone` soundness 副次獲得)。軸 (ii) は ζ sub-shard / writer batching / δ install-at-(hand+cap/2) / ε free-list が残課題で、c18s が示した「single-shard read は c17s で天井 → 次の天井は write/Mutex 側」を起点に攻める段階。

### D.1 G2-α. entry-level seqlock — **α-1 採用 (c17s) / α-2 反証 (c18s)**

`Entry<K, V>` に `version: AtomicU32` を持たせて Path A は entry 側 version 偶奇 flip で reader を seqlock、tag は `LIVE | ID | HASH` のみで Path A 中不変 — の構想。実装は 2 派生に分岐:

- **α-1 (entry 同居)**: `Entry { version, key, value }` で entries[id] が単一 line。**c17s として採用** (§I 参照)。adv-hot read-heavy T=16 で c16s 比 +27.4%、`V: Clone` soundness 獲得 (c14s/c16s の `String` SIGABRT を構造解消)、Slot16 → Slot32 化を代償として skew=1.0 gim T=4 で c16s 比 −4.7%。
- **α-2 (versions 別配列)**: `Entry { key, value }` (Slot16 復帰) + `versions: [AtomicU32; 64]` 別配列。**c18s として REJECT** (§I 参照)。Slot16 で footprint 削減できたが reader cache line touch +2 (versions 別 line + path_c_epoch 別 line) のコストが entries 半減の利得を上回り、gate (b) −8.5% と c17s より悪化。

**結論**: G2-α 系の long-term winner は α-1 (c17s)。α-2 系の「reader-touch field を分離すれば writer 干渉が減る」直感は writer 同 line touch 確率が低い場合 false positive (c18s-results §7.4)。c17s 上の minor tuning (Step 1 `len.load` 削除採用 / Step 2 `path_c_epoch` 独立 line 化 reject) も §I 参照。

更なる G2 系発展案は § D.2 G2-β (SoA state、全配列分離 + AVX-512 統合) に移行 — naive な部分 split を避け、tags/versions/visited 全部を一度に SoA 化する全 in 案。

### D.2 G2-β. SoA state (multi-word state)

```rust
struct Shard<K, V, S> {
    tags:    AlignedTags,             // [u16] = LIVE | id | hash (scan 専用、Path A で不変)
    slots:   Box<[AtomicU32]>,        // version + lock-bit + epoch (同期/所有権専用)
    visited: Box<[AtomicU64]>,        // c11s/visited-bitmap 由来
    entries: UnsafeCell<Box<[MaybeUninit<Entry<K, V>>]>>,
}
```

- **効く**: α-1 (c17s) の Slot32 化 footprint 増加なし (Slot16 維持) / B1 SoA tag split / AVX-512 V5 統合の前提
- **犠牲**: slots 配列 (cap=64 で 256B = 4 cache line) が第三の hot 配列に / reader load +1
- **重要前提 (c18s 教訓)**: α-2 (versions だけ別配列に切り出し) は **reject** 済。β を試す場合は **tags / versions / visited を一度に全部 SoA 化**、かつ AVX-512 V5 (B1 SoA tag split 統合) で reader scan を 1 chunk shot 化する形でないと、partial SoA は reader cache footprint 増加が支配的になる。「naive part-split は逆効果、all-in でないと意味がない」が `c18s-results` §7.4 / §11 の結論

### D.3 writer-side batching (c14s-vtune §8.2, c16s-results §9 筆頭)

writer が一度 Mutex を取ったら、自スレッドの **次の数 op を looking ahead で消化** してから release する。Caffeine 流の write batching を per-shard レベルで縮小実装。

- **write_batch_size=8 で transfer cost を 1/8 に amortize**
- **論点**: latency 影響 (batch 待ちで p99 悪化) / c14s の eviction 順序保存制約を壊さないか / batch 内での Path A vs Path B/C の interleave
- c16s 採用後 Memory Bound 15.9% / L3 Bound 15.5% (E-core) が残っており、tags / entries 列の transfer はまだ削れる

### D.4 G3. Path B/C writer state machine の lock-free 化

c12s (install-at-evicted-pos) で **SIEVE 保護期間が消滅して HR -10〜-72%** で棄却 (`c12s-cas-slot-claim`)。再挑戦案:

| 案 | 動作 | lock-free 度 | SIEVE 等価 | 実装複雑度 | HR 劣化リスク |
|---|---|---|---|---|---|
| **G3-δ** install-at-(hand+cap/2) | 新 entry を `(hand + cap/2) mod cap` に install、近傍に EMPTY が無ければ FAA で hand を進める | full | 統計的等価 (要検証) | 中 | 中 |
| **G3-ε** free-list 経由遅延 install | evict した pos を即 install せず lock-free MPMC queue (Vyukov / BAQ 等) を介して時間 decouple | full | 構造的に保護期間担保 (queue 深さ依存) | 高 | 低〜中 |
| **G3-ζ** per-shard sub-sharding | 1 shard を K 個の sub-shard × cap/K に分割。各 sub-shard は独立に SIEVE state machine + 独自 Mutex | per-sub-shard Mutex 残 | sub-shard 単位で自明 | **低** | 中 |

**ζ が最薄で T1 を 1/K 化できる**ので筆頭。δ/ε は writer Mutex 完全消去だが oracle 検証が hairy。

### D.5 LongAdder 流 packed visited bitmap (write-contention-design-space §6)

c11s + visited-bitmap で reader visited 経路は MESI Shared 維持済だが、hot key の `fetch_or` traffic は依然 1 line に集中する。LongAdder 流 (cache line 分離 + OR merge) を **8-shard packing で 8× 圧縮** (SHARDS=256 / N_LANES=8 で naive padded 128 KB → packed 16 KB)、HR 副作用ゼロで write contention を構造除去できる候補。

```rust
#[repr(align(64))]
struct VisitedLine {
    shards: [AtomicU64; 8],   // 8 shard 分の visited を 1 line に packing
}
struct VisitedTable {
    lines: Box<[VisitedLine]>,  // SHARD_GROUP × N_LANES
}
```

- **reader**: `lines[group * N_LANES + lane].shards[word].fetch_or(...)` で同 group の連続 shard が同 line に hit (spatial locality)
- **hand merge**: N_LANES 本の cache line load + N_LANES atomic AND が新たな merge cost。eviction-heavy で N=4 / 動的調整 / epoch-based skip など要検討

c15s sloppy visited reject の量的根拠 (`c14s-vtune §7`): visited 単独で殴っても全体 mem stall の 1/3 しか触れない。**残り 2/3 (writer state + Mutex) は c16s で削れた**ので、visited を packed LongAdder で殴る前提条件は整った。

### D.6 hot-key replica tier (write-contention-design-space §5)

Count-Min Sketch で hot top-K を検出、別の lock-free read-only replica table に複製。hot read は replica から答えて shard を踏まない。Caffeine の admission window と同じ思想で 2-tier 化。

- **効く**: hot subset の write traffic を構造ごと逃がせる
- **犠牲**: 実装が大きい / senba の薄さと折り合いが悪い / replica の write coherence で別問題が出うる
- **現スコープでは見送り候補**、long-term 課題

### D.7 G4 推奨着手順と組み合わせ戦略

α-1 は c17s で着地済、α-2 は c18s で反証済 (両方 §I)。c17s をベースに次の着手:

| 順位 | 案 | 効く課題 | 着手難度 |
|---:|---|---|---|
| 1 | ζ sub-shard | T1 uniform read-heavy 16T fail を 1/K 化 | 低 |
| 2 | writer batching (§D.3) | c16s/c17s 採用後の残り Memory Bound 15.9% / write 側天井を圧縮 | 中 (順序保存設計が論点) |
| 3 | packed LongAdder visited (§D.5) | reader visited 経路の最後の hot line | 中 |
| 4 | β SoA state (all-in) | α-1 の Slot32 化 footprint 解消 + AVX-512 V5 統合の前提。**partial SoA = α-2 は反証済**なので β を試すなら tags/versions/visited を一度に全部 SoA 化 + AVX-512 V5 化が必須 | 高 |
| 5 | δ install-at-(hand+cap/2) | T1 を更に押し下げる (Mutex 完全消去) | 中〜高 |
| 6 | ε free-list 経由 | δ で HR が崩れた場合の代替 | 高 |
| - | γ 2-word tag (棄却傾向) | AVX-512 dispatch が崩れる、long-term で β に分 | - |

「c18s が示した『single-shard read は c17s で天井』ならば、次の天井は write/Mutex 側にある」 (c18s-results §9.2) — ζ/writer batching 系を ROI 上位とする。

### D.8 G5 open questions

- α-1 (c17s) の `version` 増分は `AcqRel` (CAS odd 化 / store even-back) で実装済だが、Loom / formal concurrency 検証は未実施 (c17s-results §7 scope 外)
- α-1 `V::Clone` soundness の限界: reader が seqlock pass 後 V::clone mid-flight で並行 Path A が old V drop すると UB の可能性 (V=Copy なら問題なし、heap-owning V 本番用途は Arc<V> / Epoch GC が要、`sieve_c17s.rs` module doc 参照)
- ζ の sub-shard 数 K は静的ジェネリックか動的か。`Cache<K, V, S, SHARDS, SUB_SHARDS, H>` まで膨らむなら API 設計の論点
- β all-in の slots 配列の word サイズは `AtomicU32` か `AtomicU64` か。`AtomicU64` なら version (32) + lock_bit (1) + epoch (31) で ABA 防止
- δ の保護期間 cap/2 が NSDI'24 の "1 cycle 保護" に必要十分か、論文証明を array semantics に当てはめて再検証要
- α-1 と c14s bounded retry の共存価値 (Path C 経由の false-miss は α-1 + path_c_epoch 後でも残る、bounded retry は c17s 内で `racing` flag + epoch 不変観測の OR で実装済)
- writer batching 中の read-side observer が batch 中間状態を観測した場合の seqlock 設計

### D.9 既知の race / clean-up

- **Path A vs `writer_update_in_place` race**: `entries[id]` 2 重書き。c14s/c16s では id 重複は引き起こさないので既存 test で検出不可。c17s では Path A も `writer_update_in_place` も entry version CAS / spin-claim で排他済 (`sieve_c17s.rs` 参照)、構造的に解消した可能性が高いが test 設計は依然必要 (`c13s-c16s-path-a-cas-back` §3.3)
- ~~**read-only adv-hot 16T で c14s/c13s = 0.78 退行**~~: c17s 採用で構造解消、EMPTY-lane SIMD 検出を削除済 (`c17s-design` §2.4, `c17s-results`)
- **cluster034 で visited-bitmap の利得が薄い件**: replace 分岐支配の仮説を trace 採取で検証 (`visited-bitmap` §4.2.1)

---

## §E. データ構造 / レイアウト (cap-fits 帯 senba < orig 対策)

`vtune-windows-orig-vs-senba.md` で cap-fits 帯 senba < orig が +12–16% (Windows native) と確定。cacheline dispersion 仮説は反証されたが、**instruction footprint (+17%) と L3 queue 圧迫**が主因として残る。

### E.1 Slot8 ブラケット (256 ent/shard)

cap=1M で shards を 1/4 圧縮 → instruction footprint +17% / L3 queue 圧迫の直撃緩和。AVX2 SIMD probe は 32-tag batch なので 256 ent/shard でも 8 batch で済み、per-shard scan の伸びは支配的にはならない見込み。

- **論点**: per-shard 上限 (現 6-bit ID 制約) を 8-bit に拡張する構造改修 / `SlotSize` ブラケットへの追加 / oracle 等価性確認
- **visited-bitmap との併用前提**: Slot8 は Entry が極小なので tag 内 bit 数の制約が更に厳しく、visited が tag から離れているのが前提条件

### E.2 shards 上限の導入

`next_pow2(min(ceil(cap/64), MAX_SHARDS))` で大 cap で per-shard を膨らませる方向。E.1 と二者択一に近い。`Cache::new(cap)` の auto-shard heuristic は Windows native でも +12–16% コストで再評価対象。

### E.3 `Cache::prefetch(&key)` API

senba は BW-dominated だが Memory Latency も 22.5% 残る。caller がループで持つ次キーで `entries[id]` を投機 prefetch。op 1 個分 (~280ns) の lookahead が取れれば memory latency を完全に隠蔽可能。

- **論点**: `&key` から hash → shard → tag scan → id → prefetch の前段だけ走らせる API 形 / prefetch hint が外れる cold path での無駄 / ベンチで効きが見える条件

### E.4 instruction footprint 削減

senba の load +42% / store +44% のうち、削れる成分の精査:

- `Shard::find` の tag scan 後の `entries[id]` deref を 1 cacheline アクセスに収められるか
- visited bit の更新を AVX2 mask 内で完結させて store を削れるか (visited-bitmap で per-shard u64 化したので AVX2 path に乗せ直す余地)
- shard dispatch (hash 計算 + shard index 計算) を inline で共通化できるか

両者 IPC 1.10 で揃っているので命令数 −10pp で cycle −10pp (vtune-windows §6 検証案 5)。

### E.5 full-state specialization (`len == MAX_PER_SHARD` 専用 path)

steady-state (insert ≫ remove) では `len == cap` が常時成立する。この不変条件で **constant propagation cascade** を引き出し、hot path 全域から `len` 依存の分岐 / load を消す案。§B.1 A2 (chunk 数固定) の発展系で、`find_avx2` だけでなく `insert` / `find_evict_pos` まで full 専用 path に分岐させる。

const fold される箇所:

| 場所 | 一般 path | full 仮定 |
|---|---|---|
| `find_avx2` chunk 数 | `ceil(len/16)` | `4` (= §B.1 A2) |
| `find_evict_pos` bounds | `len` | `cap` 定数 |
| `hand` wrap | `if hand >= len` | `& (cap-1)` (power-of-two AND) |
| shift-on-evict 長 | `len - pos - 1` | `cap - pos - 1` |
| visited mask | `live & !visited` | `!visited` |
| Path A 後の install 分岐 | `if len < cap { install_empty } else { evict_then_install }` | 常に evict-install |

完全 branchless 化が出るのは `find_avx2_full` (4 chunk OR-merge + `tzcnt` 一発、早期 exit 放棄) と `find_evict_pos_full` (visited bitmap の `tzcnt` ベース) のみ。shift-on-evict は `pos` が runtime なので shift 長 dynamic、branchless にはならない。

#### 段階

| 段階 | 内容 | 期待 | 静的 binary 増 |
|---|---|---|---|
| α | hot path の warmup 分岐に `unlikely` + warmup 関数を `#[cold]` で outline | LLVM の layout 整形のみ、constant fold は出ない | 中立 (warmup を別 page に逃すだけ) |
| β | `find_avx2_full` (= §B.1 A2 そのもの) | chunk 数 const、chunk 間 branch 削除 | +1 関数 |
| γ | `insert_full` / `find_evict_pos_full` を outline、`Shard::is_full()` で dispatch | const fold が hot path 全域に cascade | +2〜3 関数 |

#### 論点

- **dynamic L1I working set**: α で warmup 関数は再ロードされず自然枯死、specialize 前と steady-state working set はほぼ同じ。むしろ steady-state code が straight-line で密になり縮む可能性すらある
- **静的 binary size**: γ で確実に増加。senba は cache library として caller の hot path に inline される前提なので、**`#[cold]` + outline で full 版だけが caller 側 inline 対象になる形を厳守** — caller 側 I-cache を warmup コードで圧迫しない
- **§E.4 (命令数削減) との方向対立**: E.4 が「同じ動作を細い命令列に」なのに対し γ は「同じ動作を 2 本の太い命令列に分ける」。footprint cost の符号が逆なので、両者は AB で総合 ROI を比較する関係。α 単独は E.4 と互換 (footprint 中立)
- **transition cost**: warmup → full の遷移は per-shard で `len == cap` が初成立した 1 回のみ。dispatch は `if shard.len == MAX_PER_SHARD` の 1 cmp、predicted-taken でほぼ free
- **remove-heavy workload**: per-shard が warmup と full を頻繁に行き来し dispatch ミス + outline cold path 復活。SIEVE 想定の典型 workload (insert ≫ remove) ではないので許容範囲
- **vtune-windows §6 で観測された +17% instruction footprint との関係**: vtune-windows は静的 footprint 寄りの観測。γ は α/β と違い静的 footprint を増やす方向 = vtune-windows の主因仮説と正面衝突。先に α + β を試して β で頭打ちなら γ に進む順が筋

#### 期待値と判断

机上 5–15% / op、ただし constant fold が cascade するかは LLVM 機嫌依存で実測必須。**第一手は α の `unlikely` hint だけ入れて vtune の front-end bound / I-cache miss 変化を観測**、β は §B.1 A2 として独立着手、γ は α/β の結果次第で go/no-go 判断。

---

## §F. 計測軸の拡張

### F.1 workload 軸の残

過去レポートで未消化:

- **D1. skew=0.5 (uniform 寄り)**: eviction が支配的で SIMD scan の真価が出る条件。`j5-pershard-pareto` で skew ∈ {0.9, 1.0, 1.2} まではある
- **D2. 大きい V (`[u8; 256]`, `Vec<u8>` 等)**: hot/cold split の効きと `SlotSize=Slot64` ブラケットでの cache footprint 影響
- **D3. churn-heavy** (req >> capacity × 100): orig の linked-list pointer chasing 悪化 / array が逆転する条件
- **D4. mixed read/write 比率**: 現状 micro bench は insert_only。`get`-only / `mixed` の 3 種で hit-path コストの相対重要度

`bench_concurrent.rs` は read-write 比を変えられるので並行 sweep §C.2 と D4 は実質統合可能。

### F.2 メソドロジー (revert 教訓 → 運用化)

- **calibration runs**: perf-gate baseline は最低 2 回取って自己分散を測ってから signal vs noise 判定 (revert §3-T4)
- **bare Linux で `perf stat -e dTLB-load-misses,iTLB-load-misses,L1-dcache-loads,page-faults,LLC-load-misses`**: bench_vtune を Linux ELF cross-build、THP 仮説 (Linux が orig の STLB miss を消している) を直接検証
- **GitHub Actions one-shot triage**: `workflow_dispatch` で bench_vtune を ubuntu-latest 上で → WSL2 ≠ Linux generic を切り分け
- **VTune Memory Access の Per-allocation breakdown**: `senba::Shard::tags` / `senba::Shard::entries` / `sieve_orig` の `Box<Node>` arena の per-allocation latency 分布で L3 queue 圧迫の主因を特定
- **Windows Large Page で orig の TLB pressure を消す対照実験**: Linux THP 仮説の傍証 (要 SeLockMemoryPrivilege)

### F.3 perf-gate scenario の追加候補

`external-lib-sweep` / `mokabench-arc-traces` の知見から:

- **OLTP cap=2000** (HR 勝ち) + **MergeP cap=400k** (HR 勝ち) + **Zipf-1.0 cap=4096** (HR tie) の 3 点で hit/miss/tie の 3 face をカバー
- **DS1 / S3 large cap 帯** の signal を取り込む (trace I/O を criterion bench に乗せる仕掛けが要、別途設計)
- **OLTP は perf-gate 候補確定**、S3 は signal 無く却下、DS1 / spc1likeread は未検証 (spc1likeread は split zst 連結処理が要追加工事)

### F.4 大 cap で orig に負ける帯の解析

Zipf cap=32k と ConCat 1M で senba < orig になるのは shard 分散の oversharing が原因か、SIMD find の cold tag load が支配的になるかを切り分ける。**vtune-windows で cacheline dispersion 仮説は反証**され、instruction footprint / L3 queue 圧迫が主因と確定したが、Linux 側の OS 因子 (40% gap のうち 24–28pp) は未確認。

---

## §G. 兄弟アルゴリズム / 比較対象拡張

- **E1. S3-FIFO 移植**: SIEVE の兄弟 (Yang 2023, NSDI'24 同チーム)、論文では多くの workload で SIEVE と同等以上 HR。実装 2-3 日。比較対象として価値高い
- **E2. moka 0.12 / mini-moka 0.10 の追測**: `j8-vs-mini-moka-twitter` §10 で Zipf + Twitter 5 cluster までは比較済。残るは大規模 cap (cap=1M〜) と W-TinyLFU が SIEVE を抜く条件の探索
- **E3. libCacheSim FFI で C 実装を直に bench に取り込む**: `sieve-c-vs-senba-twitter52` で「C 実装は libCacheSim ハーネス込みで 4-6× 遅い」が判明、純 algorithm 比較には FFI 取り込みが要
- **E4. ARC paper trace の追加**: mokabench 由来の `--source arc` は既装備、DS1 / spc1likeread 未検証 (`mokabench-arc-traces` §follow-up)

---

## §H. open questions (横断)

- §D.1 α-1 (c17s) の `version` ordering / Loom 検証 (再掲、§D.8 G5 で更新済)
- §D.1 α-1 `V::Clone` mid-flight UB の library 露出方針 (Arc<V> 内包 vs caller 責務、§D.8 G5 参照)
- §D.4 ζ の sub-shard 数 K の API 設計 (再掲)
- §B.2 OQ-V1〜V5 (avx512 §7): V1 が実機で latency 3 cy / 1 uop に乗るか、V2 downclock 実測、AlignedTags `align(32)` → `align(64)` 変更コスト、V5 の false-match 倍増 vs chunk 数 1 化の損益、AVX10 互換
- §B.3 OQ-4 B1 SoA split の false-match 倍増 (1/256→1/128) が S1+S2 改善幅を相殺するか
- §E.1 Slot8 採用時の per-shard 6-bit ID 制約緩和の波及範囲
- §E.5 α 単独で constant fold が hot path 全域に cascade するか (LLVM 機嫌依存、`unlikely` だけで warmup branch が末尾追放されるか asm 確認要)
- §E.5 γ の静的 binary size 増加が library 利用者にどこまで許容されるか — caller の hot path への inline 戦略 (`#[cold]` outlined だけが inline 対象になるか) を含めた実測必要
- §E.5 dispatch を per-call (`if shard.len == MAX_PER_SHARD`) にするか per-shard state flag (warmup 完了で関数ポインタ書き換え) にするか — 後者は indirect branch + BTB 汚染のリスク
- §F.2 WSL2 confound の本丸: Linux THP が `Box<Node>` を 2M page promote している仮説の直接確認

---

## §I. 棄却・実装済 (履歴)

| ID | 内容 | 結末 | 参照 |
|---|---|---|---|
| A1 | hasher 統一 (FxHash / XXH3) | **実装済**: 全 variant XXH3、`senba::Cache` は `with_hasher` で注入可 | `src/hash.rs` |
| C2 / E1 | `Vec<Option<Node>>` → `Vec<MaybeUninit<Node>>` | **実装済 (orig)**、bench はノイズ範囲だが構造的正しさで採用 | `sieve-orig-overhead-analysis` |
| J2 | set-associative (Map 廃止 + hash 直接 segment) | **実装済** = `sieve_j4`、以降 j5/j7/j8 全て継承 | `sieve-j4-set-associative` |
| J3 | 全 inline (Map 廃止 + tag SIMD scan) | **実装済** = `sieve_j3` | `sieve-j3-bench` |
| J5 派生 | double-hash 排除 | **実装済** = `sieve_j5` | `sieve-j5-doublehash-ab` |
| M2.1 | visited を tag MSB に同居 (j6) | **棄却**: tag bit 削減で false-match 率倍増、Twitter 全帯域で +2.5〜+11.3 ns 退行 | `sieve-j6-m21-twitter` |
| M2.3 | tag を u16 化 (j7) | **実装済** → j8 に発展継承 | `sieve-j7-m23-twitter` |
| M5.3 | slack を tags 側のみに、+ tag 内 entry_id embed | **実装済** = `sieve_j8` (inline 20 B/cap で orig 抜き) | `j8-twitter-pareto` |
| M1 系 (slack 削減) | `order_cap = 2 × cap` の slack | **構造ごと撤去**: shift-on-evict 化で `tags` 配列が `cap` に半減 | `sieve-cache-shift-on-evict` |
| B1 (visited inline) | visited bit を Entry に inline | 旧 v3 系列。j 系列では tag 内 inline で恒常的に解決 | (j7/j8 tag layout) |
| F1 (S3-FIFO) / F2 (W-TinyLFU) | 兄弟比較 | F2 部分達成 (mini-moka / moka 0.12 with Twitter 5 cluster)、F1 (S3-FIFO 移植) は **§G.E1 に再掲** | `j8-vs-mini-moka-twitter` |
| compact トリガ緩和 (D1 旧) | 旧 j 系列の compact 頻度 | **撤去**: shift-on-evict で compact 自体撤去 | `sieve-cache-shift-on-evict` |
| jedisct1/rust-sieve-cache 比較 | 既存 Rust 実装の追評 | 設計調査で oracle 不一致 (CLOCK 寄りに縮退)、詳細 bench 見送り | `jedi-vs-orig` |
| **AlignedTags 32B 化** | `Vec<u16>` + `loadu` → `Vec<TagsChunk>` + `_mm256_load_si256` | **採択**: Twitter で u64 −3.35% / String −4.39% (geomean、cache-line split 解消) | `aligned-tags-load` |
| **find caller-merge (S1+S2 + NonZeroU16 + A3)** | `find` を `Option<(usize, NonZeroU16)>` 返り、`entry_ptr_from_tag` ヘルパで sret + shift round-trip 解消 | **採択**: insert_u64 −7.15% / mixed_u64 −10.14% / insert_u32_slot16 −9.24% / insert_string −3.22%、Twitter 9 セル全 HR 一致 | `find-avx2-caller-merge` |
| **VISITED bitmap 化** | `Shard::tags[i]: u16` の VISITED bit を `Shard::visited: u64` per-shard bitmap に分離、HASH_MASK を 14→15 bit に拡張、`find_evict_pos` を O(1) bit-twiddle 化 | **採択**: get_heavy −7.76% / mixed_lowskew −10.04% / mixed_u64 −3.05%、Twitter 14/15 セル improvement | `visited-bitmap` |
| (P3) PDEP `needle_from_hash` | call ごと −2〜3 cy 期待 | **revert**: criterion noise floor を超えなかった (重要教訓: T1 命令節約 ≠ throughput) | `find-avx2-pdep-pext-revert` |
| (P2) PEXT + inner unroll ×2 | per_shard=64 で −3〜−5 cy/scan 期待 | **revert**: 3 シナリオで +3.5〜+4.9% regression、LLVM が cmp 越し load 並列化を出さない / cand 分布で unroll 不発 | `find-avx2-pdep-pext-revert` |
| c12s install-at-evicted-pos | writer Mutex 完全排除 | **棄却**: 新 entry が hand 直前 visited=0 で入る → 即 evict 候補、SIEVE 保護期間消滅 HR -10〜-72% | `c12s-cas-slot-claim` |
| c15s sloppy visited | reader visited を 1/16 sample | **reject**: c11s conditional load-then-fetch_or が既に reader を MESI Shared 維持済、TLS RNG draw cost が利得を上回る | `c15s-sloppy-visited` |
| **c16s ShardHot co-locate** | Mutex word + visited u64 + hand + len を `#[repr(C, align(64))] ShardHot` に co-locate (3 hot line → 1 cache line) | **採択**: T=4 skew=1.0 で c14s 比 +7.9%、3 hot line abs mem stall -39% / -65% / -49% | `c16s-design`, `c16s-results` |
| Path A 最終 store-back unconditional | c13s/c14s/c15s/c16s 共通 flake | **修正済**: store を CAS (`EMPTY → T_a ^ VERSION`) に変更、debug soak で 0/200 ほか全 variant clean | `c13s-c16s-path-a-cas-back` |
| **S3 + capacity field 削除** | `Shard` を `#[repr(C)]` 化、フィールド再配列、`capacity` field 削除 (`entries.len()` と恒等、`#[inline] capacity()` 経由)。read hot path 4 フィールド (tags/entries/len/visited) を cache line 1 に閉じる。`offset_of!` const-eval で契約化 + asm verify 完了 | **採択**: sizeof 112B → **104B**、criterion geomean −1.8% (`get_heavy_u64` −4.32% p=0.01)、Twitter 60 cells perf-neutral (HR 完全一致)、ARC OLTP cap=2000 −2.65%、ARC DS1 で 3/3 cells improved (geomean −2.19%) | `shard-layout-s3-capacity-removal` |
| **G2-α-1 entry-level seqlock (c17s)** | `Entry<K,V>` に `version: AtomicU32` 同居、tag は `LIVE\|ID\|HASH` のみ Path A 中不変。reader は entry version 偶奇 + tag re-load の 2-tier seqlock、Path C false-miss は `path_c_epoch` coarse seqlock + bounded retry | **採択**: adv-hot read-heavy T=16 で c16s 比 +27.4% (Step §11 で tier 2 削除して +29.1% に伸長)、`V: Clone` soundness 獲得 (c14s/c16s の `String` SIGABRT 解消)、library 候補は c17s 系統一択。代償は Slot32 化 (Entry 16B → 32B) と skew=1.0 gim T=4 で c16s 比 −4.7% (受容) | `c17s-design`, `c17s-results`, `cseries-string-baseline` |
| G2-α-2 versions 別配列 (c18s) | α-1 の Slot32 化が gate (b) 退行主因の仮説で `Entry` から version を抜き `versions: [AtomicU32; 64]` 別配列 + `path_c_epoch` も ReaderState 独立 line に分離して Slot16 復帰 | **REJECT**: gate (a) は +6.5% で通過するが c17s +23.8% から大幅縮退、gate (b) −8.5% (c17s −4.7% より更に悪化)、thread sweep 全 T で c17s に劣後。root cause は reader cache line touch +2 が entries 半減の利得を上回ったこと。教訓: naive な field split は逆効果、SoA するなら all-in でないと意味がない | `c18s-design`, `c18s-results` |
| c17s Step 1 `len.load` 削除 | `find_get` / `find_lockfree_for_path_a` から `hot.len.load(Acquire)` + `pos < len` 分岐を削除 (TOCTOU 安全: Path B は tags[len].store(LIVE, Release) で entry init 後に発火、tags[pos≥len] は EMPTY pad、len monotonic 増加) | **採択**: perf neutral (gate (a) -1.6% / gate (b) +0.9%、noise band 内) だが disasm で atomic load -1 + branch -1 を確認、code が cleaner。`path_c_epoch.load` が直前に同 line を L1 prefetch するため len.load は元から free だったのが neutral の理由 | `c17s-step1-len-load-removal` |
| c17s Step 2 `path_c_epoch` 独立 line 分離 | ShardHot から `path_c_epoch` を取り出し `#[repr(C, align(64))] EpochLine` に分離、Path A の `visited.fetch_or` による epoch line MESI invalidate 解放を狙う | **REJECT**: gate (a) -0.4% / gate (b) -0.5% / 低 T gim sweep で系統的 -1〜-2%。c18s §9.3 の「reader cache footprint +1 line のコストが writer 干渉低下を上回る」原則が path_c_epoch 単独でも成立することを追加検証。副次 learning: thread sweep の old c17s T=16 が thermal throttle で +15.8% の見かけ改善幻覚を産んだ、controlled 5-trial で neutral 着地、WSL2 単発 trial は p99 を必ず見る | `c17s-step1-len-load-removal` |
