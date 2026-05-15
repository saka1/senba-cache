# 2026-05-15 — `find_evict_pos` 周辺の instruction-level 最適化 (設計書)

- 対象実装: `src/shard/scan.rs::find_evict_pos`, 隣接 `src/shard/mod.rs::shift_visited_down_in_place`, 呼び出し側 `src/shard/state.rs::insert`
- 関連: `2026-05-10-visited-bitmap.md` (`visited` を u64 bitmap 化した前段。本稿はその上に乗る instruction-level 詰め)
- 種別: **設計書**。Phase 0..6 を順次実装→perf-gate で計測→採否を決める前提
- ターゲット ISA: x86_64 / AVX2 + BMI1/2 (Haswell+ / Zen 3+)。Zen 1/2 はもう考えない (ターゲット外)。
- 開発ブランチ: `claude/optimize-find-evict-pos-6IcJs`

## 0. TL;DR

`find_evict_pos` は eviction 経路の核。`visited: u64` bitmap 化後の現実装は
2 つの pass を 2 つのデータ依存分岐で繋いだ素直なコードで、

- `len==64` での `1<<len` 回避分岐
- `[hand, len)` pass の trailing_zeros が値を返すか否かの分岐
- `[0, hand)` pass の trailing_zeros が値を返すか否かの分岐

の 3 分岐 + 直列 tzcnt 2 本 + u128 シフト 1 個が残っている。本稿は以下を順次試す:

| Phase | 内容 | 期待 | 依存 |
|---|---|---|---|
| 0 | baseline 凍結 | — | — |
| 1 | `live_mask` の分岐除去 (`u64::MAX >> (64-len)`) | 分岐 -1 | なし |
| 2 | rotate + 単一 tzcnt 化 (intrinsics-free) | 分岐 -2, tzcnt -1 (critical path) | P1 |
| 3 | `shift_visited_down_in_place` の u128 撤去 | u128 ALU 撤去 (隣接 hot) | なし |
| 4 | BMI2 ゲート + `bzhi` で walked-mask を 1 op | ALU -1, cmov 排除 | P2 |
| 5 | PEXT/PDEP packed search (BMI2) | hand 復元の `& 63` トリック排除 + clear が PDEP 1 本 | P4 |
| 6 | `find_evict_pos` の clear と `shift_visited` を fuse | visited の store 1 回ぶん | P2 (+ P3) |

各 Phase は **独立に commit & perf-gate**。各セルで >5% 退行が出たらその Phase で revert、
次へ進む前に原因切り分け。

## 1. 現状コード再掲と uop 分析

### 1.1 `find_evict_pos` (`scan.rs:203-241`)

```rust
pub(super) fn find_evict_pos(&mut self) -> usize {
    debug_assert!(self.len > 0 && self.len == self.capacity());
    if self.hand >= self.len { self.hand = 0; }
    let len  = self.len;
    let hand = self.hand;
    let live_mask: u64 = if len >= 64 { !0u64 } else { (1u64 << len) - 1 };  // (A)
    let below_hand: u64 = (1u64 << hand) - 1;
    let above_hand: u64 = live_mask & !below_hand;

    let high_search = !self.visited & above_hand;
    if high_search != 0 {                                                    // (B)
        let victim = high_search.trailing_zeros() as usize;
        let walked = ((1u64 << victim) - 1) & !below_hand;
        self.visited &= !walked;
        return victim;
    }
    self.visited &= !above_hand;

    let low_search = !self.visited & below_hand;
    if low_search != 0 {                                                     // (C)
        let victim = low_search.trailing_zeros() as usize;
        let walked = (1u64 << victim) - 1;
        self.visited &= !walked;
        return victim;
    }
    self.visited &= !below_hand;
    hand
}
```

分岐 (A)/(B)/(C) のうち、(A) は `len == capacity` で固定する shard が大半 (capacity が
64 にチューニングされる shard では (A) は常に true 側) なので予測しやすい。**(B)/(C) は
`visited` のパターン依存で predictor がブレやすい**。Twitter trace (eviction-dominant 帯)
で (B) が分岐ミスする頻度を VTune で確認する余地はあるが、本稿では**分岐を消す**方向に倒す。

### 1.2 `shift_visited_down_in_place` (`mod.rs:97`)

```rust
fn shift_visited_down_in_place(visited: &mut u64, pos: usize) {
    let v = *visited as u128;
    let low = v & ((1u128 << pos) - 1);
    let high = (v >> (pos + 1)) << pos;
    *visited = (low | high) as u64;
}
```

`pos==63` で `>>64` を回避するためだけに u128。`u128` は SysV では reg pair (rdx:rax) に
広がるので、`shl/shr` が 2 命令ずつ + cmov になりがち。提案 3 で純 u64 に落とす。

## 2. Phase 0 — baseline 凍結

### 2.1 やること

```bash
# senba-research benches
cargo bench -p senba-research --bench sieve_cache_perf -- --save-baseline pre-evict-opt
# 同 concurrent (src/concurrent には触らないが、回帰チェック用に取っておく)
cargo bench -p senba-research --bench sieve_concurrent_perf -- --save-baseline pre-evict-opt
```

ベースラインは **branch tip ではなく Phase 0 commit (本企画書 commit)** で固定する。
以降の Phase は同じベースラインに対する AB のみで判定する (Phase 連鎖の累積誤差を避ける)。

### 2.2 補助計測 (任意)

- VTune (Windows / `research/src/bin/bench_vtune.rs`) で `find_evict_pos` の cycles/inst を
  事前取得。Phase 2 後と比較できると効果の attribution が明確になる。
- `perf stat -e branch-misses,instructions` を Linux で。eviction-dominant のシナリオ 6
  (Zipf 0.7) が一番動くはず。

### 2.3 Exit 条件

- baseline saved、perf-gate の `before` ファイル群を `target/criterion/*/pre-evict-opt/`
  に確認。

---

## 3. Phase 1 — `live_mask` 分岐除去

### 3.1 動機

`if len >= 64 { !0 } else { (1<<len) - 1 }` は **`1<<64` の UB 回避**のためだけの分岐。
`len ∈ [1, 64]` なので `u64::MAX >> (64 - len)` に置換できる:

| `len` | `64 - len` | `u64::MAX >> (64 - len)` |
|---|---|---|
| 1 | 63 | `0x1` |
| 32 | 32 | `0xFFFFFFFF` |
| 64 | 0 | `u64::MAX` |

`shr cl, rax` 1 命令 (BMI2 環境なら `shrx`、フラグ依存を切れる)。

### 3.2 変更

`scan.rs:211` の 1 行のみ:

```rust
- let live_mask: u64 = if len >= 64 { !0u64 } else { (1u64 << len) - 1 };
+ // len ∈ 1..=64 なので 64-len ∈ 0..=63 で >>UB に触れない。len=64 → >>0 = !0。
+ let live_mask: u64 = u64::MAX >> (64 - len);
```

### 3.3 検証

- `cargo test -p senba --tests` (`tests/eviction.rs` の sequence test で吸える)
- `cargo test -p senba-research --features external-traces -- oracle` で sieve_orig との
  hit/miss/eviction sequence 一致
- `cargo bench -p senba-research --bench sieve_cache_perf -- --baseline pre-evict-opt`

### 3.4 Exit 条件

- 全シナリオで within noise (±2%) または改善。
- 退行 >2% が 1 セルでもあれば差し戻し→原因調査 (LLVM の `cmov` 生成が `shr+mask` より速い
  ケースがありうる; その場合 Phase 1 は捨てて Phase 2 に統合)。

---

## 4. Phase 2 — rotate + 単一 tzcnt 化 (本命・intrinsics free)

### 4.1 動機

二段スキャンを 1 つの rotate + tzcnt に畳む。`hand` を bit 0 に持ってきて、ギャップ
(rotated 後の `[len-hand, 64-hand)`) は `avail = !visited & live_mask` の構成上必ず 0 に
なるので、`tzcnt` は SIEVE 順での最初の未訪問ビットを直接返す。

### 4.2 正当性

- `avail` の bit `i` は「位置 `i` が live かつ未訪問」のとき 1。
- `rotated = avail.rotate_right(hand)` の bit `i` は元の bit `(hand + i) mod 64`。
- 元の live 領域は `[0, len) ⊆ [0, 64)`、`avail` は live 領域外で 0。
- `rotated` の bit 配置:

  | rotated bit | 元 bit | live? |
  |---|---|---|
  | `[0, len-hand)` | `[hand, len)` | yes |
  | `[len-hand, 64-hand)` | `[len, 64)` | no (ギャップ、必ず 0) |
  | `[64-hand, 64)` | `[0, hand)` | yes |

- ギャップは 0 なので `trailing_zeros(rotated)` は SIEVE 順での最初の未訪問ビットの距離
  を返す:
  - `tz ∈ [0, len-hand)` → 元位置 `hand + tz`
  - `tz ∈ [64-hand, 64)` → 元位置 `tz - (64 - hand) = (hand + tz) mod 64`
  - `tz == 64` (`avail == 0`) → 全 visited、`(hand + 64) & 63 = hand` でフォールバック

- victim の式は 3 ケース共通で `(hand + tz) & 63`。

### 4.3 Walked-bits クリア (Phase 2 では intrinsics 不使用版)

クリア対象は rotated frame で `[0, tz)`、ただし live 領域外は 0 のままで構わないので
`live_mask.rotate_right(hand)` で AND しても挙動同等。`tz ∈ [0, 64]` での
`(1<<tz)-1` を branchless で得る 3 候補:

| 方法 | コスト | 評価 |
|---|---|---|
| `u128` 経由: `((1u128 << tz) - 1) as u64` | shl/shr × 2 + cmov 程度 | Phase 2 はこれを採用 |
| `1u64.checked_shl(tz).unwrap_or(0).wrapping_sub(1)` | cmov 1 | LLVM 出力が読みづらい |
| `_bzhi_u64(!0, tz)` | 1 op | Phase 4 で乗せる |

Phase 2 では u128 経由を採用 (BMI2 ゲートを足さずに済む)。Phase 4 で `bzhi` に置き換え。

### 4.4 提案コード (Phase 2 完成形)

```rust
pub(super) fn find_evict_pos(&mut self) -> usize {
    debug_assert!(self.len > 0 && self.len == self.capacity());
    if self.hand >= self.len { self.hand = 0; }
    let len  = self.len;
    let hand = self.hand;

    // Phase 1 と同じ分岐レス live_mask
    let live_mask: u64 = u64::MAX >> (64 - len);

    // 1 度きりの rotate で avail と live_rotated の両方を構築
    let live_rotated   = live_mask.rotate_right(hand as u32);
    let avail_rotated  = (!self.visited).rotate_right(hand as u32) & live_rotated;
    let tz             = avail_rotated.trailing_zeros();          // [0, 64]

    // victim: tz==64 のとき自動的に hand に折り畳まれる
    let victim = ((tz as usize).wrapping_add(hand)) & 63;

    // walked: rotated frame で [0, tz) ∩ live_rotated → rotate_left で戻す
    let mask_below_tz: u64 = ((1u128 << tz) - 1) as u64;          // tz=64 で u64::MAX
    let walked = (mask_below_tz & live_rotated).rotate_left(hand as u32);
    self.visited &= !walked;

    victim
}
```

クリティカルパス (rough): `load visited → andn → ror → tzcnt → add → return` ≒ 5–6 cycle。
元の最良ケース (Pass 1 ヒット) と同等、最悪ケース (Pass 1 miss → Pass 2) より 1 tzcnt
ぶん短縮、加えて 2 分岐除去。

### 4.5 検証

- `cargo test --workspace` (内部不変式を test で握っているのは eviction sequence test、
  retain test など。oracle が全パターンを舐めるので最も信頼できる)
- `cargo test -p senba-research --features external-traces -- oracle_cache_match`
  (`tests/oracle_cache_match.rs`): sieve_orig と eviction sequence が bit-for-bit 一致
  すれば semantics 維持の最強保証
- perf-gate AB (vs `pre-evict-opt`)

### 4.6 リスク

- ロジック誤り。特に `tz==64` 折り畳みと `(hand+tz) & 63` の符号扱い。Oracle test が
  検出する。
- LLVM が `rotate_right` を `rorx` (BMI2) ではなく汎用 `ror` で出すか。どちらも 1 cycle
  だが `rorx` は src/dst 別、`ror` は同一レジスタ in/out。差は小さいはず。
- u128 経由の `(1u128 << tz) - 1` が想定より重い (cmov 2 + shl 2 + sub 2)。コード生成を
  godbolt で目視確認、目に余れば Phase 4 で `bzhi` に置換。

### 4.7 Exit 条件

- oracle 全合格。perf-gate 退行 0、改善が出るシナリオが 1 つ以上。
- 退行 >5% が 1 セルでも出たら revert (commit を rebase で剥がす)。

---

## 5. Phase 3 — `shift_visited_down_in_place` の u128 撤去 (独立隣接)

### 5.1 動機

Phase 2 と独立。`mod.rs:97` の関数を u64 だけで書き直す:

```rust
fn shift_visited_down_in_place(visited: &mut u64, pos: usize) {
    debug_assert!(pos < 64);
    // pos ∈ [0, 63] なので 1<<pos は安全。
    let pos_mask = (1u64 << pos).wrapping_sub(1);   // bits [0, pos)
    let low      = *visited & pos_mask;             // bits [0, pos)
    let high     = (*visited >> 1) & !pos_mask;     // bits [pos, 63) を pos 起点に詰める
    *visited     = low | high;
}
```

### 5.2 正当性 (corner)

- `pos==0`: `pos_mask=0`, `low=0`, `high = visited>>1`. 「bit 0 を落として全体 down 1」=
  仕様通り。
- `pos==63`: `pos_mask = (1<<63)-1`, `low = visited & [0,63)`, `high = (visited>>1) &
  bit63 = 0` (shr で空いた最上位は 0). 結果 `low` = bit 63 が消えた `visited`。仕様通り。

### 5.3 検証

- `cargo test -p senba`: 既存の eviction / remove / retain test が visited bitmap の整合
  を間接的に検査している。
- oracle (Phase 2 と同じ) で 100% カバー。

### 5.4 Exit 条件

- 全テスト合格、perf-gate 退行 0。`insert` の eviction 経路と `remove` 経路が触るので、
  insert_u64 / insert_string / mixed_lowskew あたりで僅かな改善が見える可能性。

### 5.5 Phase 6 への伏線

`find_evict_pos` の clear と `shift_visited` を融合する場合 (Phase 6)、Phase 3 で純 u64
化しておくと fuse 後の式が綺麗に閉じる。

---

## 6. Phase 4 — BMI2 ゲート + `bzhi` 置換

### 6.1 動機

Phase 2 の `((1u128 << tz) - 1) as u64` を `bzhi(!0, tz)` 1 op に置き換える。
ついでに `(1u64 << hand) - 1` 系の式 (`shift_visited_down_in_place` の `pos_mask` や、
仮に残っている `below_hand`) も `bzhi(!0, hand)` に統一できる。

### 6.2 BMI2 検出

現在は `has_avx2_bmi1` 一本 (`Cache` ctor で取得)。BMI2 検出を **追加** する:

```rust
// src/lib.rs (Cache struct と new に追加)
has_bmi2: bool,
// new():
let has_bmi2 = is_x86_feature_detected!("bmi2");
```

`Shard` 側にも propagate (or `find_evict_pos` の引数として渡す。`find` と違って
`find_evict_pos` は1呼び出しごとに propagation cost が乗るので、`Cache` 側で
**コンパイル時に分岐** するルートも検討余地: `#[target_feature(enable = "bmi2")]` の
unsafe 版を `static_dispatch!` で切り替える形 (find_avx2 と同パターン)。

### 6.3 提案コード (Phase 2 + 4)

```rust
// scan.rs (BMI2 path)
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "bmi2")]
unsafe fn find_evict_pos_bmi2(&mut self) -> usize {
    debug_assert!(self.len > 0 && self.len == self.capacity());
    if self.hand >= self.len { self.hand = 0; }
    let len  = self.len;
    let hand = self.hand;
    use std::arch::x86_64::_bzhi_u64;

    let live_mask    = unsafe { _bzhi_u64(!0u64, len as u32) };       // = MAX >> (64-len) と等価
    let live_rotated = live_mask.rotate_right(hand as u32);
    let avail_rot    = (!self.visited).rotate_right(hand as u32) & live_rotated;
    let tz           = avail_rot.trailing_zeros();
    let victim       = ((tz as usize).wrapping_add(hand)) & 63;

    let mask_below_tz = unsafe { _bzhi_u64(!0u64, tz) };               // tz=64 → !0u64
    let walked = (mask_below_tz & live_rotated).rotate_left(hand as u32);
    self.visited &= !walked;
    victim
}
```

`bzhi` の挙動を再確認: `bzhi(src, idx)` は `idx[7:0] < 64` なら `src & ((1<<idx)-1)`、
そうでなければ `src` をそのまま返す。よって:

- `bzhi(!0, len)` for `len ∈ [1,64]`: `len=64` で `!0`、それ以外で `(1<<len)-1`。✓
- `bzhi(!0, tz)` for `tz ∈ [0,64]`: `tz=64` で `!0`、それ以外で `(1<<tz)-1`。✓

### 6.4 検証

- 非 BMI2 (古い CPU) でのフォールバック: `find_evict_pos` (Phase 2 版) を scalar として
  残す。`Cache::new` で `has_bmi2 == false` のとき scalar 経路へ。Linux/CI で実際に
  `bmi2=off` をシミュレートできるか確認 (`is_x86_feature_detected!` は対応する
  cpuid フラグを見るので、QEMU で `-cpu` を絞る等)。
- oracle test は CPU に依存せず scalar と BMI2 の両方が同 sequence を出すことを保証 (両
  経路を test config から呼ぶマトリクスを足すと安心)。
- perf-gate AB。

### 6.5 Exit 条件

- BMI2 path で perf-gate に改善が出る。出なければ Phase 4 はスキップ (Phase 2 の
  intrinsics-free 版でステイ)。
- 非 BMI2 path の挙動が Phase 2 と完全一致。

---

## 7. Phase 5 — PEXT/PDEP packed search (BMI2 専用)

### 7.1 動機

Phase 4 までで rotate frame の「ギャップ」を `tzcnt` の挙動に頼って無視していた部分を、
**PEXT で物理的にパックする** ことで `(hand + tz) & 63` の `& 63` トリックを廃する。
`live_rotated` を mask にして `pext` すれば、`packed` の bit `i` は SIEVE 順で `i` 番目の
位置の visited 状態に直接対応する。

### 7.2 提案コード

```rust
#[target_feature(enable = "bmi2")]
unsafe fn find_evict_pos_pext(&mut self) -> usize {
    if self.hand >= self.len { self.hand = 0; }
    let len  = self.len;
    let hand = self.hand;
    use std::arch::x86_64::{_bzhi_u64, _pdep_u64, _pext_u64};

    let live_mask    = unsafe { _bzhi_u64(!0u64, len as u32) };
    let live_rotated = live_mask.rotate_right(hand as u32);
    let visited_rot  = self.visited.rotate_right(hand as u32);

    // packed の bit i は SIEVE 順 i 番目の位置の visited 状態 (i ∈ [0, len))
    let packed       = unsafe { _pext_u64(visited_rot, live_rotated) };
    // 先頭未訪問。!packed の bit を [0, len) に制限してから tzcnt。
    let unvisited    = unsafe { _bzhi_u64(!packed, len as u32) };
    let tz           = unvisited.trailing_zeros();                   // [0, len] (len で all-visited)

    // packed offset → 元位置: hand から tz 進み、len で wrap
    let victim_offset = if tz as usize >= len { 0usize } else { tz as usize };
    let victim = {
        let raw = hand + victim_offset;
        if raw >= len { raw - len } else { raw }
    };

    // clear: packed の [0, tz) を PDEP で rotated frame に戻して rotate_left
    let walked_packed = unsafe { _bzhi_u64(!0u64, tz) };
    let walked_rot    = unsafe { _pdep_u64(walked_packed, live_rotated) };
    self.visited &= !walked_rot.rotate_left(hand as u32);

    victim
}
```

(`victim` の場合分けは cmov に潰れる。`tz >= len` を `tz == len` で書いてもよい。)

### 7.3 利点 / 欠点

**利点**:

- `& 63` トリック不要、`(hand+tz) mod len` で意味論が明示化される。
- `len < 64` の shard で「ギャップに依存しない」純粋なロジックになり、将来 `per_shard ≠ 64`
  の運用 (例: `MAX_PER_SHARD` を増やすなど大改造) でも壊れにくい。

**欠点 (要計測)**:

- `pext`/`pdep` は Intel/Zen 3+ で 3 cycle latency, 1 cycle throughput。Phase 4 (`bzhi`
  + `rotate` + `tzcnt`) との直列の比較は数 cycle 単位の勝負。**実測なしで採否を決めない**。
- 命令数は Phase 4 と同等〜少し多い。クリティカルパスの差は微妙。
- BMI2 必須。

### 7.4 検証

- oracle 全合格
- perf-gate AB vs Phase 4 (両方の baseline を比較)
- 期待: insert-only Slot32 系 (シナリオ 1, 4) で Phase 4 と同等〜微改善、mixed_lowskew
  (シナリオ 6) で eviction が多いぶん差が見えやすい

### 7.5 Exit 条件

- Phase 4 比で改善が観測されれば採択
- 同等〜微退行なら Phase 4 でステイ (コードは残すが #[cfg] 外)

---

## 8. Phase 6 — `find_evict_pos` の clear と `shift_visited` 融合

### 8.1 動機

`state.rs::insert` の eviction 経路:

```rust
let pos = self.find_evict_pos();                                  // visited &= !walked
let id  = Self::id_of(self.tags[pos]) as u16;
let entry = unsafe { std::ptr::read(self.entry_ptr(id as usize)) };
self.tags.copy_within(pos + 1..self.len, pos);
shift_visited_down_in_place(&mut self.visited, pos);              // visited を再び更新
```

`visited` は `find_evict_pos` の末尾と `shift_visited` の頭で 2 回 RMW される。これを
**1 つの式に閉じ込める** ことで store を 1 回減らせる:

```rust
// fused 版 (find_evict_pos が pos と "shift 込みの新 visited" を一緒に返す)
let v_after_clear = self.visited & !walked;
let pos_mask = (1u64 << pos).wrapping_sub(1);
self.visited = (v_after_clear & pos_mask) | ((v_after_clear >> 1) & !pos_mask);
```

### 8.2 どこに置くか

選択肢:

- (a) `find_evict_pos` が `(pos, new_visited)` を返し、caller がそれを書き込む
- (b) `find_evict_pos` を `find_and_evict_visited` のような関数に格上げして visited 更新まで責任を持つ
- (c) inline 化に任せて LLVM の load-store フォワーディングに期待 (最小手)

(c) で済むなら一番良いので、まず Phase 2 + 3 後にアセンブリで `visited` のロード/ストアが
1 回に潰れているか確認する。潰れていなければ (a) に進む。

### 8.3 リスク

- 層分け (scan.rs はスキャン primitive、state.rs は state machine) の責務が混ざる
- find_evict_pos のシグネチャ変更は呼び出し元が 1 箇所 (`state.rs::insert`) しかないので
  許容できるが、`pub(super)` の意味が変わるのでドキュメントを丁寧に
- `pos` 適用後の visited に対する `shift_visited` の挙動が「`pos` の bit を消した上で
  上位を down 1」になる。元の `find_evict_pos` 末尾の `visited &= !walked` がすでに
  `pos` の bit を 0 にしているはずなので、shift の `pos_mask` 切り出しと整合する
  (これは Phase 6 実装時に oracle で要確認)

### 8.4 検証 / Exit

- oracle 全合格 (sequence が一致しなければ即 revert)
- perf-gate で +0.5% 程度の改善を目標。出なければ放棄 (採否ライン: 退行 0)

---

## 9. 計測プロトコル (全 Phase 共通)

### 9.1 必須

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test  --workspace
cargo test  -p senba-research --features external-traces -- oracle
cargo bench -p senba-research --bench sieve_cache_perf -- --baseline pre-evict-opt
```

ベンチは **最低 2 回**回して中央値の安定を確認する (criterion の noise band より下の信号は
信用しない)。シナリオ単位で:

| シナリオ | 触る経路 | 効果見込み |
|---|---|---|
| 1 (insert_u64 Slot32 Zipf 1.0) | hot warm-up + 軽度 evict | +/- 小 |
| 2 (mixed 50/50 Zipf 1.0) | find_avx2 主 + 軽度 evict | + 小 |
| 3 (insert_string Slot64) | evict 重い (drop 含む) | + 中 |
| 4 (insert_u32_slot16) | evict 多 (per-shard 4 entries) | + 中〜大 |
| 5 (get-heavy 90/10) | ほぼ find のみ | 無関係 |
| 6 (mixed_lowskew Zipf 0.7) | evict-dominant | + 大 (本命) |
| 7 (insert u64→String Slot32) | evict + drop | + 中 |
| 8 (get-heavy u64→String) | ほぼ find | 無関係 |

### 9.2 任意 (確証強化)

- Twitter trace cross-check (`research/src/bin/bench --source twitter`): perf-gate と
  cross-check が食い違ったら layout noise を疑う (`2026-05-07-aligned-tags-load.md`
  パターン)
- VTune (Windows / `research/src/bin/bench_vtune.rs`): `find_evict_pos` の cycles/inst,
  branch-misses, retired-uops 分布。Phase 2 後と Phase 4 後の 2 点で取れると説明力が高い

### 9.3 アセンブリ目視

各 Phase 後、godbolt or `cargo rustc -- --emit=asm` で `find_evict_pos` を確認。
特に:

- Phase 2: `rotate_right` が `ror` / `rorx` で出ているか、`tzcnt` が直列に並んでいるか
- Phase 3: `shift_visited` から `mov rdx, rax; shr rdx, ...` のような u128 reg pair 残骸が
  消えているか
- Phase 4: `bzhi` が実際に発火しているか (LLVM の intrinsic dispatch)
- Phase 5: `pext`/`pdep` が `[mem]` 経由ではなく純レジスタ間で出ているか

## 10. ロールバック方針

各 Phase で **独立 commit**。退行が出た Phase は `git revert` で外し、後続 Phase は

- 該当 Phase に **依存する** 後続: 当該 Phase も revert
- **独立** な後続 (例: Phase 3 は Phase 2 と独立): そのまま継続

依存表 (再掲):

```
P0 → P1 → P2 → P4 → P5
           ↓
           P6
P3 (独立)
```

## 11. 成果物

- `src/shard/scan.rs::find_evict_pos` の改訂
- `src/shard/mod.rs::shift_visited_down_in_place` の改訂 (Phase 3)
- BMI2 経路を入れるなら `src/lib.rs` の `Cache` に `has_bmi2: bool` 追加 + `Shard::find_evict_pos`
  の dispatch (Phase 4 以降)
- 計測結果は別途 `docs/reports/2026-05-15-find-evict-pos-optimization-results.md` (この
  企画書と対) で記録
- index 更新 (`docs/reports/index.md`)

## 12. 不採用案 (記録)

- **`unvisited` 表現に切り替え**: 全体の `not` を 1 回減らせるが、on-hit ホットパスの
  `visited |= 1<<pos` も `unvisited &= !(1<<pos)` に切り替わり相殺、純 0。コードの
  読みやすさを優先して見送り。
- **戻り値を `u8` に変更**: 呼び出し側で再度 `as usize` するので net 改善小。`#[inline]`
  前提でないと逆効果の可能性もあり、本企画の費用対効果ラインを下回る。
- **`hand >= self.len` clamp の削除**: invariant 上 dead code 相当だが、API 契約 (debug
  assert は precondition のみで、retain や remove のリカバリで `hand` が clamp を頼って
  いる可能性) を厳密に検証する追加コストが見合わない。
