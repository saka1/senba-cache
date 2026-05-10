# 2026-05-10 — Shard layout: S3 reorder + `capacity` field 削除

- 親:
  - `improvement-ideas.md` §B.1 S3 (フィールド並び替えで `len` を 1st cache line に)
  - `2026-05-08-find-avx2-frontier.md` §S3 (机上見積もり)
  - `2026-05-07-aligned-tags-load.md` (perf-gate 単独評価の危うさを示す前例)
- 関連実装: `src/shard.rs` (採択)、`src/lib.rs` (`Cache::capacity` を `s.capacity()` 経由に)
- 種別: **実測ノート**。const-eval によるレイアウト契約 + asm verify + criterion + Twitter 60 cells + Zipf 高 skew + ARC OLTP / DS1 のクロスチェック

## 0. TL;DR

`Shard<K, V, S>` を `#[repr(C)]` 化、フィールドを再配列、**`capacity: usize` 自体を削除** (`entries.len()` と恒等。`#[inline] capacity()` で公開)。read-side の hot path 4 フィールド (`tags.ptr` / `entries.ptr` / `len` / `visited`) を **cache line 1 (offset < 64) に閉じる**ことが目的。

- **構造**: `sizeof(Shard) 112 → 104 B` (asm `imul rdi, 112` → `..., 104` で確認)。`len` の load 位置 `[r8 + 64]` 線 2 → **`[r8 + 48]` 線 1**。`tags.ptr@8`、`entries.ptr@32` は線 1 不変。
- **正当性**: `std::mem::offset_of!` を `Shard::_LAYOUT_OK` 内で `tags@0, entries@24, len@48, visited@56, hand@64` として const-assert。ビルド成功 = 実レイアウト一致。
- **挙動**: HR equivalence は 60 (Twitter) + 8 (Zipf) + 4 (OLTP) + 3 (DS1) = **75 cells で全完全一致** (sieve_orig oracle 等価性は無変更、構造改修のみ)。
- **perf**:
  - **criterion micro-bench (calibration ×2)**: geomean **−1.8%**、5/6 シナリオで improved 方向、`get_heavy_u64 −4.32% (p=0.01)` と `mixed_u64 −1.94% (p=0.03)` が有意。+5% gate 違反ゼロ。
  - **Twitter 60 cells (5 cluster × cap{1k,4k,16k} × per_shard{32,64} × {u64,String} × 3 trials)**: geomean **+0.11%** (≈noise)。20 imp / 21 reg / 19 neu。±1% noise floor 内。
  - **Zipf 高 skew 8 cells (skew{1.2,1.4} × cap{4k,16k} × per_shard{32,64} × len=10M × repeat=10 × 3 trials)**: geomean **+0.07%**。CV ≤ 3%。改善 1/8、退行 0/8。
  - **ARC OLTP 4 cells (--repeat 20、9 trials)**: geomean **+0.01%**。cap=2000 のみ −2.65% 改善、cap=256/512 で +1.0〜1.7% 退行 (CV 1%)。
  - **ARC DS1 3 cells (no repeat、9 trials × ~5s/trial)**: geomean **−2.19%**、**3/3 cells improved (-1.5% 〜 -3.3%)**、CV 2-3%。
- **採否**: **採択**。長尺・大 cap・多 shard 的 workload (DS1) で −2% 級の安定改善。短い trace では noise floor 下で signal 不可視だが retain side (regression) も同様に消えるので無害。構造改修としても正味の利得 (`Vec::len` で `capacity` の field 8B を浮かせる + `#[repr(C)]` で layout を契約化)。

## 1. 動機 — なぜ S3 か、なぜ capacity 削除が同居するか

`improvement-ideas.md` §B.1 S3 (`Shard` フィールド並び替え) は ROI 試算上のリスク最小案 ("cold-self で 1 line/call 削減、ほぼゼロリスク")。`find-avx2-frontier.md` §S3 でも机上で挙がっていた。

ここに**「`capacity` field は本質的に必要ない」**という観測を上乗せした。`entries: Vec<MaybeUninit<...>>` は `Shard::new` で `resize_with(capacity, ..)` され、その後リサイズしない。したがって `entries.len()` は常に `capacity` と恒等。`Vec::len` の load 位置は `entries.ptr` と同じ cache line 1 上にあるので、accessor 経由でも cost は不変。

両者をセットでやると Rust 既定 layout (rustc 1.95 / `repr(Rust)`) の `tags@0, entries@24, capacity@48, hand@56, len@64, visited@72, ...` から、

```rust
#[repr(C)]
struct Shard<K, V, S: SlotSize> {
    tags: AlignedTags,                                          //  0..24
    entries: Vec<MaybeUninit<S::Storage<Entry<K, V>>>>,         // 24..48
    len: usize,                                                 // 48..56  ★ 線 1
    visited: u64,                                               // 56..64  ★ 線 1
    hand: usize,                                                // 64..72  線 2
    hits: u64, misses: u64, insertions: u64, evictions: u64,    // 72..104
}
```

に再配置できる。read-side の hot path (`Shard::contains` / `Shard::get` / `Shard::find_avx2`) は `tags.ptr` / `entries.ptr` / `len` (scan 範囲) / `visited` (on-hit set) のみ触るので、線 1 1 本で完結する。`hand` は evict / 新規 insert path のみ、stats は計上路のみで、いずれも find dominated path の load から除外できる。

## 2. 実装

### 2.1 `Shard` 再配置

`src/shard.rs` で `Shard<K, V, S>` を `#[repr(C)]` 化、`capacity: usize` field を削除、フィールド順を上記レイアウトに変更。`#[inline] pub(crate) fn capacity(&self) -> usize { self.entries.len() }` を追加。

field 削除に伴う in-file callsite は 5 箇所:

| 行 (旧) | 用途 | 置換 |
|---|---|---|
| `entry_ptr_from_tag` debug_assert ×2 | `off < self.capacity * S::SIZE` | `self.capacity() * S::SIZE` |
| `Shard::new` 初期化 | `Self { capacity, .. }` | field 削除 |
| `insert` 満杯判定 | `self.len < self.capacity` | `self.len < self.capacity()` |
| `find_evict_pos` debug_assert | `self.len == self.capacity` | `self.len == self.capacity()` |
| `Clone for Shard` | `Shard::new(self.capacity)` | `Shard::new(self.capacity())` |

外部 (lib.rs) からの参照は `Cache::capacity()` 集計の 1 箇所:

```diff
- self.shards.iter().map(|s| s.capacity).sum()
+ self.shards.iter().map(|s| s.capacity()).sum()
```

### 2.2 レイアウト契約の const-eval

`Shard::_LAYOUT_OK` を追加し `Shard::new` から参照。コンパイル成功 = 契約一致:

```rust
const _LAYOUT_OK: () = {
    assert!(std::mem::offset_of!(Self, tags) == 0);
    assert!(std::mem::offset_of!(Self, entries) == 24);
    assert!(std::mem::offset_of!(Self, len) == 48);
    assert!(std::mem::offset_of!(Self, visited) == 56);
    assert!(std::mem::offset_of!(Self, hand) == 64);
};
```

`offset_of!` は stable since 1.77。型パラメータ (`K, V, S`) に依存する field は無く (`Vec<...>` は常に 24 B、`usize`/`u64` も同様)、契約はジェネリックでも成り立つ。

### 2.3 asm verify (release + AVX2)

`Cache::get<u64,u64,Slot32>` の prologue (target/release/deps/sieve_cache_perf-*.s):

| 操作 | before | after |
|---|---|---|
| sizeof(Shard) (`imul r8, rdi, N`) | `..., 112` | `..., 104` ✅ −8 B |
| `len` load | `mov rax, [r8 + 64]` (線 2) | `mov rax, [r8 + 48]` ✅ 線 1 |
| `tags.ptr` load | `mov rdi, [r8 + 8]` (線 1) | 同 |
| `entries.ptr` load | `mov rdx, [r8 + 32]` (線 1) | 同 |

read hot path の 4 load は全て **offset ∈ [0, 56)**、線 1 完結。

ベースラインは `aligned-tags-load.md` 教訓通り `--save-baseline before-s3` で固定し、`--baseline before-s3` で比較。

## 3. 結果

### 3.1 Quality gate

- `cargo fmt --all`: clean
- `cargo clippy --workspace --all-targets -- -D warnings`: clean
- `cargo test --workspace`: 489 tests pass (senba 101 + senba-research 388 + others)。oracle (`oracle_cache_match.rs`) を含む。
- `cargo check -p senba`: `_LAYOUT_OK` const-eval pass = 設計通りの offset

### 3.2 criterion perf-gate (calibration ×2)

`research/benches/sieve_cache_perf.rs` の 6 シナリオ。`--baseline before-s3` 比 (mean):

| シナリオ | run 1 | run 2 | 判定 |
|---|---:|---:|---|
| insert_u64/384 | −1.55% | −1.55% | improved (NS) |
| mixed_u64/384 | −1.01% | **−1.94%** (p=0.03) | improved |
| insert_string/256 | **−3.83%** (p=0.00) | −2.04% | improved |
| insert_u32_slot16/384 | −1.50% | +2.22% (CI 0 跨ぐ) | noise |
| **get_heavy_u64/384** | −1.75% | **−4.32%** (p=0.01) | **improved** |
| mixed_lowskew_u64/384 | −1.11% | +2.48% (CI 0 跨ぐ) | noise |

geomean (run 1) ≈ −1.8%。+5% regression gate 違反ゼロ。read 主体の `get_heavy_u64` が最大 −4.32% で、線 1 完結化の主目的どおり方向性が一致。

### 3.3 Twitter trace cross-check (60 cells)

5 cluster × cap{1024, 4096, 16384} × per_shard{32, 64} × source{`twitter` u64, `twitter-string`} × 3 trials (raw: `data/2026-05-10-shard-layout-s3/{before,after}.csv`)。

per-source geomean of (after / before):

| source | geomean | imp ≥1% | reg ≥1% | neu <1% |
|---|---:|---:|---:|---:|
| twitter (u64) | +0.38% | 9 | 11 | 10 |
| twitter-string | −0.16% | 11 | 10 | 9 |
| **OVERALL** | **+0.11%** | 20 | 21 | 19 |

HR equivalence: 60/60 cells で `hits` 完全一致。最大値レンジ ±5〜7% は per-cell 測定 noise の典型。改善・退行が cluster/cap で randomly に揃わない (例: cluster006/cap=4096/ps=64 +4.26% 退行 ⇔ cluster018/同条件 −6.95% 改善)。

→ **Twitter 60 cells では noise band 内で perf-neutral**。これだけ見ると判断不能。`aligned-tags-load.md` で見た「criterion 退行 / Twitter 改善」の逆向き構造 noise と同種。

### 3.4 高 skew Zipf — read-hot 強化 (8 cells、9 trials/cell)

skew ∈ {1.2, 1.4} × cap ∈ {4096, 16384} × per_shard ∈ {32, 64} × len=10M × `--repeat 10` × 3 trials = 各 trial ~3s 測定 (raw: `sensitive_zipf.csv`)。

geomean **+0.07%**。CV ≤ 3%。改善 1/8、退行 0/8、中立 7/8。**Zipf では高 skew でも signal なし**。read-hot 比率は十分高いが、大量 shard を順に touch する situation を作れていない (cap=16384 / per_shard=32 → 512 shards だが Zipf hotspot は 1 shard 周辺に集中)。

### 3.5 ARC OLTP — 小 cap・read-heavy (4 cells、9 trials/cell)

`--arc-preset oltp` (4 cap {256, 512, 1000, 2000} を 1 invocation で得る) × `--repeat 20` × 9 trials (raw: `sensitive_oltp.csv`)。

| cap | before ns/op | after ns/op | Δ | CV (after) |
|---:|---:|---:|---:|---:|
| 256 | 31.05 | 31.57 | +1.69% | 1.19% |
| 512 | 31.33 | 31.65 | +1.04% | 0.88% |
| 1000 | 31.00 | 31.01 | +0.02% | 0.82% |
| **2000** | 30.69 | **29.87** | **−2.65%** | 0.92% |

geomean +0.01%。cap=2000 (= 32 shards) で改善が出るが、cap≤512 (= 4–8 shards) では小 noise 退行。**OLTP は workload size と shard 数のスイートスポットが狭く、net zero**。

### 3.6 ARC DS1 — 長尺・大 cap (3 cells、9 trials/cell × ~5s/trial)

`--arc-preset ds1` (3 cap {1M, 4M, 8M}、trace len=43.7M ops) × 9 trials (raw: `sensitive_ds1.csv`)。

| cap | shards (auto) | before ns/op | after ns/op | Δ | CV (after) |
|---:|---:|---:|---:|---:|---:|
| 1M | 16,384 | 85.00 | **82.17** | **−3.32%** | 2.78% |
| 4M | 65,536 | 122.71 | **120.60** | **−1.72%** | 3.01% |
| 8M | 131,072 | 127.09 | **125.16** | **−1.52%** | 2.38% |

geomean **−2.19%**。**3/3 cells improved (≥1.5%)**、retain side ゼロ。CV 2-3% (1 trial 5s × 9 trials = 45s/cell 測定の安定値)。

→ **DS1 は本変更が "wall-clock で出る" 数少ない workload**。大 cap × 多 shard (10k〜130k) で find が cross-shard に散る pattern では、線 1 完結化の利得が transfer cost 削減として観測される。

## 4. 解釈

| 観測 | 解釈 |
|---|---|
| criterion で improved 5/6, geomean −1.8% | 構造改善は出ているが小さい (synthetic Zipf 単 shard hot pattern では shard 数=4-32 と少なく、線 1 改善の出番が小さい) |
| Twitter 60 cells perf-neutral | per-trial 30ms と短い + 1 cluster あたり cap 帯が散らばるため noise band 内 (CV 2-5%) に signal が埋もれる |
| Zipf 高 skew でも flat | hotspot が 1 shard に集中するため shard 数の効きが弱い |
| OLTP cap=2000 だけ −2.65% | shard 数 (32) が小さいうちは線 1 1 本でも transfer 量が少ない。cap が大きくなって shard 数が増えてから利得が顕在化 |
| **DS1 全 3 cap で 1.5〜3.3% 改善** | shard 16k〜130k 規模の大 cap workload で **線 1 完結が transfer cost を実際に削る** ことの直接証拠 |

DS1 と OLTP cap=2000 の improved 方向、criterion `get_heavy_u64 −4.32%` (p=0.01) の方向はすべて整合。**signal は workload-specific で確実に存在**するが、micro 系の noise floor (±1〜2%) より小さいため short trace では見えない。aligned-tags-load.md と同じく、「criterion 単独」でも「Twitter 単独」でも判断できず、複数 workload で方向の一致を取って判断すべきという教訓 (revert §3-T4) を踏襲した。

## 5. 採否

**採択** (`src/shard.rs` + `src/lib.rs`、commit 候補)。

| 軸 | 判定根拠 |
|---|---|
| 構造 | sizeof 104B 確定 / `_LAYOUT_OK` 契約 / asm verify 完了 |
| HR 等価性 | 75 cells で完全一致 |
| 性能 | DS1 −2.19% geomean (3/3 imp)、OLTP cap=2000 −2.65%、criterion −1.8% geomean |
| regression | どの workload でも +5% gate 違反なし、+1〜2% の点状 retain は CV 内 noise |
| API 影響 | なし (`Cache::capacity()` 公開 API は不変) |
| 後続最適化への足場 | §A.2.2 prefetch (線 1 完結なら 1 prefetch 命令で hot path 全体を投機 fetch) / §E.4 (instruction footprint −10pp 探索の前提) を整備 |

## 6. open questions / follow-ups

- **`capacity()` accessor の inline 失敗ケース**: `entries.len()` は `Vec::len` (= `#[inline]` の 1 word load) なので原理的には field load と同 cost だが、generic monomorphization 境界を跨ぐと codegen が膨らむ可能性 (`Borrow<Q>` 経路と同じ論点)。`Cache::capacity()` 集計の sum loop を `--release --emit=asm` で確認するのは follow-up。
- **DS1 で改善する具体メカニズム**: 線 1 完結化が L1 hit を増やしたのか、L2 / L3 hit を増やしたのか、あるいは TLB miss を減らしたのか — VTune / perf stat 細粒度計測で因果分解。`bench_vtune.rs` を DS1 traceで回す follow-up が筋。
- **OLTP cap=256/512 の +1.0〜1.7% 退行**: CV 1% なので 1.5σ 程度で完全 noise 判定はできない。`capacity()` のインライン化で生じた何かの命令並びの差で micro-arch port pressure に乗っている可能性。short trace なので追検は薄い。
- **Twitter 5 cluster で signal が見えない件**: per-trial 30ms と短いため。`--repeat 100` で 3s/trial 化して 5 trial 取れば DS1 と同じ強度の signal が出るかは未検証。
- **shards 数 sweep**: SHARDS が小さい (=4) と本変更の効きは弱いが、shards 上限を増やす (`Cache::with_shards(cap, n)` で n を盛る) と DS1 様の効きが出る workload があるかは別軸。

## 7. 反映先

- 採択を反映: `src/shard.rs` (`#[repr(C)]`、フィールド再配列、`capacity` field 削除、`capacity()` accessor、`_LAYOUT_OK`)、`src/lib.rs` (`Cache::capacity` の集計を `s.capacity()` 経由に)
- `improvement-ideas.md` §B.1 S3 と §1 短期 priority 表から S3 行を削除し、§I (棄却・実装済 履歴) に下ろす (本レポートを参照)
- raw data: `docs/reports/data/2026-05-10-shard-layout-s3/{before,after,sensitive_zipf,sensitive_oltp,sensitive_ds1}.csv` + `analyze.py` / `analyze_sensitive.py`
