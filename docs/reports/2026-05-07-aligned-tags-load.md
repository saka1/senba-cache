# `find_avx2` の SIMD load を aligned 化 — `AlignedTags` 導入

## 背景

`Shard::find_avx2` (src/shard.rs) は `tags: Vec<u16>` の上を 32B (= LANE
× 2 = 16 u16 lane) 単位で走査する:

```rust
let v = _mm256_loadu_si256(tags_ptr.add(i) as *const __m256i);
let masked = _mm256_and_si256(v, mask_v);
let cmp    = _mm256_cmpeq_epi16(masked, needle_v);
```

`Vec<u16>` の base address は glibc malloc の都合で 16B 揃え止まり。32B
intrinsic (`_mm256_load_si256`) を使うには storage を 32B 揃えにする必要が
ある — その下準備をして、実際にどれだけ速くなるかを測ったのが本実験。

## 実験プロトコル

3つの状態を比較:

| 略称 | storage | load 命令 |
|---|---|---|
| **A** | `Vec<u16>` | `_mm256_loadu_si256` (= HEAD) |
| **B** | `AlignedTags` (`Vec<TagsChunk>`, `align(32)`) | `_mm256_load_si256` |
| **C** | `AlignedTags` | `_mm256_loadu_si256` |

`AlignedTags` は `#[repr(C, align(32))] struct TagsChunk([u16; 16])` の
`Vec` を持ち、`Deref<Target = [u16]>` で平坦スライスとして見せる薄い wrapper。
スカラ経路 (`self.tags[i]`、`copy_within` 等) はゼロコストでそのまま使える。

評価指標は2系統:

1. **Criterion perf-gate** (`research/benches/sieve_cache_perf.rs`): 既存の
   3シナリオを `--save-baseline before` (= A) と比較。
2. **Twitter trace 実測**: cluster006 / cluster018 × cap{1024, 4096, 16384,
   65536} × per_shard{32, 64} × {`twitter` u64-hashed, `twitter-string`
   raw String} × 3 trials = 96 cells / 状態。

## 結果

### Criterion perf-gate

`--baseline before` 比 (median):

| シナリオ | B (load) | C (loadu) |
|---|---|---|
| insert_u64/384 | -1.5% | -3.8% |
| mixed_u64/384 | -2.5% | -3.2% |
| **insert_string/256** | **+4.5〜5.0%** | **+7.1%** |

`insert_string` が CLAUDE.md の "5%超退行は要調査" ラインに乗る。これだけで
判断すると adopt しない結論になるが、Twitter で逆方向の結果が出た。

### Twitter trace (32 cells / 状態)

| source | B / A geomean | C / A geomean |
|---|---|---|
| twitter (u64) | **−3.35%** | −2.48% |
| twitter-string | **−4.39%** | −4.07% |

cell 単位でも 32 中 27 cells で改善方向、`twitter-string cluster006/65536`
では **-13〜-16%** という大きな改善。`insert_string` の +5% はそのシナリオ
固有のヒープレイアウト依存 noise であり、実 workload の代表ではないと結論。

### B vs C の差はほぼゼロ

- perf-gate: B のが C より僅かに `insert_string` 退行が小さい (+5% vs +7%)
- Twitter: B のが C より僅かに改善幅が大きい (-3.35% vs -2.48% / -4.39%
  vs -4.07%)

差は run-to-run noise の中。**aligned intrinsic そのものに perf 効果は無い**
というのが次のディスアセンブル比較で確定する。

## ディスアセンブル: A と B/C で hot loop が完全一致

`u64` monomorph (`Shard<u64, u64, Slot32>::find_avx2`) の hot loop:

**A (`Vec<u16>` + `loadu`)**
```nasm
add  rsi, 0x10
cmp  rsi, rcx
jae  .exit
vpand    ymm2, ymm1, YMMWORD PTR [r8+rsi*2]   ; load was folded into vpand
vpcmpeqw ymm2, ymm2, ymm0
vpmovmskb r10d, ymm2
test r10d, r10d
je   .loop_top
```

**B (`AlignedTags` + `load`)**
```nasm
add  rsi, 0x10
cmp  rsi, rcx
jae  .exit
vpand    ymm2, ymm1, YMMWORD PTR [r8+rsi*2]   ; same encoding
vpcmpeqw ymm2, ymm2, ymm0
vpmovmskb r10d, ymm2
test r10d, r10d
je   .loop_top
```

**bit-for-bit 同一**。prologue/epilogue も完全一致 (`Shard` のフィールドオフセット
`[rdi+0x8]`/`[rdi+0x20]`/`[rdi+0x40]` が変わらないのは `Vec<TagsChunk>` も
`Vec<u16>` も sizeof = 24B (ptr+cap+len) のため)。`String` monomorph も同様。

LLVM はどちらの intrinsic でも `_mm256_loadu/load_si256` → `vpand ymm, ymm,
m256` (memory operand) に **fold** する。VEX-encoded AVX2 の memory operand は
構造的にアライン要求が無いため、両者で同一エンコードが出る。

## それでもなぜ +3〜4% 速くなるのか — cache-line split

VEX 命令の memory operand 自体に「32B 揃え」のペナルティ判定は無い。実際に
効くのは **64B キャッシュラインを跨ぐかどうか** (split load の +1〜数 cycle)。

glibc malloc は 16B 揃えなので `tags` の base address mod 64 は確率的に
{0, 16, 32, 48} のどれか:

| base mod 64 | 32B chunk のオフセット系列 (mod 64) | split 頻度 |
|---|---|---|
| 0 | 0, 32, 0, 32, ... | 0/2 |
| 16 | 16, 48, 16, 48, ... | **1/2** |
| 32 | 32, 0, 32, 0, ... | 0/2 |
| 48 | 48, 16, 48, 16, ... | **1/2** |

`#[repr(align(32))]` を付けると base mod 64 ∈ {0, 32} に絞られ、split 頻度は
常に 0/2。確率的に半分のアロケーションで起きていた split が消える、これが +3〜4%
の正体。`vpand ymm, ymm, m256` が 6〜7 cycle の load-bound パスを構成しており、
split 頻度 1/2 × 1 cycle が 3〜4% の改善幅と整合する。

## 採否判断

採用する。理由:

- **B (`AlignedTags` + `load_si256`) を採る**。命令選択そのものに今は perf
  効果が無いが、将来 LLVM が memory operand fold をやめて明示的な `vmovdq{u,a}`
  を出すようになったら aligned 側が fast path に乗る、という保険。`debug_assert!`
  で alignment invariant をコードに刻める副次効果もある。
- 実 workload の改善 (Twitter で -3〜-4% geomean、安定して同方向) は十分。
  criterion `insert_string` の +5% は同シナリオ固有のヒープレイアウト依存で、
  perf-gate のラインを跨ぐが investigation 結果として原因と無害性を documented。

## 教訓

- **Aligned intrinsic vs unaligned intrinsic は LLVM の memory-operand fold の
  下では codegen 上等価**。perf 差を期待するなら storage 側を揃えて
  cache-line split を構造的に消す方が本質的。
- **perf-gate の 1 シナリオが退行しても実 workload で改善するケースがある**。
  criterion micro-bench は ヒープレイアウト依存の noise を拾うので、Twitter
  trace のような実 workload で cross-check すると判断材料が立体的になる。
- **glibc malloc の 16B alignment が暗黙の前提として効く** こともある (今回の
  ように 32B 揃え load の split 頻度を 50% 引き起こす)。アロケータ依存性を
  理解した上で `repr(align(N))` で structural に締めるのが安全。

## 参照

- 生データ: `docs/reports/data/2026-05-07-aligned-tags-load/{aligned_A,aligned_B,aligned_C}.csv`
- 集計スクリプト: 同 `data/analyze_aligned.py`
- Intel Optimization Reference Manual §2 / §15 (split load penalty)
- Agner Fog "The microarchitecture of Intel, AMD and VIA CPUs" (per-uarch split table)
