# 2026-05-08 — `find_avx2` caller-merge 最適化探索 (S1+S2 棄却 → A3+`#[inline]` で期待 asm 達成 → NonZeroU16 で sret 解消、強い perf-gate 利得)

- 親:
  - `2026-05-08-find-avx2-frontier.md` (S1/S2/A3 設計 + 机上見積もり)
  - `2026-05-07-aligned-tags-load.md` (perf-gate 単独評価の危うさを示す前例)
- 関連実装: `src/shard.rs` (採択、commit 済み)。Twitter trace
  cross-check は後続作業で実施
- 種別: **実測ノート**。3 段階の試行 (S1+S2 棄却 → A3+`#[inline]` で
  期待 asm 部分達成 → NonZeroU16 + A3 で sret も解消) を経て、
  OQ-1 (NonZeroU16 niche で 16 byte 維持) が想定以上に効いたところまで。

## 0. TL;DR

frontier.md の予測 (S1+S2 で hit path **−5〜−7 cy/hit**) を 3 段階で詰めた:

1. **第 1 試行 (S1+S2)**: 棄却。`find_avx2` は `target_feature` 制約で
   inline されず tuple 返り (24 byte) が **sret 化**、LLVM は shift
   round-trip を畳まず、perf-gate で get_heavy_u64 +5.11% (gate 越え)
2. **第 2 試行 (A3+`#[inline]`)**: 期待 asm の主要部分 (shift round-trip
   消滅) は達成。`entry_ptr_from_tag(tag) = entries + (tag & ID_MASK)`
   ヘルパで source 側 fold。get_heavy +1.51% (gate 内)、insert/mixed 系
   −3.83〜−8.32%。ただし sret は stable Rust の構造制約で残存
3. **第 3 試行 (NonZeroU16 + A3 + `#[inline]`、本稿で新規)**: **完全達成**。
   `find` 返り型を `Option<(usize, NonZeroU16)>` に。LIVE bit 立ち = 非ゼロ
   なので niche optimization が発動、`Option` discriminant byte が消えて
   **size = 16 byte** に収まり sret 解消。**`Cache::get` AVX2 hit branch
   全行が予測通りの asm**:

   ```asm
   call   find_avx2          ; rdi = &shard, NOT sret slot
   test   dx, dx              ; niche: dx == 0 が None marker
   je     .miss
   inc    qword ptr [rbx + 72]            ; hits++
   or     word ptr [rcx + 2*rax], 16384   ; tags[pos] |= VISITED
   and    edx, 2016                       ; ★ tag & ID_MASK = byte offset (1 op)
   mov    rax, [rbx + 32]                 ; entries ptr
   add    rax, rdx
   add    rax, 8                          ; → &value
   ```

   perf-gate AB (vs before):
   - **insert_u64 −7.15% / mixed_u64 −10.14% / insert_string −3.22% /
     insert_u32_slot16 −9.24%** で 4 シナリオ大勝
   - get_heavy_u64 +0.86% (ほぼ noise、5% gate 内)
   - mixed_lowskew_u64 +1.38% (gate 内、A3 と同水準)

**採択** (`src/shard.rs` に反映、5% gate 違反なし)。**Twitter trace
cross-check も完了 (§5.2)**: cluster016 で −2.82〜−9.25%、cluster018/019
は gate 内の +0.42〜+1.63% で perf-gate の get_heavy/lowskew 退行と
整合的、HR 9 セル全完全一致。perf-gate と方向が大きく揃っており採択
維持。

## 1. 計測条件

- HW: 本 worktree の開発機 (詳細は perf-gate 既存レポートと同条件)
- bench: `research/benches/sieve_cache_perf.rs` の 6 シナリオ
- 計測フロー:
  1. `--save-baseline before` (HEAD = `84517e4`)
  2. **第 1 試行 S1+S2**: `find` を `Option<(usize, u16)>` に + `entry_ptr`
     を bounds-check 廃止 → AB
  3. 第 1 試行 + `#[inline]` を `find_avx2` に追加した版でも AB
  4. **第 2 試行 A3+`#[inline]`**: S2 撤回、`entry_ptr_from_tag(tag)`
     新ヘルパで hit path を id 不経由 → AB
  5. **第 3 試行 NonZeroU16 + A3 + `#[inline]`**: `find` を
     `Option<(usize, NonZeroU16)>` に変更 (LIVE bit ⟹ NonZero)、
     `entry_ptr_from_tag` を NonZeroU16 受けに更新 → AB
- asm: `RUSTFLAGS="--emit=asm -C llvm-args=-x86-asm-syntax=intel"
  cargo rustc -p senba-research --release --bench sieve_cache_perf` で
  各段階の `Cache::get<Slot32, u64, u64>` 周辺を採取・比較
- oracle: `cargo test --workspace` で全合格 (`oracle_cache_match.rs` 含む)。
  4 段階どこでも eviction sequence は不変

## 2. 結果まとめ

| scenario | S1 alone | S1+S2 | S1+S2+`#[inline]` | A3+`#[inline]` | **NonZeroU16+A3+`#[inline]`** |
|---|---:|---:|---:|---:|---:|
| insert_u64/384 | **−3.59%** | +0.76% | **−7.24%** | **−5.93%** | **−7.15%** |
| mixed_u64/384 | **−7.19%** | −2.66% | **−8.85%** | **−8.32%** | **−10.14%** |
| insert_string/256 | +2.04% | +1.70% | +0.86% | −0.28% (noise) | **−3.22%** |
| insert_u32_slot16/384 | **−4.77%** | **−6.35%** | **−6.53%** | **−3.83%** | **−9.24%** |
| **get_heavy_u64/384** | **+1.88%** | **+3.17%** | **+5.11%** | **+1.51%** | **+0.86%** |
| mixed_lowskew_u64/384 | +3.10% | +2.10% | +3.15% | +1.40% | +1.38% |

太字: 有意 (p < 0.05) かつ |Δ| > 1%。"(noise)": criterion が "No change"
判定。

第 3 試行のハイライト:

- 4 シナリオで有意改善 (insert/mixed 系 −3.22 〜 −10.14%)
- get_heavy_u64 が +0.86% (A3 単独の +1.51% から更に縮小、ほぼ noise 域)
- 5% gate 違反なし

## 3. asm 比較

### 3.1 BEFORE: register 返り、shift round-trip 残存

```asm
call   find_avx2
cmp    rax, 1                          ; rax = discriminant (register)
je     .hit
.hit:
  inc   qword ptr [rbx + 72]           ; hits++
  movzx edi, word ptr [rcx + 2*rdx]    ; ★ tag を tags[pos] から再 load
  shr   edi, 5                         ; >> 5
  and   edi, 63                        ; & 63 = id  (round-trip 1)
  cmp   r8, rdi                        ; entries[id] bounds check
  jbe   panic
  shl   edi, 5                         ; << 5  (round-trip 2)
```

### 3.2 第 1 試行 S1+S2: sret 化、shift round-trip 残存

`Option<(usize, u16)>` = 24 byte で sret 化:

```asm
lea    rdi, [rsp + 320]                ; ★ sret 出力先
call   find_avx2
cmp    dword ptr [rsp], 1              ; ★ stack から discriminant
mov    rdi, qword ptr [rsp + 8]        ; ★ stack から pos
movzx  eax, word ptr [rsp + 16]        ; ★ stack から tag
.hit:
  inc   qword ptr [rbx + 72]
  or    word ptr [rcx + 2*rdi], 16384
  shr   eax, 5                         ; ★ shift round-trip 残存 (S2 fold 失敗)
  and   eax, 63
  shl   eax, 5
  ...
```

### 3.3 第 2 試行 A3+`#[inline]`: 期待 asm の半分達成

`entry_ptr_from_tag` で source 側 fold。**byte offset は 1 op**:

```asm
lea    rdi, [rsp + 320]                ; ★ sret は残る
call   find_avx2
cmp    dword ptr [rsp], 1
mov    rdi, qword ptr [rsp + 8]        ; pos
movzx  r9d, word ptr [rsp + 16]        ; tag
.hit:
  inc   qword ptr [rbx + 72]
  or    word ptr [rax + 2*rdi], 16384
  and   r9d, 2016                      ; ★★★ shift round-trip 消滅 (1 op)
  add   rax, r9
  add   rax, 8
```

### 3.4 第 3 試行 NonZeroU16+A3+`#[inline]`: 完全達成

`Option<(usize, NonZeroU16)>` = **16 byte** (niche)、レジスタ返り:

```asm
mov    rdi, rbx                        ; rdi = &shard (NOT sret!)
mov    rsi, r14                        ; key
mov    edx, ecx                        ; needle
call   find_avx2                       ; rax = pos, rdx = NonZeroU16 tag
test   dx, dx                          ; ★★★ niche: dx == 0 が None marker
je     .miss
.hit:                                  ; LBB170_12
  inc   qword ptr [rbx + 72]           ; hits++
  cmp   rax, rsi                       ; pos < cap (tags[] bounds)
  jae   panic
  or    word ptr [rcx + 2*rax], 16384  ; tags[pos] |= VISITED
  and   edx, 2016                      ; ★★★ tag & ID_MASK (1 op)
  mov   rax, [rbx + 32]                ; entries ptr
  add   rax, rdx                       ; +offset
  add   rax, 8                         ; +offsetof(value)
```

3 つすべて達成:

- ✅ **sret 解消**: `mov rdi, rbx` で rdi が shard pointer に戻る、stack
  buffer 確保なし。返り値は `rax` (pos) + `rdx` (tag) のレジスタ
- ✅ **niche optimization 発動**: discriminant 別バイト不要、`test dx, dx`
  だけで Option の None/Some を判別
- ✅ **shift round-trip 消滅**: `and edx, 2016` 1 op で byte offset
  (A3 から継続)

## 4. NonZeroU16 が効いた仕組み

`Option<T>` のサイズは:

| inner T | T size | Option<T> size | 備考 |
|---|---:|---:|---|
| `usize` (8B) | 8 | 16 | disc 1B + pad 7B + usize 8B |
| `(usize, u16)` (16B) | 16 | **24** | disc 1B + pad 7B + tuple 16B → **sret threshold 超え** |
| `(usize, NonZeroU16)` (16B) | 16 | **16** | NonZeroU16 の `0` パターンを None に転用 (niche) → discriminant byte 消滅 |

NonZeroU16 のゼロ値 (= 通常の `u16` の 0) は型の不変条件として禁止。
`Option<NonZeroU16>` の None は「内部値 = 0」のビットパターンで表現される
(layout 上の追加 byte 不要)。タプル `(usize, NonZeroU16)` を Option で
包んだ場合も同じ niche が使え、Option<T> のサイズが T と同じ 16B に
収まる。

senba の場合、`tag` は live なら必ず LIVE = 0x8000 が立っているので、
`tag != 0` は型不変条件として保証できる。これが NonZeroU16 を使える
直接の根拠。`find_avx2` / `find_scalar` の return 直前で
`NonZeroU16::new_unchecked(tag)` を呼ぶ:

```rust
// find_scalar
if (t & Self::SCAN_MASK) == needle {
    let id = Self::id_of(t);
    let e = unsafe { &*self.entry_ptr(id) };
    if e.key.borrow() == key {
        // SAFETY: scan match implies LIVE bit set, so t != 0.
        return Some((i, unsafe { NonZeroU16::new_unchecked(t) }));
    }
}
```

caller 側は `let (pos, tag) = find(...)?;` で受けて
`entry_ptr_from_tag(tag)` (NonZeroU16 受け) を呼ぶ。`tag.get() & ID_MASK`
が byte offset。

const_assert で sizeof 16 を anchor:

```rust
const _FIND_RET_FITS_REGISTERS: () = assert!(
    std::mem::size_of::<Option<(usize, NonZeroU16)>>() == 16,
    "find return type must fit in 2 registers (16 byte) to avoid sret"
);
```

将来 layout 計算が変わって sret 化したら build 時に検出できる。

## 5. 採否判定: 採択 (Twitter trace cross-check 済み)

第 3 試行 (NonZeroU16 + A3 + `#[inline]`) を採択して `src/shard.rs` に
commit。perf-gate 数字:

- 利得: insert_u64 −7.15% / mixed_u64 −10.14% / insert_string −3.22% /
  insert_u32_slot16 −9.24% (4 シナリオ)
- 微退行: get_heavy_u64 +0.86% / mixed_lowskew_u64 +1.38% (いずれも
  5% gate 内、A3 単独からも改善)

5% gate 違反なし、4 シナリオで二桁近い改善という Pareto から採択を判断。
memory `perf-gate には多様な workload が必要` に従う Twitter trace
cluster016 / 018 / 019 の AB は §5.2 で実施済み (HR 完全一致、最大改善
−9.25%、最大退行 +1.63%)。perf-gate と Twitter で方向が揃ったため
commit 維持。

採択した変更点:

- `src/shard.rs` に const assert
  `_FIND_RET_FITS_REGISTERS: size_of::<Option<(usize, NonZeroU16)>>() == 16`
  を追加 (将来の layout 変更で sret 化したら build error)
- `find` / `find_scalar` / `find_avx2` の return 型を
  `Option<(usize, NonZeroU16)>` に変更
- `find_avx2` に `#[inline]` 追加 (LLVM 任意判断、強制ではない)
- `entry_ptr_from_tag(tag: NonZeroU16)` /
  `entry_ptr_mut_from_tag(tag: NonZeroU16)` 新ヘルパ追加
- `get` / `get_mut` / `peek` / `peek_mut` / `get_key_value` /
  `peek_key_value` / `get_or_insert_with` (hit branch) /
  `insert` (replace branch) を新ヘルパに切替、`id_of(tags[pos])` →
  `entry_ptr(id)` の 2 段経由を廃止
- `remove` 等 cold path は `entry_ptr(id)` を温存 (hot path のみ A3)

## 5.2 Twitter trace cross-check (cluster016 / 018 / 019)

CLAUDE.md memory `perf-gate には多様な workload が必要` に従って
3 cluster × 3 cap (4096 / 16384 / 32768) で AB を取った。HEAD (採択版
`bb7ca9e`) と parent (`84517e4`) の release `bench` バイナリを別々に
ビルドし、`research/src/bin/bench.rs --source twitter --variant senba_n{N}`
で同一 trace を 3 run ずつ実行した median 比較:

| cluster | cap | parent (ms) | head (ms) | Δ |
|---|---:|---:|---:|---:|
| cluster016 | 4096 | 27.05 | 26.29 | **−2.82%** |
| cluster016 | 16384 | 23.88 | 21.67 | **−9.25%** |
| cluster016 | 32768 | 22.01 | 20.93 | **−4.93%** |
| cluster018 | 4096 | 23.01 | 23.13 | +0.53% |
| cluster018 | 16384 | 20.22 | 20.55 | +1.62% |
| cluster018 | 32768 | 19.42 | 19.69 | +1.41% |
| cluster019 | 4096 | 27.02 | 26.63 | −1.45% |
| cluster019 | 16384 | 28.73 | 28.85 | +0.42% (noise) |
| cluster019 | 32768 | 29.10 | 29.57 | +1.63% |

per_shard は 16 固定 (cap=4096→shards=256、cap=16384→shards=1024、
cap=32768→shards=2048)、過去 sweep の sweet spot に合わせた。HR
(hits / misses / evictions) は **9 セル全てで HEAD=parent 完全一致**。
oracle 等価性は保たれている。

判定:

- **5% gate 違反なし** (最大退行 +1.63%)
- **cluster016 で −2.82〜−9.25% の大勝**: scan-heavy cluster で find_avx2
  hit path 支配下、本変更が一番効くケース。perf-gate の mixed_u64 −10.14%
  と同水準で整合
- cluster018 / cluster019 は gate 内の小退行 (+0.4〜+1.6%): perf-gate の
  get_heavy_u64 +0.86% / mixed_lowskew_u64 +1.38% と方向・幅とも揃って
  おり、別 OQ-2 の延長線上で扱う性質
- Pareto: 改善 4 セル / 退行 4 セル / noise 1 セルで微妙な分岐があるが、
  改善側のスケール (max −9.25%) が退行側 (max +1.63%) を圧倒。**採択を
  撤回する根拠なし**、commit はそのまま維持

cluster018 / 019 で軽微な退行が出る workload 構造的特徴:

- cluster018 は HR ≈ 73% の hit-heavy cluster (`st-twitter-5cluster.md`
  の整理)。hit path のコード肥大が刺さる
- cluster019 は scan-heavy だが per-shard の SIEVE state machine 滞留が
  長く、`Cache::get` 全体の icache 圧迫が出やすい

両者とも perf-gate `get_heavy_u64` / `mixed_lowskew_u64` の +1% 退行と
同根 (OQ-2)。本 commit のスコープ外。

## 6. オープン課題

| # | OQ | 状態 |
|---|---|---|
| OQ-1 | `Option<(usize, u16)>` の sret 化を NonZeroU16 niche で 16 byte 化して回避 | **解消** ✓ |
| OQ-2 | get_heavy_u64 / mixed_lowskew_u64 の +1% 退行の正体 | 残: NonZeroU16 で +0.86%, +1.38% に縮小したが完全消滅せず。Twitter cluster018/019 でも同方向の小退行を確認 (§5.2)。`Cache::get` 全行 disasm diff でコード肥大化の局所要因を特定するのは別 OQ |
| OQ-3 | Twitter trace cross-check (cluster016/018/019) | **解消** ✓ §5.2 で実施、HR 完全一致 + Pareto は改善寄りで採択維持 |
| OQ-4 | `find_scalar` 側にも A3 適用 | 残: scalar-only ホスト向け対称化、bench には影響しないが将来の CI ホスト想定 |
| OQ-5 | `entry_ptr_from_tag` を SAFETY assertion を debug only にして release で完全 1 命令化 | 残: 既に `debug_assert!` 経由なので release では消えているが、明文化していない |

## 7. 学び

- **sret threshold (16 byte) は ABI 設計の重要なクリフ**。返り型が
  「あと 2 byte で 16 を切れるか」の境目で性能が一段変わる。NonZeroU16 /
  NonZeroU32 等の niche-bearing 型でサイズを詰める設計は積極的に検討する
- **niche optimization は const_assert で固定化**: `size_of` を assert
  するだけで、将来の layout 変更で niche が崩れた場合 build error で
  検出できる。`#[repr(C)]` を使わない場合の保険として有効
- **多段試行で「制約の本体」を切り分ける**: S1+S2 棄却 (sret + LLVM fold
  失敗) → A3 で fold は解決、sret は残る → NonZeroU16 で sret も解消、
  という段階分解で各制約を独立に閉じられた。最初から正解を狙うより、
  asm を見ながら段階的に詰める方が本質を外さない
- **frontier.md / pext.md の机上見積もりは方向は正しかった**: 予測した
  「caller-merge で −5〜−7 cy 削減」は、3 段階すべて回した最終形で
  insert_u64 −7.15% / mixed_u64 −10.14% という数字に到達。ただし
  「inline されない」「LLVM fold が効かない」という障害は実機で
  asm を読まないと見えない、というのは memory `feedback_asm_inline_assumption`
  に追記済み
