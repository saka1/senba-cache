# 2026-05-12 — c17s Step 1 (len.load 削除) + Step 2 (path_c_epoch 分離) 検証

- 種別: tuning attempt の結果 report (Step 1 採用、Step 2 reject)
- 動機: `c18s-results §9.1` で残された single-shard read 側の 2 候補 (`len.load` 削除 / `path_c_epoch` 移動) を c17s に直接適用して perf 改善を試みる
- 関連:
  - `2026-05-11-c17s-results.md` §8.1 — Step 2 の元提案 (path_c_epoch を tags 隣接 reader-only line へ)
  - `2026-05-12-c18s-results.md` §9.1, §9.3 — Step 1 推奨 / Step 2 単独に対する警告
  - `improvement-ideas.md` §D.1 (G2-α)

## TL;DR

- **Step 1 (`len.load` 削除)**: perf neutral (gate_a -1.6%, gate_b +0.9%、noise band 内)、**採用**。理由は perf ではなく、disasm 確認で 1 atomic load + 1 branch が確実に消えており、reader hot path の責務がより clean になったため (code quality)。
- **Step 2 (`path_c_epoch` を独立 64B line に分離)**: perf neutral〜微負 (gate_a -0.4%, gate_b -0.5%、低 T gim で -1〜-2%)、**reject**。c18s §9.3 の警告通り、reader が touch する line 数が +1 になるコストが、Path A `visited.fetch_or` による MESI invalidate 解放のメリットを上回る。
- 副次 learning: WSL2 単発 trial は thermal throttle で容易に ±10% を作り出す (gate_a sweep の old c17s T=16 で trial 0,1 が p99 ~430-490ns に跳ね、+15.8% の見かけ改善を産んだ)。controlled 5-trial 計測でなければ判断不能。

## 1. 動機と仮説

`c18s-results §9.1` で c17s reject 後の単体シャード read 側残存候補:
1. `len.load` 削除 (TOCTOU 検証込み)
2. t1 二重 load 削減
3. AVX-512 V5

加えて `c17s-results §8.1 #2`: `path_c_epoch` を ShardHot から reader-only line (tags 隣接) に分離。

本稿では (1) と (path_c_epoch 移動) を **直接 c17s に in-place** で適用し perf-gate AB を回す方針。design ノートを別途立てない理由: 改造規模が小、変更点が局所的、Mops AB が早期に効きを判定できるため。

### 仮説

| step | 仮説 | 期待効果 |
|---|---|---|
| 1 | `pos < len` フィルタは TOCTOU 安全に省略可能 (Path B は entries[len] 初期化後 tags[len].store(LIVE, Release) のため LIVE 観測時 init 完了。tags[pos≥len] は EMPTY pad で SIMD candidate にならない。len monotonic 増加) | gate (a/b) で perf 中立〜小幅正、code が clean に |
| 2 | `path_c_epoch` は現状 ShardHot 内で `visited` と同 line。Path A writer の `visited.fetch_or` が reader の `path_c_epoch.load` を MESI invalidate していたのを解放 | gate (b) で +1pp 程度、Path A 並行の adv-hot 高 T 帯で更に改善 |

## 2. Step 1: `len.load` 削除

### 2.1 変更

`find_get` / `find_lockfree_for_path_a` から:

```rust
// 削除:
let len = self.hot.len.load(Ordering::Acquire);
// ...
if pos < len { ... try_candidate ... }
```

を削除し、try_candidate を unconditional に呼ぶ。loop bound は `self.tags.len()` (Box の slice length、非 atomic) を使う。

### 2.2 disasm 確認

`bench_concurrent` release build (`objdump -d`) で u64 specialization の `find_get` / `find_lockfree_for_path_a` を確認:

- ✓ 関数開始の atomic load: なし (旧 `mov rax, [self+offset_of_hot_len]` の Acquire load が消失)
- ✓ loop body の `if pos < len` 分岐なし (SIMD scan → 候補 → 直接 try_candidate)
- 関数開始の `mov r9, [rsi+0x60]` / `mov r10/r11, [rsi+0x58]` は `Box<[AtomicU16]>` の fat ptr の (data_ptr, len) 非 atomic load — Step 1 前後で同じ
- Rust auto-insert の `tags.len()` 上 bounds check (候補 pos に対する `cmp rax, r8; jae <panic>`) は残る — これも Step 1 前後で同じ
- AtomicU16::load(Acquire) は x86 では `movzx` で plain load (acquire は architecture でただで満たされる)

意図通りのコード生成 (atomic load -1、branch -1)。

### 2.3 paired AB 計測

| metric | old c17s (5 trials) | Step 1 (5 trials × 2) | Δ% |
|---|---|---|---|
| gate (a) advhot read-heavy T=16 | 163.27 Mops | 160.59 Mops | **-1.6%** |
| gate (b) gim T=4 skew=1.0 | 34.31 Mops | 34.61 Mops | **+0.9%** |

両方 noise band (±2%) 内。仮説 (perf 中立〜小幅正) と整合。

### 2.4 なぜ perf が動かないか

- get_by_hash は find_get の **直前**に `path_c_epoch.load(Acquire)` を実行 → ShardHot line を L1 prefetch
- 続く find_get の `hot.len.load(Acquire)` は同 line で L1-hot、コストはほぼゼロ
- `if pos < len` 分岐は steady-state で len == cap (Path C 後) のため常に true、predictable
- → 「1 atomic load + 1 branch 削減」の cost は元から ~0

つまり code を simpler にする効果はあるが、perf 向上要素ではなかった。

## 3. Step 2: `path_c_epoch` を独立 line に分離

### 3.1 変更

ShardHot から `path_c_epoch: AtomicU64` を取り出し、独立 `#[repr(C, align(64))] struct EpochLine { path_c_epoch: AtomicU64 }` に格納。`Shard` struct で `epoch: EpochLine` を `tags` の直前に配置。`#[repr(C)]` を Shard に追加して field 順を固定。

```rust
#[repr(C, align(64))]
struct EpochLine { path_c_epoch: AtomicU64 }

#[repr(C)]
pub struct Shard<K, V> {
    epoch: EpochLine,        // 64B aligned, 独立 line
    tags: Box<[AtomicU16]>,
    entries: EntriesArena<K, V>,
    hot: ShardHot,            // path_c_epoch を含まない (= 64B, Mutex+visited+len)
    capacity: usize,
}
```

「tags 直前」を狙ったが、tags は別 heap alloc (Box) なので物理隣接は保証されない。実質「独立 64B reader-frequent line」になる。

### 3.2 paired AB 計測

| metric | old c17s | Step 1 のみ | Step 1+2 (10 trials) | Δ% (vs old) |
|---|---|---|---|---|
| gate (a) advhot T=16 | 163.27 | 160.59 | 163.17 | **-0.06%** |
| gate (b) gim T=4 | 34.31 | 34.61 | 34.15 | **-0.5%** |

thread sweep T={1,2,4,8,16} で gim skew=1.0:

| T | old | new (Step 1+2) | Δ% |
|---|---|---|---|
| 1 | 11.90 | 11.79 | -0.9% |
| 2 | 20.29 | 19.89 | -2.0% |
| 4 | 35.08 | 34.63 | -1.3% |
| 8 | 49.66 | 49.95 | +0.6% |
| 16 | 61.87 | 62.51 | +1.0% |

低 T 帯で **-1〜-2% の系統的な負傾向**。advhot T-sweep は thermal artifact で読み取り困難 (§4 参照)。

### 3.3 失敗の構造的理由

c18s-results §7.4 で確立された原則:

> 「reader が touch する field を分離すれば writer 干渉が減る」という直感は、writer が同 line を 1/N 確率でしか触らない場合は false positive

c17s 旧 layout の reader hot path:
- 1 line touch: ShardHot (path_c_epoch.load + visited.fetch_or)

c17s + Step 2 reader hot path:
- 2 line touch: EpochLine (path_c_epoch.load) + ShardHot (visited.fetch_or)

MESI 観点での想定利得 (Path A 並行時の epoch line invalidate 解放) より、reader cache footprint +1 line の常時コストが大きい。Path A の `visited.fetch_or` 自体は per-hit 1 回しか撃たれず、しかも skew=1.0 では半分以上が hit で visited bit が既に立っているため `if visited & mask == 0` の conditional set 経路 (c11s 由来) で `fetch_or` をスキップする頻度も高い。

結果: Step 2 は **c17s-results §8.1 #2 の元提案も c18s-results §9.3 の警告も両方で支持される rejection**。path_c_epoch の現位置 (ShardHot 内 visited 同居) が最適。

## 4. 副次 learning: WSL2 thermal noise

Step 2 検証の thread sweep (3 trials/cell) で advhot T=16 が **+15.8%** を示し、別途 5-trial controlled AB では -0.4% (neutral) になった矛盾を観測。

trial-level の p99 chunk_ns を見ると:
- sweep の old c17s T=16: trial 0 = 424ns, trial 1 = 496ns (p99 spike)、trial 2 = 218ns
- sweep の new c17s T=16: trial 0 = 239ns, trial 1 = 211ns, trial 2 = 211ns (全部 normal)

old 側だけ thermal throttle ヒット → 平均 146.47 Mops に押し下げ。new 側は throttle なし → 169.55 Mops。差は 全部 environment ノイズ。

**運用教訓**:
- 単発 trial や少 trial (3) の `bench_concurrent` 結果は ±10% を簡単に作る (WSL2 + thermal)
- 5-trial 以上、`p99_chunk_ns` で thermal anomaly を検出する習慣 (通常時 200-300ns、throttle 時 400ns+)
- 既に `project_wsl2_measurement_confound.md` に記録済の事実だが、本件で「sweep CSV を見て改善幻覚を見る」典型例として記録

## 5. 採否

| step | 採否 | 理由 |
|---|---|---|
| 1 (`len.load` 削除) | **採用** | disasm で atomic load -1 + branch -1 を確認。perf neutral だが code が cleaner、commit-worthy。`pos < len` フィルタを「現状 (Path B/C invariants 下) では redundant」と明示するコメントを追加 |
| 2 (path_c_epoch 分離) | **reject、revert** | perf neutral〜微負。c18s §9.3 の構造的警告 (reader cache footprint +1 line) が path_c_epoch 単独でも成立することを 1 つ追加検証 |

## 6. `improvement-ideas.md` への反映

`§D.1 G2-α` の注記を更新する想定:
- α-1 (entry 同居 = c17s 現状) **確定採用**
- α-2 (versions 別配列 = c18s) **反証済** (`c18s-results`)
- α 周辺 minor tuning:
  - `len.load` 削除 → **採用、perf neutral** (本稿)
  - path_c_epoch reader-only line 移動 → **反証済** (本稿)
- 残る単体シャード read 側候補は `c18s-results §9.1` の (2) t1 二重 load 削減、(3) AVX-512 V5

## 7. 想定 deliverable

- `research/src/experimental/sieve_c17s.rs` — Step 1 のみ適用 (Step 2 は revert 済)
- 計測 raw data: `docs/benchmark/c17s-tuning/data/{sweep_advhot,sweep_gim}_{old,new}.csv`
- 本 report
- `docs/reports/index.md` の 1 行
