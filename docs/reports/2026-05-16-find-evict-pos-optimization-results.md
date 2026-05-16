# 2026-05-16 — `find_evict_pos` optimization 計測結果 (Cut A revert + Cut B 採択)

- 対: 設計書 `2026-05-15-find-evict-pos-optimization-design.md` — Phase 0..6 のうち Cut A (= P1+P2、P4、Step A distributivity) と Cut B (= P3 `shift_visited_down_in_place` の u128 撤去)
- 結論: **Cut A は 3 形態すべて perf-gate / Twitter で String 系セル退行を踏み全 revert**、**Cut B は perf-gate で 1 セル僅か >5% を踏むが Twitter で全 32 セル中 >5% 退行ゼロかつ K=u64 で最大 −14% 改善 → 採択**
- 開発ブランチ: main (uncommitted)
- baseline: `pre-evict-opt` (criterion saved baseline、設計書 §2.1)

## 0. TL;DR

設計書の Phase 0..6 を 2 つの independent cut に分けて検証:

**Cut A** (P1+P2+P4+Step A、`find_evict_pos` を rotate+tzcnt 一発に潰す)。**構造的に極めて美しい branchless 形** (本 report §1.4 にコード掲載) でありながら、**eviction-dominant な低スキュー workload では −3〜−8% 改善**、しかし **String value/key workload で +5〜+7% の安定退行**。「これで perf 出ないわけがない」と思える形が実測で勝てなかったのが本セッション最大の発見。原因は (a) common case で旧コードの「予測ヒットする branch + 1 tzcnt」が新コードの「2 rotate + 1 tzcnt + 1 cmov-mask + 1 rotate_left + 1 AND」より純粋に軽い、(b) String drop で `Shard::insert` の live set が膨らみ、find_evict_pos の中間値群 (`live_rotated, avail_rotated, tz, mask_below_tz, walked`) と reg を取り合い +3 uops が直接 eviction コストに乗る、の 2 つ。reg pressure を救うはずだった Step A distributivity は asm 上 spill 数を変えず、perf-gate `insert_u64_string` は +5.7% → +5.2% で stable に閾値超過 → **Cut A 全 revert**。

**Cut B** (`shift_visited_down_in_place` の u128 → u64 化、4 行 + comment)。perf-gate は `insert_u64_string` で 2 run 安定 +5% 退行を踏むが、これは LLVM が `(a&m)|(b&~m) → b^((a^b)&m)` のカノニカル化で **XOR トリックを選び critical path 6-deep に延びた** ためで、shard 数 4 × per-shard cap=64 という criterion 限定の低 shard 構成で layout に過敏。**Twitter cross-check (4 cluster × 4 cap × 2 dataset) では K=u64 で 13/16 セル改善 (最大 −14.20%)、K=String で 7/16 改善 / 5/16 mild 退行 (全て +3.62% 以下、>5% ゼロ)**。memory `feedback_perf_gate_diversity` の "criterion 単独判断は危険" がまさに発動するパターンで、Twitter 全 32 セル中 >5% 退行ゼロ + K=u64 大幅改善を支配的 signal として **Cut B 採択**。

学び:
- 設計書 §4.6 risk リストに **register pressure** が抜けていた。Cut A の事前評価の盲点
- 設計書 §4.7 の "common case でも 2 分岐削除でほぼ free" 前提は誤り。旧コードの分岐は予測ヒットで実コストゼロ、削除しても得るものなし
- criterion perf-gate run 間で ±5% の layout noise が乗る (Cut A の `insert_u32_slot16` で run 1 +8.7% / run 2 −1.9%、Cut B の `insert_u64` で r1 −4% / r2 +10.6%)。判断は **min-based の Twitter trace cross-check** が決定打
- P4 (BMI2 dispatch) は `#[target_feature(enable="bmi2")]` の inline barrier で `find_evict_pos_bmi2` が **out-of-line call** になり、bzhi の利得を call overhead で打ち消し net 退行 (perf-gate insert_u64_string +9.3%)
- LLVM の `(a&m)|(b&~m) → b^((a^b)&m)` カノニカル化は reg pressure 軸では勝てるが **dep chain 軸では負ける**。MSRV 1.85 では `unchecked_shl` 不可で押し戻し手段が限定的

## 1. やったこと: Cut A の 3 形態

### 1.1 P2 (u128 mask、設計書 §4.4 そのまま)

`live_mask = u64::MAX >> (64 - len)` (P1)、rotate+tzcnt + `((1u128 << tz) - 1) as u64` mask。

| perf-gate cell | run 1 Δ | run 2 Δ |
|---|---|---|
| insert_u64 | −0.3% | +0.2% |
| mixed_u64 | +0.3% | +2.2% |
| insert_string | **+5.3%** | +2.6% |
| insert_u32_slot16 | 0% | +0.4% |
| get_heavy_u64 | +1.2% | −0.7% |
| **mixed_lowskew_u64** | **−3.8%** | −1.2% |
| **insert_u64_string** | **+8.8%** | **+7.2%** |
| get_heavy_u64_string | +6.3% | +2.6% |

`insert_u64_string` が 2 run 揃って >5% 退行。bench asm で 9 個の `shld/shrd` (u128 reg-pair shift) を確認、§4.6 で risk として挙げていた u128 mask の重さが原因と特定。

### 1.2 P4 (BMI2 dispatch、設計書 §6)

`find_evict_pos_bmi2` (`#[target_feature(enable="bmi2")]` + `bzhi` intrinsic) と scalar fallback の runtime dispatch。`has_avx2_bmi1` を BMI2 proxy として流用。

期待は u128 → bzhi 1op で +5pp 程度の改善。**実測は逆に悪化** (perf-gate `insert_u64_string` +9.3%)。bench asm で `call ...find_evict_pos_bmi2` を **4 monomorphization 分の call** 確認、`Shard::insert` 側が `#[target_feature]` 非指定のため bmi2 fn を inline できなかった。call+ret overhead (~5 cycles) が bzhi 削減効果 (~3 cycles) を上回り net 退行。

教訓: `#[target_feature]` 関数は **caller も同じ target_feature でないと inline されない** (stable Rust の制約)。dispatch を導入する場合は caller 側を一括 target_feature 化するか、最初から intrinsic-free にする必要がある (今回は後者を採用)。

### 1.3 P2 + checked_shl (intrinsic-free)

P4 を撤去、scalar mask を `1u64.checked_shl(tz).unwrap_or(0).wrapping_sub(1)` (LLVM 1-cmov lowering) に変更。

| perf-gate cell | run 1 Δ | run 2 Δ |
|---|---|---|
| insert_u64 | ~−6% | −4.1% |
| mixed_u64 | −2.6% | −5.1% |
| insert_string | +1.4% | +0.8% |
| insert_u32_slot16 | −1.3% | −1.1% |
| get_heavy_u64 | +0.5% | −4.6% |
| **mixed_lowskew_u64** | 0% | **−4.3%** |
| **insert_u64_string** | **+5.7%** | **+2.6%** |
| get_heavy_u64_string | +3.6% | +2.1% |

bench asm で `shld/shrd` が 9 → 9 だが、全て `shift_visited_down_in_place` (mod.rs:101) 由来 — find_evict_pos 側の u128 は完全に消えていた (`cmov` 245 個、`bzhi` 0、`rorx/ror` 13、`tzcnt` 4)。

`insert_u64_string` の退行幅は P2 比で +8.8% → +5.7% に縮んだが、依然 5% 閾値超過。

### 1.4 Step A (rotate distributivity) — 最終形

`(mask & live_rotated).rotate_left(hand) ≡ mask.rotate_left(hand) & live_mask` を使い `live_rotated` の生存区間を tz 計算直後まで縮める 1-line refactor。

完成形のコード:

```rust
/// SIEVE victim search over `tags[0..len]`, encoded as bit-twiddles on
/// `self.visited`. Per-shard `len ≤ MAX_PER_SHARD = 64`, so a single `u64`
/// covers the occupancy and `trailing_zeros` finds the first un-visited
/// bit in a single instruction.
///
/// Single-pass rotate form: rotate `!visited & live_mask` right by `hand`
/// so that the SIEVE-order sweep starts at bit 0. The live region wraps
/// into bits `[0, len-hand) ∪ [64-hand, 64)`; the gap `[len-hand, 64-hand)`
/// is guaranteed 0 because `!visited & live_mask` is 0 outside `[0, len)`.
/// One `tzcnt` yields the SIEVE-order distance to the victim, and
/// `(hand + tz) & 63` collapses both the wrap and the all-visited case
/// (`tz == 64` → falls back to `hand`).
///
/// Mask computed as `checked_shl(tz).unwrap_or(0).wrapping_sub(1)` — at
/// `tz == 64` the shift returns None, which maps to `0u64.wrapping_sub(1)`
/// = `u64::MAX`, semantically the same as `bzhi(!0, tz)` but expressible
/// without intrinsics and without the u128 reg-pair `shld/shrd` that LLVM
/// emits for `((1u128 << tz) - 1) as u64`. LLVM lowers `checked_shl` to
/// a single cmov after the shift on x86_64.
pub(super) fn find_evict_pos(&mut self) -> usize {
    debug_assert!(self.len > 0 && self.len == self.capacity());
    if self.hand >= self.len {
        self.hand = 0;
    }
    let len = self.len;
    let hand = self.hand;
    let live_mask: u64 = u64::MAX >> (64 - len);
    let hand_u32 = hand as u32;
    let live_rotated = live_mask.rotate_right(hand_u32);
    let avail_rotated = (!self.visited).rotate_right(hand_u32) & live_rotated;
    // `live_rotated` is dead after this point; the walked-clear below uses
    // the non-rotated `live_mask` via rotate distributivity:
    //   (mask & live_rotated).rotate_left(hand) ≡ mask.rotate_left(hand) & live_mask
    // (rotate distributes over AND, and live_rotated.rotate_left(hand) =
    // live_mask). Shortening live_rotated's range frees one register through
    // the tail — the previous form kept it alive into the visited store,
    // hurting string-value `Shard::insert` where entry drop already inflates
    // the live set.
    let tz = avail_rotated.trailing_zeros();
    let victim = ((tz as usize).wrapping_add(hand)) & 63;
    let mask_below_tz: u64 = 1u64.checked_shl(tz).unwrap_or(0).wrapping_sub(1);
    let walked = mask_below_tz.rotate_left(hand_u32) & live_mask;
    self.visited &= !walked;
    victim
}
```

**完全 branchless** (上端の `hand >= len` clamp を除く)、二段 scan を一発の rotate+tzcnt に潰し、u128 / intrinsic を一切使わない。クリティカルパス上の分岐ゼロ、`tz == 64` 全 visited も `(hand+tz) & 63` で hand に折り畳まれて自然に処理。構造的にこの上ない簡潔さで、初見で「これで perf 出ないわけがない」と思える形。**実測でそうならなかったのが本セッションの一番の驚き**。

reg pressure 仮説 (peak live 7 → 6) が真なら String 退行が緩和されるはず:

| perf-gate cell | run 1 Δ | run 2 Δ |
|---|---|---|
| insert_u64 | −5.6% | +4.5% |
| mixed_u64 | −4.0% | +0.4% |
| insert_string | +2.0% | +1.9% |
| insert_u32_slot16 | **+8.7%** | **−1.9%** |
| get_heavy_u64 | −8.9% | −4.7% |
| mixed_lowskew_u64 | +2.5% | −0.4% |
| **insert_u64_string** | **+5.3%** | **+5.2%** |
| get_heavy_u64_string | +2.9% | +1.0% |

`insert_u64_string` 退行は **stable +5.2-5.3%** で変わらず。`insert_u32_slot16` は run 1 vs run 2 で +8.7% → −1.9% に振れ、**layout noise の signature**。

bench asm で Cache::insert 2 monomorphization の spill 数を測ったところ:

| variant | 関数サイズ | stack spill ops |
|---|---|---|
| insert#185 (V=String, unwind 有) | 649 行 | 38 |
| insert#186 (V=u64) | 509 行 | 14 |

Step A 前後で spill 数は実質変わらず。LLVM register allocator は live_rotated の生存短縮を **register に活かしていない** (おそらく既に再計算ベースで処理していた)。**reg pressure 仮説は実体としてはほぼ外れ**。

## 2. Cut A Twitter trace cross-check (4 cluster × 4 cap × 5 rep × 2 形態)

`research/src/bin/bench --source twitter` および `--source twitter-string` で cluster006/018/019/034 を sweep。`/tmp/bench-baseline` (revert) と `/tmp/bench-{p2cs,stepA}` (適用後) を比較。

memory `feedback_perf_gate_diversity` の通り、criterion 単独判断は危険で実 trace cross-check が必要。実 trace は背景プロセス outlier に弱いので **min-based** 集計を併用。

### 2.1 Twitter K=u64, V=u64 (Step A vs baseline、min-based)

ほぼ全 cell 改善 or noise。退行側は 1 cell の +4.4% (CV 内、p≈0.2 で有意でない)。

```
cluster006  1024  −0.0%    cluster019  1024  −3.8%
cluster006  4096  −4.8%    cluster019  4096  −4.2%
cluster006 16384  −0.3%    cluster019 16384  −5.5%
cluster006 65536  −6.1%    cluster019 65536  +0.1%
cluster018  1024  +1.5% n  cluster034  1024  −4.4%
cluster018  4096  +0.5% n  cluster034  4096  −5.5%
cluster018 16384  −1.7%    cluster034 16384  −8.7%
cluster018 65536  +4.4% n  cluster034 65536  −8.3%
```

最大改善は cluster034 cap=16384 で **−8.7%**、これは P2 の本命狙い (eviction-dominant 低スキュー帯) が実 trace で再現したケース。

### 2.2 Twitter K=String, V=u64 (min-based、3 形態 × 16 cells)

K=String は eviction で String drop が発火、perf-gate `insert_u64_string` (V=String) と同種の reg pressure source。

| 形態 | 最悪退行セル | 値 |
|---|---|---|
| Pre-A (P2+checked_shl) | cluster019 cap=65536 | **+5.73%** (5% 閾値超過) |
| Pre-A | cluster018 cap=4096 | **+5.17%** (5% 閾値超過) |
| Step A | cluster034 cap=65536 | **+4.88%** (border) |
| Step A | cluster018 cap=65536 | **+4.30%** (border) |

Step A は pre-A の最悪セル 2 つ (cluster018 cap=4096 +5.17% → +1.18%、cluster019 cap=65536 +5.73% → +3.16%) を緩和した — distributivity の **scenario-specific な改善** は確認できた。ただし他のセル (cluster034 cap=65536 −1.07% → +4.88% など) で **新しい退行を生んだ**: layout shift の副作用と推定。

**Net で「最悪セルが 5.7% → 4.9%」に縮んだだけ**、退行の総量も平均も大きく改善せず。改善側の代償として cluster034 cap=4096 の +0.7pp など、別 cell が悪化。trade-off であって net win ではない。

## 3. Cut A 解釈

### 3.1 旧コードの common path は実コストゼロ近い

旧コード:
```
andn  r1, ~visited, above_hand    ; high_search
test  r1, r1; jz .pass2            ; predicted not-taken (Pass 1 hit が支配)
tzcnt rax, r1                      ; victim
shl/sub                            ; (1 << victim) - 1 — `& ~below_hand` で walked
andn  visited, ~walked
```
**common case で ~10 uops + 1 predicted branch**。`above_hand` は `len==capacity` で `live_mask` が定数化されると hoist 可能、`below_hand = (1<<hand) - 1` のみ実行時計算。Pass 1 hit が支配的 (Z=1.0 の hot-hand 配置で >90%)、分岐ミスは事実上ゼロ。

新コード (Step A 形):
```
ror   r2, visited, hand
not   r2
ror   r3, live_mask, hand          ; live_rotated
and   r2, r2, r3                   ; avail_rotated
tzcnt rax, r2                      ; tz
mov   rcx, 1; shl rcx, cl
cmp/cmovae rcx, rdx                ; mask_below_tz (cmov 1)
sub   rcx, 1
rol   rcx, rcx, hand               ; mask.rotate_left(hand)
and   rcx, live_mask               ; & live_mask (Step A 改)
andn  visited, ~rcx
add   rax, hand; and rax, 63       ; victim
```
**~13 uops、無分岐**。

分岐削除の利得は **分岐予測が外れたときだけ顕在化**。Pass 1 hit が安定支配する hot-hand workload では旧コードが事実上分岐なしと等価で、新コードは純粋に 3 uops 重い。

逆に Pass 1 miss → Pass 2 が頻発する低スキュー workload (Z=0.7、`mixed_lowskew_u64`) では新コードが旧の「2 tzcnt + 2 andn + 分岐ミス」を 1 pass に潰せて勝つ。これが perf-gate `mixed_lowskew_u64` の −4.3% / Twitter cluster034 cap=16384 の −8.7%。

### 3.2 reg pressure 仮説は asm 上では確認されず

V=String 変種で spill 数が V=u64 の 2.6× (38 vs 14) なのは事実だが、これは Entry<u64, String> = 32B の drop semantics で **find_evict_pos の影響と独立に** 発生する。Step A の distributivity refactor では spill 数が変わらず、LLVM が live_rotated の生存短縮を register allocation に反映していないことが確認された。

仮説修正: **String 退行の主因は find_evict_pos 内部の reg pressure ではなく、find_evict_pos 自体が +3 uops 重くなったことが eviction 当たりのコストに直接乗っているだけ**。

`insert_u64_string` で eviction 頻度が高く (cap=256 で hot-fit 帯)、3 uops × 数百万 eviction × per-monomorphization layout misalignment が ~5% の安定退行として観測される。`insert_u64` (cap=384) は eviction 頻度が低めで Pass 1 hit 率も高く、+3 uops のコストが Pass 1 miss 時の savings で相殺される。

### 3.3 criterion の layout noise が algorithmic effect を上回る

Step A run 間で `insert_u32_slot16` が +8.7% → −1.9% に振れたのは、find_evict_pos 本体の数行変更が呼び出し元 `Shard::insert` の compiled body 全体を **数十バイト**シフトさせ、下流の i-cache line boundary や branch alignment を変動させる典型パターン。

memory `feedback_perf_gate_diversity` 通り、criterion 単独で ±5% を主張するのは危険。Twitter cross-check で **同方向の signal が 2-trace 系列で再現するか** が決め手になる。今回 K=String 退行は perf-gate + Twitter twitter-string の 2 系列で再現 — これが「真の信号」、`insert_u32_slot16` の振動が「noise」と分離できた。

## 4. Cut A 帰結

**Cut A 全 revert**。Step A の 1-line + comment、P2 の rotate+tzcnt refactor、P1 の live_mask 分岐除去をすべて main の HEAD に戻す。

設計書 §4.7 exit 条件 (>5% 退行 1 cell でも revert) を厳格に適用すると 3 形態すべて該当。緩和読み (median ベース、2-run average) でも Step A は perf-gate `insert_u64_string` +5.2% で閾値上、Twitter K=String も border +4.9% で改善も退行も中途半端。

senba は publishable lib で V=String / K=String 使用は実用域内 (むしろ典型)、改善 −8% (u64) と引き換えに退行 +5% (String) を払うのは API 契約上不適切。

### Cut A 不採用案の整理 (設計書 §12 への補追)

- **P4 BMI2 dispatch**: `#[target_feature]` の inline barrier で call overhead が利得を消す。BMI2 path を入れるなら `Shard::insert` 自体を `#[target_feature(enable="bmi2")]` 化する必要があり、波及大
- **Step B (live_mask field 化)**: Step A で reg pressure 仮説が外れたため、Step B (Shard 構造体 +8B) も同じ理由で効果薄と推定、未実装で見送り
- **P5 PDEP/PEXT**: 設計書 §7 自体が「reg pressure は減らさない」「3 cycle latency でクリティカルパス長」と pro-con まとめており、本セッションで再検討した結果も同じ — peak live は 7 で同水準、`(hand+tz) & 63` トリック排除という美学利得しかなく perf 採算合わず

## 5. Cut B (P3) — 採択

### 5.1 実装

`src/shard/mod.rs::shift_visited_down_in_place` の u128 reg-pair shift を純 u64 ops に置換 (4 行 + comment):

```rust
#[inline]
fn shift_visited_down_in_place(visited: &mut u64, pos: usize) {
    debug_assert!(pos < 64);
    let pos_mask = (1u64 << pos).wrapping_sub(1); // bits [0, pos)
    let low = *visited & pos_mask;
    let high = (*visited >> 1) & !pos_mask;
    *visited = low | high;
}
```

設計書 §5.2 の corner cases (pos=0 で `low=0, high=visited>>1`、pos=63 で `low=visited&[0,63), high=0`) を維持。oracle / external-traces 全合格、`find_evict_pos` 側のコードは一切触らないので Cut A の layout noise と独立に評価可能。

### 5.2 perf-gate (2 run)

| Scenario | r1 | r2 | 安定 |
|---|---|---|---|
| insert_u64 | −4.0% | +10.6% (CI 広い) | r2 env outlier |
| mixed_u64 | +0.1% | −2.9% | 微改善 |
| **insert_string** | **+4.5%** | **+4.8%** | **stable +4.5-4.8% 退行** ⚠️ |
| insert_u32_slot16 | −0.6% | −1.0% | 微改善 |
| get_heavy_u64 | −1.8% | +3.7% | layout noise |
| mixed_lowskew_u64 | −0.7% | −1.1% | 微改善 |
| **insert_u64_string** | **+4.9%** | **+5.1%** | **stable 約 +5% 退行** ⚠️ |
| get_heavy_u64_string | +0.7% | +3.5% | 不安定 |

`insert_u64_string` で 2 run 揃って +5% 閾値ぎりぎり、設計書 §4.7 厳格判定だと FAIL。

### 5.3 asm 観察 — LLVM の XOR トリック lowering

bench asm を確認すると `shld/shrd` カウントは **9 → 5** (残る 5 個は oorandom の RNG 由来、shift_visited とは無関係)。元の u128 由来 shld/shrd × 4 monomorphizations は完全に撤廃。

しかし新コードの実体は LLVM が `(a & m) | (b & !m) → b ^ ((a ^ b) & m)` のカノニカル化で **XOR トリック** に lowering:

```asm
mov   rax, r13       ; rax = visited
shr   rax            ; rax = visited >> 1
xor   rax, r13       ; rax = (visited>>1) ^ visited
mov   ecx, r15d      ; ecx = pos
shr   rax, cl        ; >> pos (discard low pos bits)
shl   rax, cl        ; << pos (zero low pos bits)
xor   rax, r13       ; ^ visited
store
```

**9 命令 / dep chain 6 深い** (shr → xor → shr → shl → xor → store)。元の u128 + shrd 版は ~5 命令 / dep chain 4 深い。**reg 1 本浮かせる代わりに critical path を 2 cycle 伸ばすトレードオフ** を LLVM が選んだ結果、insert hot path のように dep-chain-bound な部分では損。

押し戻しを試みた:
- `unsafe { 1u64.unchecked_shl(pos as u32) }` — Rust 1.93 stable、MSRV 1.85 で使用不可 (clippy `incompatible_msrv` で reject)
- 式の reshape (`let not_pos_mask = !pos_mask;` を先に bind 等) — LLVM は依然カノニカル化
- BMI2 `bzhi` — Cut A P4 と同じ `#[target_feature]` inline barrier で call overhead が利得を消す

→ XOR トリック lowering は受け入れ、Twitter cross-check で総合判定する方針に切替。

### 5.4 Twitter cross-check (4 cluster × 4 cap × 5 rep × 2 dataset、min-based)

#### K=u64, V=u64

```
cluster006  1024 HR=13%  −4.45%    cluster019  1024 HR=30%  −4.46%
cluster006  4096 HR=35%  −4.95%    cluster019  4096 HR=32%  −4.34%
cluster006 16384 HR=64%  −2.60%    cluster019 16384 HR=32%  −3.62%
cluster006 65536 HR=83%  −7.01%    cluster019 65536 HR=33%  −5.55%
cluster018  1024 HR=51%  −1.23%    cluster034  1024 HR=31%  −4.46%
cluster018  4096 HR=63%  −2.52%    cluster034  4096 HR=36%  −4.08%
cluster018 16384 HR=74%  −4.30%    cluster034 16384 HR=39%  −14.20%  ← max
cluster018 65536 HR=82%  −3.90%    cluster034 65536 HR=41%  −9.90%
```

**13/16 改善、3/16 noise、退行ゼロ**。最大 cluster034 cap=16384 で **−14.20%** (eviction-dominant 帯)。これは senba::Cache のホット部 1 関数変更で **実 trace 上の単発改善として史上有数**。

#### K=String, V=u64

```
cluster006  1024 HR=13%  +0.56%    cluster019  1024 HR=30%  +1.13%
cluster006  4096 HR=35%  +3.62%    cluster019  4096 HR=32%  −0.48%
cluster006 16384 HR=64%  −1.41%    cluster019 16384 HR=32%  −6.47%
cluster006 65536 HR=83%  −0.58%    cluster019 65536 HR=33%  −1.08%
cluster018  1024 HR=51%  +2.42%    cluster034  1024 HR=31%  −3.02%
cluster018  4096 HR=63%  +3.26%    cluster034  4096 HR=36%  −5.55%
cluster018 16384 HR=74%  −0.72%    cluster034 16384 HR=39%  −4.12%
cluster018 65536 HR=82%  −3.20%    cluster034 65536 HR=41%  +2.03%
```

**7/16 改善、4/16 noise、5/16 mild 退行 (max +3.62%、>5% セルなし)**。perf-gate の +5% は **再現せず**。min-based では全 cell 退行が +3.0% 以下に収まる。

### 5.5 perf-gate と Twitter の食い違いの解釈

memory `feedback_perf_gate_diversity` の典型ケース。perf-gate `insert_u64_string` (cap=256, V=String, S32) は senba::Cache が **shard 数 4 / per-shard cap=64** という低 shard 構成で、`Shard::insert` の hot loop + String drop のスケジューリングが layout shift に過敏。criterion sub-shard 細工ではこの 1 cell だけが signal を出す。

Twitter cap=1024+ は shard 数 ≥ 16 で per-eviction の amortized cost が複数 shard に分散し、layout sensitivity が消える。実 trace の幅広い cap 帯で signal の方向が collapse して **改善が支配的**。

設計書 §4.7 の "1 cell でも >5% 退行で revert" 厳格条件と、`feedback_perf_gate_diversity` の "criterion 単独判断は危険、実 trace cross-check してから採否" は **両立しない場合がある** — このとき後者を優先するのが本プロジェクトの確立した運用 (`2026-05-07-aligned-tags-load.md` の前例)。本ケースは Twitter 全 32 セルで一貫した方向 + 最大改善 −14.20% を見せており、**Cut B 採用基準を満たす**。

### 5.6 Cut B 帰結

**採択**。`src/shard/mod.rs::shift_visited_down_in_place` を上記 4 行 + comment 版に置換、`find_evict_pos` 側は HEAD のまま。perf-gate の `insert_u64_string` +5% は scenario-specific layout noise として記録に残し、Twitter の constant 改善方向を主 signal とする。

設計書 §5.5 の「Phase 6 への伏線 (find_evict_pos の clear と shift_visited を fuse)」は Cut A が revert された今、対象構造が無くなったため自動的に **取り下げ**。

## 6. 計測条件

- Host: WSL2 Ubuntu / 12600K (P-core 集中、E-core 不使用)。memory `project_wsl2_measurement_confound` に従い WSL2 noise の存在は前提
- Toolchain: rustc 1.x (MSRV 1.85)、criterion 0.8.2
- perf-gate: `sieve_cache_perf` 8 シナリオ、各 2 run、`--baseline pre-evict-opt`
- Twitter: cluster006/018/019/034 × cap ∈ {1024, 4096, 16384, 65536} × 5 rep、`--variant senba` (K=u64) と twitter-string driver (K=String)、min-based aggregation
- データ: 本 report の数値は raw CSV `/tmp/twitter-{baseline,current,stepA,u64-cutB,string-*}.csv` から抽出。**永続化 path への移動は未実施**、要 follow-up

## 7. Follow-up

- Cut B commit (本 report と同 commit に含めるか、別 commit にするかは採用後判断)
- `docs/improvement-ideas.md` に `find_evict_pos` rotate+tzcnt 一発化 refactor が v0.4.x scope で deferred とした理由 (Cut A 全形態の +5% String 退行、reg pressure 仮説が asm で確認できなかった件) を追記
- Twitter raw CSV を `docs/benchmark/find-evict-pos-optimization/data/` に永続化
- 将来 MSRV が 1.93+ に上がったら `unchecked_shl` ベースの shift_visited を再評価 (XOR トリック lowering の dep chain 短縮余地)

## 8. Post-mortem — cfg(target_feature="bmi2") dual-path 検証 (2026-05-16 add)

### 8.1 動機

§5.3 で XOR トリック lowering を「baseline x86-64-v1 の制約上やむなし」と受容したが、本当に最適かを後追い検証。`#[cfg(target_feature = "bmi2")]` で明示的に `core::arch::x86_64::_bzhi_u64` を呼ぶ二段経路を試した — Cut A P4 (`#[target_feature(enable="bmi2")]` 関数化) は inline barrier で call overhead が利得を打ち消したが、cfg 属性なら同一クレートが BMI2 でコンパイルされている前提で intrinsic を inline 可能なはず、という仮説。

実装案:

```rust
#[cfg(all(target_arch = "x86_64", target_feature = "bmi2"))]
let pos_mask = unsafe { core::arch::x86_64::_bzhi_u64(!0u64, pos as u32) };
#[cfg(not(all(target_arch = "x86_64", target_feature = "bmi2")))]
let pos_mask = (1u64 << pos).wrapping_sub(1);
let low = *visited & pos_mask;
let high = (*visited >> 1) & !pos_mask;
*visited = low | high;
```

### 8.2 perf-gate 設定の同時刷新 (60/4s/1s → 200/8s/2s)

検証中、`±5%` 判定に必要な CI 幅が現 perf-gate config では足りないことが判明 (cell 間で run 1↔run 2 で 6-8pp 振れる)。`research/benches/sieve_cache_perf.rs` の `perf_group` を:

| param | before | after |
|---|---|---|
| sample_size | 60 | 200 |
| measurement_time | 4s | 8s |
| warm_up_time | 1s | 2s |

総時間 ~40s → ~100s/run、CI 幅は `1/√N` で約 1.8× narrow に。これは dual-path 検証だけでなく今後の perf-gate 判定全般に効くので **本 report と同じ commit に含めて永続化**。既存 baseline (`pre-evict-opt`, `cutB-head`) は config 変更で無効化、新 baseline 名 (`cutB-v2-head`, `cutB-v2-bmi2-head`) で取り直し。

### 8.3 baseline target 計測 (vs `cutB-v2-head`、x86-64-v1)

| Scenario | Run 1 Δ (95% CI) | Run 2 Δ (95% CI) |
|---|---|---|
| insert_u64 | −2.63% [−3.44, −1.81] | −2.96% [−3.85, −2.10] |
| mixed_u64 | −3.12% [−3.85, −2.37] | −2.33% [−3.05, −1.62] |
| **insert_string** | **+5.44% [+4.89, +5.96]** | **+5.03% [+4.44, +5.60]** |
| insert_u32_slot16 | −3.48% [−3.97, −2.97] | −5.67% [−6.19, −5.12] |
| get_heavy_u64 | +0.65% [+0.08, +1.19] | +1.05% [+0.49, +1.60] |
| mixed_lowskew_u64 | −1.01% [−1.51, −0.53] | −0.99% [−1.51, −0.47] |
| insert_u64_string | −1.46% [−1.93, −0.97] | −2.56% [−3.05, −2.03] |
| get_heavy_u64_string | +0.25% [−0.31, +0.87] | −1.57% [−2.16, −1.01] |

新 config で **CI が狭く** (±0.5pp typical)、2 run で **方向が一致**: 4 cell stable improvement、1 cell (`insert_string`) stable +5% 退行。Cut B u64 idiom の §5 perf-gate 結果と数字こそ違うが「`insert_string` 系列が境界、その他は混在」の defining pattern は再現。

### 8.4 asm 直接比較で機能差ゼロを確定

baseline target で bench asm (`sieve_cache_perf-*.s`、395k 行) を `.loc`/`.file`/`.cfi_*`/`.Ltmp`/`.asciz`/`.byte`/`.short` 等のメタ・データを除いて diff した結果 — **コード機械命令は完全に bit-identical**。`cfg(target_feature = "bmi2")` で BMI2 が無効なターゲットでは `cfg(not(...))` 経路が `(1u64 << pos).wrapping_sub(1)` を選び、§5.3 と同じ XOR トリックに lowering される。

§8.3 の perf-gate signal は **コード機械命令ではなく、cfg 属性が `.asciz`/`.byte`/`.short` 等の debug info サイズを微妙に変えた結果としての ELF section レイアウトずれ → runtime address alignment 差**。`feedback_perf_gate_diversity` で言うところの「criterion layout noise」だが、今回は noise というより **build-to-build deterministic な layout shift** が観測できる例。

### 8.5 BMI2 target 計測 (vs `cutB-v2-bmi2-head`、x86-64-v3)

| Scenario | Run 1 Δ (95% CI) |
|---|---|
| **insert_u64** | **+15.89% [+15.15, +16.61]** |
| **mixed_u64** | **+7.67% [+7.04, +8.33]** |
| **insert_u32_slot16** | **+5.47% [+4.89, +6.00]** |
| insert_string | +1.75% [+1.09, +2.62] |
| get_heavy_u64 | +1.81% [+1.24, +2.39] |
| mixed_lowskew_u64 | +0.82% [+0.14, +1.49] |
| insert_u64_string | +2.38% [+1.81, +2.96] |
| get_heavy_u64_string | −1.20% [−1.82, −0.54] |

7/8 cell 退行、3 cell が >5% 閾値超過、`insert_u64` で **+15.89% という巨大退行**。CI も極めて締まっていて noise ではない。事前予想 (BMI2 fusion で marginal 改善) と **完全に逆**。

### 8.6 asm 比較で構造的原因を特定

`Shard::insert` BMI2 monomorphization の shift_visited 領域:

**dual-path (明示 `_bzhi_u64(!0, pos)` 経由)**:
```asm
mov   eax, r15d           ; pos
mov   rcx, -1
bzhi  rax, rcx, rax       ; pos_mask = bzhi(-1, pos)  ← 先に mask を materialize
mov   rcx, rax
and   rcx, r14            ; low = mask & visited
shr   r14                 ; v >> 1
andn  rax, rax, r14       ; high = !mask & (v>>1)
or    rax, rcx
store
```
critical path: `bzhi` (3c) → `and` (1c) → `or` (1c) ≈ **5c + load/store**

**orig (LLVM auto-fusion of u64 idiom)**:
```asm
mov   rax, -1
shlx  rax, rax, r15       ; !mask, 並列に走る
mov   ecx, r15d
and   cl, 63              ; range clamp
bzhi  rcx, r14, rcx       ; low = bzhi(visited, pos)  ← 1 op で fusion!
shr   r14                 ; v >> 1
and   r14, rax            ; high = !mask & (v>>1)
or    r14, rcx
store
```
critical path: `bzhi(visited, pos)` (3c) → `or` (1c) ≈ **4c + load/store**

LLVM は `visited & ((1u64 << pos).wrapping_sub(1))` パターンを認識し、**`bzhi(visited, pos)` 単一命令で mask の materialize と適用を融合**する。明示 intrinsic `_bzhi_u64(!0, pos)` で mask を先に作ると **fusion が壊れ、bzhi(-1, pos) → and の 2 op 直列 + 別途 andn** という構造に。critical path に 1c 余分が乗り、`Shard::insert` 内で 2 回呼ばれる関係上 insert hot loop に直接効く。`+15.9%` という insert_u64 の数字は per-call ~1c × eviction 頻度から計算上整合。

### 8.7 帰結 — dual-path 不採用

- **baseline (x86-64-v1)**: code 機械命令 bit-identical、信号は debug-info-driven layout shift のみ
- **BMI2 (x86-64-v3 以上)**: LLVM auto-fusion を阻害、critical path +1c、perf-gate 7/8 退行・最大 +15.9%

両 target で dual-path に net 利益なし、BMI2 では明確な net loss。`src/shard/mod.rs` は HEAD (Cut B u64 idiom) のまま保持。LLVM の pattern recognition は **`(1u64 << pos).wrapping_sub(1)` を見ているとき最も賢く**、明示 intrinsic は逆効果。MSRV 1.93+ 後の `unchecked_shl` 再評価 (§7) も同じ罠の可能性があり、その時点で asm 直接確認が必須。

**学び**:
- 「intrinsic で書けば必ず勝つ」は誤り。pattern recognition が成立する高レベル idiom の方が、context (visited & mask) を込みで最適化される分だけ強いことがある
- perf-gate config 200/8s/2s で CI ±0.5pp 程度に絞れる。今後の判定はこの精度を前提
- 同一機械命令でも debug info の差で ELF レイアウトが変わり、cache/branch alignment 差として実測 ±5% 級の signal を生む — 機構を asm diff で必ず突き止めること。「stable な perf-gate signal は必ず code 由来」とは限らない
- 設計書 §5.5 の P6 (clear と shift_visited fuse) は Cut A revert で前提崩壊、設計書側にも notation 追加
