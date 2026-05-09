# 2026-05-10 — 並行 write contention の設計空間と hot-key 対策の道具箱

- 関連: `2026-05-08-c14s-design.md` (Path A lock-free 化、Path B/C は Mutex 残)、
  `2026-05-08-c14s-sweep.md` (T1 uniform read-heavy 16T fail の構造的天井)、
  `improvement-ideas.md` §G (c15 以降の design lane)、
  `2026-05-10-visited-bitmap.md` (visited を per-shard u64 bitmap 化、本稿の前段)
- 種別: **設計空間の議論ノート**。具体実装・bench は無し、複数案の比較整理と次の試行計画

## 0. TL;DR

並行 SIEVE で「hot-key だけが本丸」と問題を再フォーカスし、4 つの結論を得た:

1. **並行 write contention の改善策は 4 系統に分類できる** (直列化 / 空間分散 / decouple / 楽観的 retry) — senba の文脈で各々が hot-key にどう効くかを地図化。
2. **「atomic = 速い」は誤解で、実態は MESI で hardware level に直列化される** — 数値で 5〜10 ns (uncontended) → 50〜200 ns (cross-core) → 200〜500 ns (NUMA)。lock-free の真の利得は「lock を消すこと」ではなく「**cache line ownership を分散すること**」。
3. **shard-affinity (per-shard worker thread) は moderate Zipf には効くが、hot-key には効かない** — 1 shard に集中した hot 操作はどのみち 1 worker が直列化する。Mutex を worker に置換するだけで上限は変わらない。
4. **LongAdder 流 visited bitmap (cache line 分離 + OR merge) を 8 shard packing で 8× 圧縮できる** — naive padded で 128 KB かかる memory を 16 KB に縮められ、HR 副作用ゼロで write contention を構造除去できる候補。

進め方は **(1) sloppy visited を 5 行で入れて効きを測る → (2) 効きが見えたら packed LongAdder visited で HR ロスゼロ版に置換 → (3) shard-affinity は中規模 Zipf 帯で詰まったときの保険札** の 3 段。

## 1. 出発点 — Caffeine 流 vs per-shard 改善

最初の問いは「`improvement-ideas.md` §G の per-shard 改善 (G2-α / G3-ζ 等) より、Caffeine 流の write batching + maintenance thread の方が筋が良くないか」。

Caffeine 流の構造は:
- get(): per-thread / striped read buffer に access event を ring 投入、return 値は lock-free
- insert(): write buffer に event 投入
- maintenance: 単一スレッドが buffer を drain、cache state machine を進める

**技術的優位は Caffeine 流寄り**。Path B/C の writer Mutex 競合と reader の visited 書き込み争奪を構造ごと回避できる。ただし

- maintenance thread (or amortized batching) が要る
- bounded read buffer は overflow 時に access event を drop → SIEVE の visited bit 取りこぼし → HR が原本 SIEVE と数 % ずれる
- eventual consistency になる (insert が即時 visible でない)

これは **senba が「sieve_orig と eviction 列が一致する薄い SIEVE crate」を目指している現スコープから構造的に外れる**。よって c14s 系列の延長 (G2-α + G3-ζ) を先に潰し、それでも詰むなら別 lane で `sieve_cb` (Caffeine 流) を立てる、という順序が妥当と整理。

## 2. 並行 write contention の道具箱 — 4 系統

このトピックを senba 固有から離れて整理した分類:

### 2.1. 直列化 (1-writer 原則)

- **Flat combining** (Hendler et al. 2010): 各スレッドは publication record に置き、最初に lock を取れた 1 人が "combiner" として全員ぶんを実行。N writer の cache line ping-pong が 1 combiner の sequential apply に圧縮。
- **LMAX Disruptor**: ring buffer で「各 slot は 1 producer のみ」と決め切り、CAS すら排除。
- **Caffeine maintenance thread**: 単一スレッドが state machine を独占。

スループット上限は combiner / 単一 writer の処理速度で決まる。

### 2.2. 空間分散 (privatization / per-thread shadow)

- **`LongAdder` (Java)** / **sloppy counters (FreeBSD)** / **per-CPU 変数 (Linux kernel)**: 1 個のグローバル counter を N cell に増殖、read 時に sum で復元。write は cache line を取り合わない。
- **tcmalloc / jemalloc の thread-local arena**: アロケーション要求を thread-local に閉じ、周期的に central heap と授受。
- **OpenMP / GPU の privatization**: reduce 演算を thread-local accumulator → 末尾で merge。

`improvement-ideas.md` §G3-ζ (sub-shard 化) もこの系統。**merge / sum 可能な構造でしか使えない** のが制約。

### 2.3. 読み書き decouple (versioning / append-only)

- **MVCC** (PostgreSQL, InnoDB): writer は新 version を作り、reader は snapshot 経由で見るので write lock を取らない。
- **RCU** (Linux kernel) / **epoch-based reclamation** (crossbeam-epoch) / **hazard pointers**: reader は lock 不要、writer は古い version を quiescent state まで delay free。
- **LSM-tree** (RocksDB) / **log-structured FS**: 更新は in-place せず append のみ、別軸で compaction。
- **persistent data structure** (Clojure PHM): 全 update が新木を返す、構造共有で copy cost を抑える。

senba c8 系の **seqlock-via-tag** は MVCC の lite 版。read 側の問題には効くが write 競合は解けない。

### 2.4. 楽観的 retry

- **lock-free CAS + backoff**: Treiber stack / Michael-Scott queue。失敗したら exponential backoff で衝突確率を下げる。
- **STM** (Haskell, Clojure ref): transaction を投機実行、conflict 検出で abort/retry。
- **HTM** (Intel TSX, POWER8+): hardware が transaction を best-effort で見守る。

contention 低時は速いが、Path B/C みたいに **構造的に同じ slot を奪い合う** ケースでは retry storm になる (c12s が崩れたのもこの形)。

### 2.5. senba の文脈での効き方マップ

| 系統 | senba での具体策 | T1 (write-heavy contention) への効き |
|---|---|---|
| 1. 直列化 | Caffeine 流 maintenance thread / flat combining per shard | ◎ (構造的に解消) |
| 2. 空間分散 | sub-sharding (G3-ζ) / LongAdder visited (本稿 §6) | ○ (1/K 化、上限はある) |
| 3. decouple | seqlock-via-tag (既存 c8) / α entry-level seqlock | △ (read 側、write 競合は解けない) |
| 4. 楽観 | install-at-evicted-pos (c12s) | ✕ (実証的に崩れた) |

## 3. cache invalidation コストの実態

「multi-thread の write がここまで厳しい」という点を数値で押さえ直した:

| 操作 | 単独レイテンシ | 競合下 | 倍率 |
|---|---|---|---|
| atomic OR / CAS (L1 hot) | 5〜10 ns | — | 1× |
| 同 (cross-core, L3 経由) | — | 50〜100 ns | 10× |
| 同 (cross-socket / NUMA) | — | 200〜500 ns | 50× |

**「atomic = 速い」ではなく「atomic = MESI で直列化される」が実態**。同 cache line に N thread が書くと、line は常に 1 core の M (Modified) 状態 → 別 core が触る瞬間に invalidate broadcast → ownership transfer、というのを毎回踏む。これは hardware level の lock とほぼ等価で、「lock-free だけど contention あり」は「lock-based」より速いとは限らない (むしろ遅いことすらある)。

LongAdder が AtomicLong より 10-100× 速いのもこれで、答えは「atomic を速くする」ではなく「**cache line を thread 間で共有しない**」になる。

**派生する見方**: lock-free 設計の真の利得は "lock を消すこと" ではなく **"cache line ownership を分散すること"**。Caffeine も Disruptor も LongAdder も、全部この観点で読み直すと一貫している (どれも cache line owner を 1 thread に固定する仕掛け)。

## 4. shard-affinity (per-shard worker thread) 分析

「shard ごとに専用 worker を割り振り、彼らが専用に write する」案を評価。LMAX Disruptor / actor model のそのもの。

### 4.1. スケーラビリティ上限

write throughput の上限 = N_workers (≈ N_shards) × single-thread rate。各 shard を 1 thread が占有 → cache line は常に owner core の M state に固定 → MESI ping-pong が **shard 内部からは完全消滅**。

### 4.2. cross-thread transfer cost

| 経路 | 1 op あたりの cache line transfer |
|---|---|
| 直接共有 atomic (現状 c8/c14s) | N 方向 ping-pong (全 contender 間) |
| MPSC queue 経由 (shard-affinity) | 1〜2 line (queue tail + slot) |

クロスオーバーは **contender ≈ 2-3 thread** 付近。それ以上で queue が勝つ。LMAX が "1 producer/consumer pair で 100M ops/sec" を出せるのは、queue の tail/head が常に 1 core の L1 に居着くから。これは §3 の「shared を queue に変換する」変換と読める。

### 4.3. hot-key には効かない (本質的限界)

shard-affinity が解決するのは **「同じ shard に異なる key が集まったときの spurious contention」**。
hot-key は **「同じ key が同じ shard に集まる fundamental contention」** で、「1 key を直列化しないと SIEVE algorithm 不変条件 (visited bit / hand 進行) が壊れる」という構造から来る不可避なもの。

worker が 1 人になっても、その 1 worker が hot key 1 個に詰まったら他 shard が遊ぶ構図は不変。**Mutex を worker thread に置き換えただけ** で、hot shard が bottleneck な構図は解けない。

| 解決対象 | shard-affinity | sloppy visited | replica tier |
|---|---|---|---|
| 同 shard 異 key 競合 | ◎ | ○ | △ |
| 同 shard 同 key 競合 (hot-key) | ✕ | ◎ (read 側) | ◎ (read 側) |
| Path A 同 key 同時 update | ✕ | ✕ | △ (replica の write coherence で別問題) |

### 4.4. 結論

「ホットキーさえ解けたら満足」が研究目標である以上、shard-affinity は **対象外**。中規模 Zipf 帯で sharding 並列性が崩れかけたときの保険札として位置付ける。

## 5. hot-key 対策の比較

senba の hot-key contention は構造的に 2 種類:

| 競合 | senba での発生源 | 既存対策 |
|---|---|---|
| **(i) reader の visited 書き込み** | hot key の visited bit を毎 read で OR → cache line bouncing | c11s で別配列化済 (ただし 64-bit word に同居する 63 key は false sharing) |
| **(ii) writer Path A (value 更新)** | hot key を全 thread が同時 update → shard Mutex で詰まる | c14s で lock-free 化、ただし HR 退行 |

実装候補と効き方:

1. **Sloppy / sampled visited update** — 1/16 など確率で visited を立てる。hot key は volume で必ず立つので HR 影響は理論的に微小、atomic write traffic が直接 16× 減。実装 10 行レベル。
2. **Per-thread visited shadow (LongAdder 流)** — thread-local bitmap に "触った" を貯め、周期的に global へ OR merge。本稿 §6 で詳細設計。
3. **Hot-key replica tier (Window-TinyLFU 的)** — Count-Min Sketch で hot top-K を検出、別の lock-free read-only replica table に複製。hot read は replica から答えて shard を踏まない。冷たい key は通常経路。Caffeine の admission window と同じ思想だが、senba では「SIEVE 上位 + hot replica」の 2-tier。
4. **Per-thread last-seen 1-slot cache** — 各 thread が直近 1〜4 hot key を thread-local に持つ。hit ならゼロ contention。極端に薄い L1 として作用。

senba の薄さを壊さない順だと **1 → 4 → 2 → 3**。

## 6. LongAdder 流 visited bitmap の詳細設計

§5 の (2) を具体化。

### 6.1. 基本形 (naive padded)

```rust
#[repr(align(64))]
struct VisitedLane(AtomicU64);

struct Shard {
    visited_lanes: [VisitedLane; N_LANES],
    // ...
}
```

**reader (visited セット、hot path)**:
```rust
let lane = thread_idx_hash() & (N_LANES - 1);
visited_lanes[lane].0.fetch_or(1u64 << pos, Relaxed);
```

**hand 進行 (eviction、cold path)**:
```rust
let merged: u64 = visited_lanes.iter().map(|l| l.0.load(Relaxed)).fold(0, |a, b| a | b);
```

**hand evict (visited bit クリア)**:
```rust
let mask = !(1u64 << pos);
for lane in &visited_lanes {
    lane.0.fetch_and(mask, Relaxed);
}
```

`fetch_and` と `fetch_or` は同じ word に対して原子的に直列化されるので **bit lost は起きない**。LongAdder/sum (load-then-add で再構成) と違い、OR の merge は monotone なので「全 lane を順に load」で安全な近似値が出る。

### 6.2. memory cost (naive 版)

| N_LANES | 競合度 (16T) | shard あたり | SHARDS=256 全体 |
|---:|---|---|---|
| 1 (現状 c11s) | 16-way | 8 B | 2 KB |
| 8 | 2-way (birthday) | 512 B | **128 KB** |
| 16 | ~1.5-way | 1 KB | 256 KB |

per_shard=64 entries × SHARDS=256 = 16K entries の cache に対して、**128 KB は重すぎ** (実態 u64 1 個に対して padding 56 B が 8 倍)。

### 6.3. 8-shard packing による 8× 圧縮

cache line が Modified 化するコストは line の中身が 1 word でも 8 word でも同じ、という観察から:

```rust
#[repr(align(64))]
struct VisitedLine {
    // 8 shard 分の visited を 1 line に packing
    shards: [AtomicU64; 8],
}

// 全体: [SHARD_GROUP][LANE] の 2 次元
//   SHARD_GROUP = SHARDS / 8
//   LANE = thread_idx & (N_LANES - 1)
struct VisitedTable {
    lines: Box<[VisitedLine]>,  // SHARD_GROUP × N_LANES
}
```

**reader**:
```rust
let lane = thread_idx & (N_LANES - 1);
let group = shard_idx / 8;
let word  = shard_idx & 7;
let line  = &lines[group * N_LANES + lane];
line.shards[word].fetch_or(1u64 << pos, Relaxed);
// ↑ 同 thread が同 group 内の他 shard を触っても同じ line に hit、L1 stay
```

**hand merge** (1 shard 分):
```rust
let group = shard_idx / 8;
let word  = shard_idx & 7;
let merged = (0..N_LANES)
    .map(|l| lines[group * N_LANES + l].shards[word].load(Relaxed))
    .fold(0u64, |a, b| a | b);
```

### 6.4. memory cost (packed 版)

| 構成 | 計算 | 合計 |
|---|---|---|
| 現状 c11s (lane=1, padding 無) | 256 × 8 B | 2 KB |
| naive padded LongAdder (N=8) | 256 × 8 × 64 B | 128 KB |
| **packed (本案、N=8)** | (256/8) × 8 × 64 B | **16 KB** |
| packed (N=16) | (256/8) × 16 × 64 B | 32 KB |

**8× 圧縮**で 128 KB → 16 KB。senba の薄さと整合する。

### 6.5. 副次効果

同 thread が連続する shard を触ったときの **spatial locality** が立つ。例えば burst で shard 5,6,7 (全部 group 0) を順に触ると、padded 設計ではそれぞれ別 cache line だけど packed 設計では同 line に 3 連続 hit。Zipf でない uniform 系 workload で薄く効く。

### 6.6. 唯一の注意点 — eviction-time merge cost

hand merge が「N_LANES 本の cache line load」になる ((group, word) は同じだが lane が違うので line が違う):

- N=8, eviction 1 回あたり: 8 line load + 8 atomic AND = 16 line touch
- eviction 頻度は steady-state で insert miss 率 (Twitter trace で 5〜30%) なので、insert あたり 0.5〜5 line touch 換算

これは現行 c11s (1 line touch / eviction) より重い。**eviction が hot な workload (churn-heavy / 低 HR 帯) では merge 側が新たな bottleneck** になる可能性がある。

mitigation:

- N_LANES を小さくする (N=4) で merge cost を半減
- thread 数とのバランスで N=4 でも contention 2-way に収まる構成 (8 thread 想定なら N=4 で OK)

### 6.7. sloppy visited との比較

| 軸 | LongAdder packed visited | sloppy visited (1/16 sample) |
|---|---|---|
| 実装複雑度 | 中 (lane 数 / hash / clear / merge path) | 低 (rng & gate 1 行) |
| memory | +14 KB (N=8 packed) | 0 |
| HR 影響 | 0 (full coverage) | ε (cold key の visited が確率的に立たない) |
| write traffic 削減 | N 倍 | sample 率倍 (16) |
| API 契約 | 内部実装 | "visited は確率的" を売る形になる |

**LongAdder 流が HR に副作用ゼロで write contention だけ消すクリーンな解**。sloppy は「cold key の visited を捨ててもいい」という HR トレードを払って同じ traffic 削減を取りに行く設計。

senba の thin SIEVE crate という性格を考えると、**API 契約を変えない LongAdder packed visited のほうが筋が良い**。ただし実装規模は半日仕事 vs 5 行なので、効きが見える前に作るのは賭けが大きい。

## 7. 進め方

```
Phase 1: sloppy visited (5 行、1 日以内)
   ↓ hot-key contention 由来の throughput 改善幅を測定、HR ロス測定
   ↓ Twitter 5 cluster + perf-gate で 6 シナリオ確認

Phase 2: packed LongAdder visited (本稿 §6、半日)
   ↓ Phase 1 で効きが見えた地点で HR ロスゼロ版に置換
   ↓ eviction-heavy workload (churn-heavy / 低 HR 帯) で merge cost regression 確認
   ↓ N_LANES sweep ({4, 8, 16}) で memory / contention / merge cost トレードオフ

Phase 3 (条件付き): shard-affinity 等の追加策
   ↓ 中規模 Zipf 帯で詰まったときの保険札として
   ↓ Caffeine 流 sieve_cb は別 lane で立てる (senba スコープ外)
```

判断基準:

- Phase 1 で sloppy が **uniform read-heavy 16T を c14s 比 1.5× 以上** 改善するなら Phase 2 に進む価値あり
- Phase 1 で HR ロスが Twitter 5 cluster で **平均 0.5pp 以上** ならスコープ外、Phase 2 直行
- Phase 2 で merge cost が churn-heavy で **5% 超 regression** なら N_LANES=4 に縮める / hand 走査自体の頻度を減らす方向

## 8. open questions

- **hot key 検出の sketch (Count-Min) を入れる場合、senba の薄さとどう折り合うか**: §5 の (3) replica tier は実装が大きく、現スコープでは見送り候補。ただし hot subset の write traffic を構造ごと逃がすので long-term は有望。
- **LongAdder packed visited で hand クリアの atomic AND を eviction 経路から外せるか**: 全 lane を逐次 AND するのは N_LANES atomic 操作になる。「epoch 化して clear をスキップ、reader は epoch チェックで stale を弾く」設計が可能か検討の余地あり。ただし complexity 増。
- **Phase 1 の sloppy 比率をどう動的に決めるか**: 静的 1/16 はベースライン、Zipf 系数や hot key 集中度で適応的に絞る案 (例: hot 上位 1% のみ sample、cold は full update) はあり得るが、検出 cost が要る。

## 9. 関連案件との接続

- `improvement-ideas.md` §G2-α (entry-level seqlock): false-miss 解消と adv-hot read-only -22% 退行回収。本稿の hot-key 対策と **直交** で、両方乗せられる。
- `improvement-ideas.md` §G3-ζ (sub-sharding): T1 を 1/K 化。本稿の packed LongAdder visited と組み合わせると、shard 内の visited contention は LongAdder で消し、shard 間の writer Mutex 競合は sub-shard で 1/K 化、という二段構成になる。
- `2026-05-10-visited-bitmap.md`: visited を tag から分離して per-shard u64 bitmap 化した実装。本稿の packed LongAdder はこの構造の自然な拡張で、`Shard.visited: u64` を `VisitedTable` に差し替える形で乗る。
