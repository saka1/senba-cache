# 2026-05-08 — `find_avx2` 詰め残し分析: caller 縫い目の tag 再 load と関連 micro-opt

- 親:
  - `2026-05-06-j8-candidate-loop-analysis.md` (§8.1〜§8.5 候補一覧 / Path A・B 定式化)
  - `2026-05-06-j8-c-hoist.md` (§10.5 「inner 単独最適化は打ち止め、次は load latency hide」)
  - `2026-05-07-aligned-tags-load.md` (32B align で cache-line split 解消)
- 関連実装: `src/shard.rs` `find_avx2` / `find_scalar` / `entry_ptr` / `Shard` 構造体
- 種別: **解析ノート (実測なし)**。生 asm の読解と机上見積もりまで。実装・ベンチは未着手で
  本稿の射程外。

## 0. TL;DR

`j8-c-hoist` で「inner 単独最適化は打ち止め」と結論したが、これは **`find_avx2` の関数
内側だけを見た結論**。`get` / `insert` 置換 path 等の **caller との縫い目** に
asm レベルで未回収の冗長があり、最大は次の 2 点:

- **S1**: `find` が `Option<usize>` (= pos のみ) を返すため、caller が `tags[pos]` を
  もう一度 load して id を再抽出している。inner で 1 op (`andl $ID_MASK`) で済んでいた
  byte offset 計算が、return の境界で **load + shr + and + shl の 4 op** に膨らみ直す。
- **S2**: `entry_ptr(id)` が `self.entries[id]` (bounds-checked) を経由するため、LLVM が
  `((tag & ID_MASK) >> ID_SHIFT) << ID_SHIFT == tag & ID_MASK` を畳めない。raw pointer
  算術にすれば `find_avx2` 内側と同じ c-hoist が caller でも効く。

机上見積もりで hit path から **−5〜−7 cy / hit** (= load 1 個 + shift round-trip 4 op)。
hit 率 ~60% 帯なら −3〜−5 cy/op 相当で、perf-gate と Twitter trace の両方で +3〜+5%
規模の改善が出る方向。本稿はその根拠を asm で示すところまで。

これに加えて **Slot16 monomorph 限定で `vpbroadcastd .LCPI` の per-chunk 再構築**
(他 monomorph では prologue で hoist 済み) と、**`Shard` 構造体の `len` フィールドが
2nd cache line に落ちている** 点も観測した。

中規模 (要実測) で残るのは inner unroll ×2 (旧 OQ-3 を Path A 視点で再評価) と
`limit == MAX_PER_SHARD` での 4-chunk specialization。構造改修 (賭け) 軸として
SoA tag split (8-bit hash 配列で 32 lane/ymm) も棚に置く。

## 1. asm 観察: caller 縫い目の冗長

`cargo rustc -p senba-research --release --bench sieve_cache_perf -- --emit asm` で
出力された asm を見る。`Slot32, u64, u64` モノモーフィゼーションの inner loop
(LBB172_15) 自体は `j8-c-hoist` §10.1 の理想形に到達済み:

```asm
LBB172_15:                                   ; Path A 14 cy / Path B 2 cy
  tzcntl  %r10d, %edx                        ; bit
  movzwl  (%r11,%rdx), %ebx                  ; tag load (5 cy)
  andl    $2016, %ebx                        ; tag & ID_MASK = id × 32 (c-hoist 効)
  cmpq    %rsi, (%rdi,%rbx)                  ; entries[id].key cmp + load
  je      success
  blsrl   %r10d, %edx                        ; BLSR ×1
  blsrl   %edx, %r10d                        ; BLSR ×2
  jne     LBB172_15
```

ここまでは既知。問題はこの `je success` の合流先、つまり `find_avx2` を inline した
caller が return 直後に何をやっているか。`insert` 置換 branch (LBB171_5) を読むと:

```asm
movzwl  (%rax,%rdx,2), %edi                  ; ★ tag を再 load (5 cy)
shrl    $5, %edi                             ; tag >> ID_SHIFT
andl    $63, %edi                            ; & 63 → id (0..63)
... cmpq %rdi, %rsi; jbe LBB171_37 ...       ; entries.len() bounds check
shll    $5, %edi                             ; id << ID_SHIFT (= byte offset へ戻す)
movq    %rsi, 8(%rcx,%rdi)                   ; entries[id].value 書き込み
orb     $64, 1(%rax,%rdx,2)                  ; tags[pos] |= VISITED (上位 byte だけ RMW)
```

inner で `tag & 0x07E0` の 1 命令で持っていた byte offset が、合流点で 4 命令に
膨らみ直している。同じパターンは Slot16/u32 (LBB51_24)、Slot64/String (LBB49_28
近傍) でも確認:

| monomorph | inner の byte offset 構成 | 合流点の byte offset 構成 |
|---|---|---|
| Slot16 / u32 (LBB51) | `andl $1008, %r8d` (1 op) | `movzwl + shrl $4 + andl $63 + shll $4` (4 op) |
| Slot32 / u64 (LBB172) | `andl $2016, %ebx` (1 op) | `movzwl + shrl $5 + andl $63 + shll $5` (4 op) |
| Slot64 / String (LBB49) | (同上) | (同上) |

合流点が膨らむ機序は 2 段階:

### 1.1 (S1 部分) tag が return 境界で捨てられている

`find_avx2` の返り型は `Option<usize>` (= pos のみ)。inner loop でレジスタにあった
tag (`%ebx` / `%r8d`) は cmp 一致直後の `je success` 経由で **success 先のラベル
(LBB51_24 や LBB171_5) に到達した時点で** SSA 名を失う。LBB51_24 に至る前駆は
SIMD 一致 path (LBB51_13 系) と scalar 一致 path (LBB51_19 系) の 2 つで、それぞれ
tag を別レジスタに持っている。LLVM が合流点で SSA 名を統一できず、merge した形で
caller 側に渡せなかった結果、**合流点で `movzwl (mem)` を再発行**している。

### 1.2 (S2 部分) `entry_ptr` の bounds check が shift 往復を強制

合流点に tag を渡せたとして、その先の `id_of` → `entry_ptr` 経路はこうなる:

```rust
let id = ((tag & Self::ID_MASK) >> Self::ID_SHIFT) as usize;   // 0..63
self.entries[id].as_ptr()                                       // bounds check
```

`((tag & ID_MASK) >> ID_SHIFT) << ID_SHIFT` は代数的に `tag & ID_MASK` と等価で、
LLVM の InstCombine が畳めるはずの式。それが畳まれない直接の原因は `self.entries[id]`
が **unscaled な id (0..63) を bounds check の引数として要求する** こと。
indexing が `(id << ID_SHIFT)` バイト先のメモリを指す形に変換される段階では bounds
check はもう「id の数値そのものを cmp する」形で固定化済みで、後段の shift とは
畳めない。

`find_avx2` 内では:

```rust
let id_bytes = (tag & id_mask_u32) as usize;
let entry_ptr = entries_byte_ptr.add(id_bytes) as *const Entry<K, V>;
```

と raw pointer 算術で書いているから bounds check が無く、c-hoist がそのまま効いて
1 op (`andl $0x07E0, ebx`) になっていた。caller 側が同じ恩恵を受け取れていないのが
S1+S2 の合算した姿。

## 2. 提案する 4 つの未着手最適化 (S 系)

### S1. `find` を `Option<(pos, tag)>` 返すよう変更

```rust
fn find<Q>(&self, key: &Q, needle: u16, has_avx2_bmi1: bool) -> Option<(usize, u16)>
where K: Borrow<Q>, Q: Eq + ?Sized,
{
    #[cfg(target_arch = "x86_64")]
    if has_avx2_bmi1 {
        return unsafe { self.find_avx2(key, needle) };
    }
    let _ = has_avx2_bmi1;
    self.find_scalar(key, needle)
}

#[target_feature(enable = "avx2,bmi1")]
unsafe fn find_avx2<Q>(...) -> Option<(usize, u16)> { ... return Some((i + lane, tag as u16)); }
fn find_scalar<Q>(...) -> Option<(usize, u16)>     { ... return Some((i, t)); }
```

caller:

```rust
let (pos, tag) = self.find(...)?;
self.tags[pos] |= VISITED;       // RMW (ここは残る)
let id = Self::id_of(tag);       // ← レジスタの tag から計算、再 load なし
```

`tags[pos] |= VISITED` の RMW は残る (semantic に必要) が、これは `or [mem], imm`
1 命令で別 dep chain。**消えるのは「id を取り出すための tag 再 load + その下流」**で、
下記 S2 と組み合わせると合流点 4 op が 1 op (`andl $ID_MASK`) まで縮む。

期待効果: hit path から load 1 個 (≈5 cy) + shift round-trip (3 op ≈ 3 cy) ≈ **−7〜−8 cy**
/ hit。`get` (純粋 hit) と `insert` 置換 branch の両方に効く。

修正コスト: `find_scalar` / `find_avx2` の return 値を tuple 化、`find` でラップ、
caller 数箇所 (`get`, `get_mut`, `peek`, `peek_mut`, `peek_key_value`,
`get_key_value`, `get_or_insert_with` の hit branch、`insert` 置換 branch) の
パターンマッチを更新するだけ。1 ファイル diff、~50 行程度。

### S2. `entry_ptr` を bounds-check 経由から raw pointer 算術に

```rust
// 現状
pub(crate) fn entry_ptr(&self, id: usize) -> *const Entry<K, V> {
    let _: () = Self::_SIZE_OK;
    let _: () = Self::_STORAGE_SIZE_OK;
    self.entries[id].as_ptr() as *const Entry<K, V>      // bounds check
}

// 提案
pub(crate) fn entry_ptr(&self, id: usize) -> *const Entry<K, V> {
    let _: () = Self::_SIZE_OK;
    let _: () = Self::_STORAGE_SIZE_OK;
    // SAFETY: id は live tag の id_of から得るため id < capacity.
    // 全 caller (find / get / insert / drop / clone / retain / clear / remove) で
    // 同じ不変条件を満たすよう既に書かれている — bounds check は実質ノーオプ。
    unsafe { self.entries.as_ptr().add(id) as *const Entry<K, V> }
}
```

`entry_ptr_mut` も同様。bounds check が消えると LLVM の GEP 経路で
`entries.as_ptr() + id * sizeof(Storage)` = `entries.as_ptr() + (id << ID_SHIFT)`
が見えるので、`((tag & ID_MASK) >> ID_SHIFT) * sizeof(Storage)` の shr/shl が
キャンセルされて `entries.as_ptr() + (tag & ID_MASK)` まで畳まる…はず。

仮説の弱点: LLVM の GEP 化と shift 簡約の順序によっては畳めない可能性が残る。
畳まれない場合は **A3 (tag-direct helper)** にフォールバック:

```rust
#[inline(always)]
unsafe fn entry_ptr_from_tag(&self, tag: u16) -> *const Entry<K, V> {
    let off = (tag & Self::ID_MASK) as usize;   // 既に byte offset
    (self.entries.as_ptr() as *const u8).add(off) as *const Entry<K, V>
}
```

を hot path 専用に追加。caller は `id_of` を経ずに直接 entry に到達できる。
`id_of` を本当に必要とするのは `find_evict_pos` / `remove` の id 比較
(`Self::id_of(t) == max_id` 等) 系のみ。

期待効果: S1 と合算で hit path **−5〜−7 cy**。S1 単独でも tag 再 load は消えるが、
shift round-trip 3 op が残る。S2 を組み合わせて初めて asm が `find_avx2` 内側並みに
縮む。

### S3. `Shard` 構造体のフィールド並び替え

現状 (Rust の `repr(Rust)`、u64 align 推定):

```text
0..8     capacity
8..32    tags     (Vec<TagsChunk> = 24 B: ptr / cap / len)
32..56   entries  (Vec = 24 B)
56..64   hand
64..72   len      ← ★ 2nd cache line
72..104  hits / misses / insertions / evictions
```

`find_avx2` が `&self` から触るのは **`tags.as_ptr()` (offset 8)、`entries.as_ptr()`
(offset 32)、`len` (offset 64)** の 3 箇所。`len` だけ 2nd cache line にあり、cold な
`Shard` を読むたびに 2 line 引かれる。Twitter trace のように shard 数が多くて per-shard
の access frequency が薄い workload で命中率が下がる方向。

並び替え案:

```rust
pub(crate) struct Shard<K, V, S: SlotSize> {
    pub(crate) tags: AlignedTags,        // 0..24
    pub(crate) entries: Vec<...>,        // 24..48
    pub(crate) len: usize,               // 48..56  ← hot 3 つで 1 line に収まる
    pub(crate) capacity: usize,          // 56..64
    pub(crate) hand: usize,              // 64..72  (insert / evict 専用)
    pub(crate) hits: u64,                // 72..
    pub(crate) misses: u64,
    pub(crate) insertions: u64,
    pub(crate) evictions: u64,
}
```

期待効果: perf-gate (steady-state、self は L1 hit) では出にくい。Twitter trace の
**SHARDS が多くて shard hop が頻繁な cell** (例: `c8-vs-moka-thread-sweep` の
SHARDS=256 帯) ほど効くはず。具体数字は実測必須。

副作用: `Shard::new` の初期化と `Drop` / `Clone` の field 列挙、テストの順序依存
箇所 (もしあれば) に diff が出るが、本質的にはコード変更は宣言部だけ。

### S4. Slot16 monomorph の `vpbroadcastd .LCPI` を毎 chunk 再構築している

asm を比較すると monomorph で扱いが分かれている:

```asm
; Slot16 / u32 (LBB51_11) — 毎 chunk で SCAN_MASK broadcast 再構築
LBB51_11:
  vpbroadcastd  .LCPI51_1(%rip), %ymm1       ; ★ ループ内で毎回
  vpand         (%r14,%rcx,2), %ymm1, %ymm1
  vpcmpeqw      %ymm0, %ymm1, %ymm1

; Slot32 / u64 (LBB172_13) — prologue で hoist 済み (%ymm1 を 16 chunk 走らせる間維持)
LBB172_13:
  vpand         (%r8,%r9,2), %ymm1, %ymm2
  vpcmpeqw      %ymm0, %ymm2, %ymm2
```

source は両方 `let mask_v = _mm256_set1_epi16(...)` を loop 外で定義しているので
**Rust 側の差ではなく LLVM の reg-alloc 判断**。Slot16 の周辺で register pressure が
高まっている (周辺コードの使う ymm が多い、もしくは prologue/epilogue の cross-bb
reg-alloc が悲観的になっている) のが原因と推定される。

期待効果: `vpbroadcastd m32, ymm` は 1 uop / 3-4 cy load latency (memory operand
fold で実質 0 frontend 帯)。明示的な perf へのインパクトは小だが、Slot16 monomorph
だけ chunk あたり +1 op 余計に走らせている事実は perf-gate `insert_u32_slot16`
シナリオに出ているはず。

打ち手は仮説段階:

- 周辺 (insert / get) の `#[inline]` 設定を変えて caller の register pressure を
  動かす (`inline-design-cache-vs-inner` の知見的に作用が読みやすい軸)
- mask 定数を `Cache` レベルの `static` に置いて register hint を逆向きに与える
- `find_avx2` の中で `Self::SCAN_MASK` を `let` で受けてから broadcast 化する書き方を
  変えてみる

これは原因特定が先の OQ。実装着手前に reg-alloc 結果を 1 度ベンチで確認する。

## 3. 中規模 (Tier A)、要実測

### A1. inner loop unroll ×2 — Path A 視点での OQ-3 再評価

`j8-c-hoist` §10.5 で OQ-3 (inner unroll) を **Path B 視点** (= 2 cy しか残らないので
unroll 利得は 2 cy/iter まで) で deprioritize した。本稿は **Path A (= 14 cy)** が
load latency dominated で、2 candidate の load chain を別レジスタに pipeline すれば
load port を使い切って throughput が 2× に近づく可能性、と読み直す。

```rust
while mask != 0 {
    let bit1 = mask.trailing_zeros() as usize;
    let tag1 = *(chunk_byte_ptr.add(bit1) as *const u16);
    let off1 = (tag1 & ID_MASK) as usize;
    let p1   = entries_byte_ptr.add(off1) as *const Entry<K, V>;
    // tag1 → off1 → entry load を発行
    let m1 = mask & mask.wrapping_sub(1);
    let m1 = m1   & m1.wrapping_sub(1);
    if m1 == 0 {
        // 単発、現状ロジックと同じ
        if (*p1).key.borrow() == key { return Some((i + (bit1 >> 1), tag1)); }
        mask = 0;
    } else {
        let bit2 = m1.trailing_zeros() as usize;
        let tag2 = *(chunk_byte_ptr.add(bit2) as *const u16);
        let off2 = (tag2 & ID_MASK) as usize;
        let p2   = entries_byte_ptr.add(off2) as *const Entry<K, V>;
        // tag2 → off2 → entry load も並列発行 (p1 と独立な dep chain)
        let k1 = (*p1).key.borrow();
        let k2 = (*p2).key.borrow();
        if k1 == key { return Some((i + (bit1 >> 1), tag1)); }
        if k2 == key { return Some((i + (bit2 >> 1), tag2)); }
        let m2 = m1 & m1.wrapping_sub(1);
        let m2 = m2 & m2.wrapping_sub(1);
        mask = m2;
    }
}
```

dep chain 図:
- 旧: `[tag1 load] → [cmp1] → [tag2 load] → [cmp2] → ...` (直列、L1 5 cy ずつ)
- 新: `[tag1 load][tag2 load]` 並列、`[cmp1][cmp2]` 並列

per_shard=64 / N_cand≈0.88 帯 (= `j8-candidate-loop-analysis` §6 の机上 underestimate
領域) で特に効くはず。per_shard=16 / N_cand≈0.69 では candidate が 1 個未満なので
unroll の入り口に到達する確率が低く、cold side で命令数 +α、hot side でほぼ無効化。

修正コスト: 中。inner loop が枝分かれする分コード量は +30 行程度。`asm!` の確認も要る。

### A2. `len == MAX_PER_SHARD` での 4-chunk specialization

`MAX_PER_SHARD = 64`、`LANE = 16` なので chunk は最大 4 枚。実 hot path で
`len == cap == MAX_PER_SHARD == 64` (全埋まり steady state) が頻出するなら、
chunk 間 branch を消した specialized path を `if len == MAX_PER_SHARD` で分岐させる:

```rust
if self.len == MAX_PER_SHARD {
    return self.find_avx2_full(key, needle);   // 4-chunk 全 unroll
}
// 一般 path (現状コード)
```

`find_avx2_full` は 4 chunk を `vpand`/`vpcmpeqw`/`vpmovmskb` で全部発行してから
mask を組み合わせる:

```rust
let v0 = _mm256_load_si256(p.add(0)  as *const __m256i);
let v1 = _mm256_load_si256(p.add(16) as *const __m256i);
let v2 = _mm256_load_si256(p.add(32) as *const __m256i);
let v3 = _mm256_load_si256(p.add(48) as *const __m256i);
let m0 = _mm256_movemask_epi8(_mm256_cmpeq_epi16(_mm256_and_si256(v0, mask_v), needle_v));
let m1 = _mm256_movemask_epi8(_mm256_cmpeq_epi16(_mm256_and_si256(v1, mask_v), needle_v));
let m2 = _mm256_movemask_epi8(_mm256_cmpeq_epi16(_mm256_and_si256(v2, mask_v), needle_v));
let m3 = _mm256_movemask_epi8(_mm256_cmpeq_epi16(_mm256_and_si256(v3, mask_v), needle_v));
// chunk 別に candidate 処理 (chunk 内の bit パターンは現状と同じ)
// が、chunk 間の testl + je がなくなる
```

期待効果: chunk 間 branch が 3 個減って ≈ −3〜−6 cy/scan (well-predicted 域なので
過大評価しない)。OoO は元々前後 chunk を overlap してくれているので、明示しても
利得が頭打ちになる可能性は高い。1 dispatch を入れることで cold path (`len < 64`) を
傷つけないかは要実測。

### A3. tag-direct entry pointer ヘルパー

S2 が LLVM に畳んでもらう前提で書いたが、畳めない場合のフォールバック。S1 の tag
を貰った caller が `id_of` を経ずに直接 entry を取る:

```rust
#[inline(always)]
unsafe fn entry_ptr_from_tag(&self, tag: u16) -> *const Entry<K, V> {
    let off = (tag & Self::ID_MASK) as usize;
    (self.entries.as_ptr() as *const u8).add(off) as *const Entry<K, V>
}
```

`get` / `peek` / `get_or_insert_with` の hit branch、`insert` 置換 branch から呼ぶ。
`Self::id_of` は `find_evict_pos` / `remove` (id を数値として比較する経路) のみが
継続使用。

### A4. VISITED set を `find_avx2` 内に押し込む案

cmp 一致直後に byte store で tags の上位 byte に VISITED bit を立て、return 後の
RMW を消す案。

```rust
// inner で cmp 一致直後:
*(chunk_byte_ptr.add(bit + 1) as *mut u8) |= 0x40;   // VISITED = 0x4000 = 上位 byte の bit 6
return Some((i + lane, tag | VISITED));
```

利点: caller 側の `tags[pos] |= VISITED` が消える (= memory dep chain 1 個削減)。
`peek` 系 (非 promoting) との分岐は引数で切り分け可。

弱点:
- `&self` を `&mut self` にするか `UnsafeCell` 経由にするかでコードが膨らむ。`get`
  系は元々 `&mut self` だが、`peek` / `peek_key_value` は `&self`、`contains` も
  `&self`。promote させたいかどうかで API 形状が分岐するので、`find_avx2` を
  `find_avx2<const PROMOTE: bool>` のように const generic で 2 版に分けるのが現実解。
- VISITED bit 競合がある並行 path (= c8 系列の議論) と semantics を合わせる必要が
  あって、`senba::Cache` (ST) 単独で先行投入できるか要検討。

優先度低め (ST 単独で見ると S1+S2 ほどクリーンに勝てない)。

## 4. 構造改修 (Tier B)、賭け

### B1. SoA tag split — 8-bit hash 配列 + 8-bit (id | visited) 配列

abseil SwissTable 風。`tags` を 2 つに分解:

- `tags_scan: Vec<u8>` = `LIVE bit + 7-bit hash`
- `tags_meta: Vec<u8>` = `6-bit id + 1-bit visited` (もしくは独立 visited 配列にして 6-bit id を packed nibble に)

SIMD scan が `_mm256_cmpeq_epi8` で **32 lane / ymm** = 2× throughput。chunk 数が
半分になり、per_shard=64 帯で 4 chunks → 2 chunks。

```rust
let v = _mm256_load_si256(tags_scan.add(i) as *const __m256i);
let cmp = _mm256_cmpeq_epi8(v, needle_byte);
let mask = _mm256_movemask_epi8(cmp) as u32;     // 1 bit = 1 lane (BLSR ×1 で進む)
```

トレードオフ:

- false-match 率 1/256 → 1/128 (7-bit hash)。Tier-S 効果と一部相殺。Path A の
  発火頻度が 2× になるが、それを Path A 短縮 (S1+S2) でカバーするのが筋。
- メモリは合計 2 byte/slot で同じ (1 byte × 2 配列)。
- 32B align は両配列で必要、AlignedTags の構造を 2 系統に複製する必要。
- 大改修。`needle_from_hash` / tag layout / `id_of` / VISITED bit 操作の全箇所が
  影響範囲。oracle テスト (`oracle_cache_match.rs`) を通せば semantic は守れるが、
  diff は数百行。
- BLSR ×1 で済む (旧 BLSR ×2 から −1)。Path B 1 cy に到達。

per_shard=64 で chunk が 4→2 に減る効果 + Path B の更なる短縮で **机上 −5〜−10 cy/scan**。
ただし false-match 倍増分の Path A 増分と相殺するので、勝つかどうかは実測まで分からない。

### B2. 32-bit 拡張 tag — 小さい K 限定で inner loop 自体を消す

`u32` / `u64` 等の小さい K では tag を 16-bit から 32-bit に拡張して上位 16-bit に
key の hash 16-bit (元の tag の 8-bit より 8 bit 多く) を載せる。SIMD は
`_mm256_cmpeq_epi32` で 8 lane 比較。一致したら **`entries[id].key` を読まずに即 hit
判定**。

- false match 率 ≈ 2^(-24) ≈ 6e-8 → 事実上ゼロ。Path A の `cmp + load` (=≈6 cy) が
  消える。
- chunk あたり lane 8 (1/2 throughput)。chunk 数が倍になるが、chunk 1 個あたりの
  inner loop 仕事量はゼロに近い。
- K のサイズ依存。`String` には適用不能、`u64` 以上は hash 16-bit しか乗せられない
  (それでも false match は 1/65536 で十分)。

SlotSize と独立な軸なので、`SlotSize` ジェネリックの一部に組み込むか、別 trait で
分岐する設計が要る。Tier-A の S1+S2 を入れた後の感度で判断するのが順序的に正しい。

### B3. vpgather 経由の batch key 比較

`_mm256_i32gather_epi32` で複数 candidate の `entries[id].key` を一気に gather →
SIMD 比較。u32 key 限定。gather 自体 10〜20 cy だが、独立の load chain が並列化
されるので false-match heavy で勝つ可能性。Skylake では gather が遅く、Ice Lake
以降はだいぶ改善。実機依存性が高くて perf-gate 通すのが面倒な軸。

### B4. tags + entries を 1 alloc に co-locate

別 `Vec` 2 個を 1 つの `Box<[u8]>` に詰めて offset で持つ。TLB は元々共通だが、
prefetcher が連続領域でより素直に動くケースがある。実利は限定的、構造改修は重い。

## 5. 棄却 / 効果薄 (参考)

| ID | 内容 | 評価 |
|---|---|---|
| C1 | `mask &= 0x55555555` で BLSR ×1 | 算: per_shard=16 で **損する方向** (chunk あたり setup +1 op vs N_cand≈0.69)。BLSR ×2 が均衡解 |
| C2 | BMI2 PEXT で lane 圧縮 | Zen 1 / 2 で PEXT 激遅 (~hundreds cy)。non-portable 前提を踏むなら別議論 |
| C3 | AVX-512 (vpcmpw + kmask) | non-portable、consumer CPU カバレッジ低 |
| C4 | prefetch entries[id] | steady-state で entries は L1 hit、prefetch は無意味。cold workload 限定 |
| C5 | vptest で no-match 早期検出 | 既に `testl + je` (movemask 後) が同等仕事 |
| C6 | NT load (vmovntdqa) | tags は WB memory、無効 |
| C7 | inner loop branchless (cmov) | 1 candidate / chunk が大半、mispredict 率は元々低い |

## 6. 着手順の提案

| 優先度 | 項目 | 期待効果 (机上) | リスク |
|---|---|---|---|
| 1 | **S1** `find -> Option<(pos, tag)>` | hit path tag reload 解消、−5 cy | 低 (caller 数箇所のパターンマッチ更新のみ) |
| 2 | **S2** `entry_ptr` raw arith | shift round-trip 解消、−2〜3 cy | 低 (unsafe 1 行 + SAFETY コメント) |
| 3 | **S3** `Shard` フィールド並び替え | cold-self で 1 line/call 削減 | ほぼゼロ |
| 4 | **S4** Slot16 broadcast hoist 確認 | 1 cy/chunk × ~4 = −4 cy (Slot16 限定) | 中 (LLVM reg-alloc 説得が要る、原因特定が先) |
| 5 | A2 4-chunk specialization | branch 削減 ≈ −3〜−6 cy/scan | 低 |
| 6 | A1 inner unroll ×2 (OQ-3 再評価) | per_shard=64 で大、ps=16 で小 | 中 (コード量 +30 行) |
| 7 | A3 tag-direct entry helper | S2 fallback として | 低 |
| 8 | B1 SoA tag split | per_shard=64 で chunk 半減、要実測 | 高 (大改修) |

S1 + S2 は **1 ファイル diff、合計 ~80 行程度**で書ける見込み。perf-gate (`mixed_u64`、
`get_heavy_u64`) と Twitter trace 両方で +3〜+5% は出る方向と踏んでいる。perf-gate
の `--save-baseline` で計測しつつ 1 個ずつ commit するのが安全。

## 7. オープン課題

| # | 課題 |
|---|---|
| OQ-1 | S2 の「bounds check 消せば LLVM が shift round-trip を畳む」仮説の確認。手段:
        `cargo asm` 比較で `((tag & MASK) >> SHIFT) << SHIFT` が `tag & MASK` まで
        畳まれるかを実装後に検証。畳まれなければ A3 (tag-direct helper) に切り替え |
| OQ-2 | S4 の reg-alloc 原因特定。Slot16 / Slot32 / Slot64 の prologue 比較で `ymm`
        使用本数を数える。`#[inline]` 操作で動くか確認 |
| OQ-3 | A1 の dep chain 分割が LLVM で素直に出るか。出ない場合 `asm!` 直書きが要るか |
| OQ-4 | B1 SoA split の false-match 倍増 (1/256→1/128) が S1+S2 の改善幅を相殺するか。
        実装前に静的に見積もる方法は限定的、prototype で 1 monomorph だけ作って測るのが
        早い |

## 8. まとめ

`find_avx2` は **関数内側だけ見れば最適に近い** が、`get` / `insert` 等の caller との
合流点で「tag が SSA 的に消える」「`entries[id]` の bounds check が shift 簡約を
ブロックする」という 2 つの独立な要因で **load 1 個 + shift 3 op** が hit ごとに
発火している。これは関数境界の API (return 型) と `entry_ptr` の bounds check を
触るだけで両方解消できる。

機械的な diff で書ける内容なので、Tier-S を先に commit して perf-gate と Twitter
trace で実測し、その上で Tier-A (unroll / specialization) → Tier-B (SoA split) と
順に踏むのが筋。`j8-c-hoist` §10.5 で「inner 単独最適化は打ち止め、次は load latency
hide」と書いた "hide" の最初のレイヤーは、prefetch でも chunk overlap でもなく、
**caller との縫い目で発生している不要な load を消すこと** であった、というのが
本稿の中心的な発見。
