# SIEVE 実装 改善アイディア (living doc)

このドキュメントは date-stamped レポートではなく、**現時点で未検証のまま残っている改善案の一覧** を保持する場所。日付つきの実験レポートは `docs/reports/` を、横並び比較は `docs/api-comparison.md` を参照。

各案には参照レポートを併記し、検証 / 採択 / 棄却が出たら本ドキュメントから「§I. 棄却・実装済 (履歴)」へ移動する運用。**ここに残っているもの = まだ実測 or 実装で潰せていない案**、と読んでよい。

---

## §0. 現状サマリ (〜2026-05-10)

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
| **`sieve_c16s`** | c14s の per-shard hot 4 line を `#[repr(C, align(64))] ShardHot` に co-locate (Mutex word + visited u64 + len + hand)。**T=4 skew=1.0 で c14s 比 +7.9%、3 hot line すべて abs mem stall -39% / -65% / -49%、Memory Bound 22.0% → 15.9%、CPI 0.526 → 0.449。採用** | `c14s-vtune-write-contention`, `c16s-design`, `c16s-results` |

ベースラインは ST が `senba::Cache`、並行が `sieve_c16s`。以降の改善は基本的にこの 2 つを起点に評価する。

### 残っている構造的天井

| 課題 | 出典 | 状況 |
|---|---|---|
| (i) 並行 T1 (uniform read-heavy 16T) で Path B/C writer Mutex 競合 | c14s-sweep §4.1, c16s-design | c16s 採用後も未解決。G3 系 (sub-shard / install-at-hand+cap/2) 待ち |
| (ii) seqlock-via-tag 由来の false-miss と read-only adv-hot −22% 退行 | c14s-sweep §4.2, improvement-ideas §G2-α | c16s 採用でも残存。entry-level seqlock or SoA state 待ち |
| (iii) cap-fits 帯 senba < orig (Windows native でも +12–16%) | vtune-windows-orig-vs-senba | instruction footprint +17% / L3 queue 圧迫が主因。Slot8 / shards 上限 / prefetch / footprint 削減が候補 |
| (iv) Path A vs `writer_update_in_place` の `entries[id]` 2 重書き race | c13s-c16s-path-a-cas-back §3.3 | id 重複は引き起こさないので既存 test 不可。別 test 設計要 |

---

## §1. 推奨優先度マップ

「(a) 効きが大きい × (b) 実装複雑度が低い × (c) 既存資産を壊さない」の 3 軸での主観評価。

### 短期で perf に乗せやすい (1 日〜数日)

| 優先 | 案 | 効く課題 | 出典 |
|---:|---|---|---|
| 1 | **§B.1 S3** Shard フィールド並び替え (`Shard::len` を 1st cache line に) | cold-self で 1 line/call 削減 | find-avx2-frontier §S3 |
| 2 | **§B.1 S4** Slot16 monomorph の `vpbroadcastd` chunk 内再構築の reg-alloc 是正 | Slot16 限定 −4 cy | find-avx2-frontier §S4 |
| 3 | **§B.1 A2** `len == MAX_PER_SHARD` 専用 4-chunk specialize | chunk 間 branch 3 個削減 | find-avx2-frontier §A2 |
| 4 | **§D.4 G3-ζ** sub-sharding | T1 uniform read-heavy 16T fail を 1/K 化 | improvement-ideas §G3-ζ |
| 5 | **§F.2 calibration runs** | perf-gate 自己分散の常時計測 (revert 教訓 T4) | find-avx2-pdep-pext-revert §3 |

### 中規模 (1 週間程度)、構造改修だが ROI 期待大

| 案 | 効く課題 | 出典 |
|---|---|---|
| **§B.2 V1** AVX-512 VL + kmask | per_shard=16/64 で −0.8/−3 ns、no downclock | find-avx2-avx512 §V1 |
| **§B.3 B1** SoA tag split (8-bit hash + 8-bit meta) | per_shard=64 で chunk 半減 | find-avx2-frontier §B1 |
| **§D.1 G2-α** entry-level seqlock | adv-hot HR + read-only adv-hot −22% 回収 | improvement-ideas §G2-α |
| **§D.3 writer batching** | per-shard 単位で transfer cost を amortize | c14s-vtune §8.2, c16s-results §9 |
| **§D.5 packed LongAdder visited** | reader visited contention を 8× 圧縮 (HR 副作用ゼロ) | write-contention-design-space §6 |
| **§E.1 Slot8 ブラケット** | 大 cap で shards を 1/4 圧縮 → instruction footprint / L3 queue 圧迫の直撃緩和 | external-lib-sweep §検証案1, vtune-windows §6 |

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
| **S3** | `Shard` フィールド並び替え (`len` を 1st cache line に上げる、cold field を後ろ) | cold-self 1 line/call 削減 | ほぼゼロ | frontier §S3 |
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

## §C. 並行版 SIEVE — c16s 以降

### C.1 c8 派生変種 (V: Copy 制約の解消系)

c16s が採用された今でも、c8 lineage の owned-V 版は research 軸として価値あり:

| ID | 内容 | 動機 | 優先度 |
|---|---|---|---:|
| **`c8a`** | `V: Clone` 化のため内部 `Arc<V>` wrap、reader は Arc clone (refcount inc) のみで生 V に触れない | `String` 等 owned 型を並行 cache に載せる | 高 |
| `c8e` | `crossbeam_epoch` で writer の drop を遅延、reader は raw 参照保持 | `V: Clone` 任意で zero-copy read | 中 (複雑度高) |
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

### D.0 背景 — なぜ c14s/c16s で頭打ちになったか

c14s は Path A (= 既存キーの value 更新) のみを lock-free 化し、Path B/C (= warmup install / evict) は writer Mutex 配下に残した。c16s は per-shard hot 4 line を 1 cache line に co-locate して transfer cost を圧縮したが、§4 acceptance T1 (uniform read-heavy 16T) は依然 c11s 比 0.323 で fail のまま。これは **c14s/c16s 固有のバグではなく Path B/C の構造的競合**で、両者のスコープでは解けない。

加えて c14s は false-miss を bounded retry (racing 観測時のみ) で実装内に隠したが対症療法。seqlock-via-tag 構造を取る限り Path A の VERSION flip は reader scan に副作用を落としつづける。read-only adv-hot 16T で c13s 比 −22% の退行は EMPTY-lane 検出 SIMD overhead の固定コスト由来。

これら 2 つを構造的に解くためには、**(i) tag が抱え込んでいる責務の分離**、**(ii) writer の lock-free 化と SIEVE 等価性の両立**、の 2 軸の再設計が要る。

### D.1 G2-α. entry-level seqlock

```rust
struct Entry<K, V> {
    version: AtomicU32,  // 偶数 = stable, 奇数 = in_progress
    key: K,
    value: V,
}
```

tag は `LIVE | id | hash` のみ、Path A では **不変**。同期通知は entry 側 `version` が担う。

- **効く**: tag scan 完全 scan-clean / false-miss が API 表面から消える / EMPTY-lane 検出 SIMD overhead を削除できる (read-only adv-hot −22% の回収候補)
- **犠牲**: entry +4B / Path C (shift-on-evict) は依然 tag を動かすので **2 段 seqlock 構造** / Path B/C Mutex は残る (T1 fail は α 単独では解けない)

### D.2 G2-β. SoA state (multi-word state)

```rust
struct Shard<K, V, S> {
    tags:    AlignedTags,             // [u16] = LIVE | id | hash (scan 専用、Path A で不変)
    slots:   Box<[AtomicU32]>,        // version + lock-bit + epoch (同期/所有権専用)
    visited: Box<[AtomicU64]>,        // c11s/visited-bitmap 由来
    entries: UnsafeCell<Box<[MaybeUninit<Entry<K, V>>]>>,
}
```

- **効く**: α と同じ false-miss 解消 + entry サイズ膨張なし (small V cache の Slot16 維持) / B1 SoA tag split / AVX-512 V5 統合の前提
- **犠牲**: slots 配列 (cap=64 で 256B = 4 cache line) が第三の hot 配列に / reader load +1 / **長期ポートフォリオでは α より β に分がある**

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

| 順位 | 案 | 効く課題 | 着手難度 |
|---:|---|---|---|
| 1 | α entry-level seqlock | adv-hot HR (構造除去) + read-only adv-hot −22% 退行回収 | 低〜中 |
| 2 | ζ sub-shard | T1 uniform read-heavy 16T fail を 1/K 化 | 低 |
| 3 | writer batching (§D.3) | c16s 採用後の残り Memory Bound 15.9% を圧縮 | 中 (順序保存設計が論点) |
| 4 | β SoA state | α の long-term 上位互換、AVX-512 / B1 統合の前提 | 中 |
| 5 | packed LongAdder visited (§D.5) | reader visited 経路の最後の hot line | 中 |
| 6 | δ install-at-(hand+cap/2) | T1 を更に押し下げる (Mutex 完全消去) | 中〜高 |
| 7 | ε free-list 経由 | δ で HR が崩れた場合の代替 | 高 |
| - | γ 2-word tag (棄却傾向) | AVX-512 dispatch が崩れる、long-term で α/β に分 | - |

α と ζ は **直交** (α は同期通知層分離、ζ は state machine 多重化) なので両載せ可能。c16s に対して 1.→2.→ (3 or 5) の順が筋。

### D.8 G5 open questions

- α の `version` 増分は `AcqRel` で十分か、fence が要るか (Loom 検証要件)
- ζ の sub-shard 数 K は静的ジェネリックか動的か。`Cache<K, V, S, SHARDS, SUB_SHARDS, H>` まで膨らむなら API 設計の論点
- β の slots 配列の word サイズは `AtomicU32` か `AtomicU64` か。`AtomicU64` なら version (32) + lock_bit (1) + epoch (31) で ABA 防止
- δ の保護期間 cap/2 が NSDI'24 の "1 cycle 保護" に必要十分か、論文証明を array semantics に当てはめて再検証要
- α と c14s bounded retry の共存価値 (Path C 経由の false-miss は α でも残る)
- writer batching 中の read-side observer が batch 中間状態を観測した場合の seqlock 設計

### D.9 既知の race / clean-up

- **Path A vs `writer_update_in_place` race**: `entries[id]` 2 重書き。id 重複は引き起こさないので既存 test では検出不可、別 test 設計要 (`c13s-c16s-path-a-cas-back` §3.3)
- **read-only adv-hot 16T で c14s/c13s = 0.78 退行**: EMPTY-lane 検出 SIMD overhead を `find_get_avx2` から外す or 粗い検出 (全 chunk OR で代用) に置換する余地 (`c14s-sweep` §4.2)
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

- §D.1 α の `version` ordering / Loom 検証 (再掲)
- §D.4 ζ の sub-shard 数 K の API 設計 (再掲)
- §B.2 OQ-V1〜V5 (avx512 §7): V1 が実機で latency 3 cy / 1 uop に乗るか、V2 downclock 実測、AlignedTags `align(32)` → `align(64)` 変更コスト、V5 の false-match 倍増 vs chunk 数 1 化の損益、AVX10 互換
- §B.3 OQ-4 B1 SoA split の false-match 倍増 (1/256→1/128) が S1+S2 改善幅を相殺するか
- §E.1 Slot8 採用時の per-shard 6-bit ID 制約緩和の波及範囲
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
