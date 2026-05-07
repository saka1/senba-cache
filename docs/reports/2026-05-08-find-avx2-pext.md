# 2026-05-08 — `find_avx2` PEXT/PDEP 採用案: inner unroll ×2 と needle 構築の畳み込み

- 親: `2026-05-08-find-avx2-frontier.md` (Tier-S/A/B 着手順、C2 で BMI2 PEXT を一旦棄却)
- 関連実装: `src/shard.rs` `find_avx2` / `needle_from_hash`、`src/lib.rs` (`Cache::new` で
  cpu features を採取する dispatch flag)
- 種別: **解析ノート (実測なし)**。前報 §C2 で「Zen 1/2 で PEXT 激遅、non-portable 前提
  踏むなら別議論」と棚上げした件を、運用前提が変わった (Zen 1/2 シェア低下) ことで
  解禁、机上検討まで進めたもの。

## 0. TL;DR

前報 §C2 で BMI2 PEXT を棄却した直接の理由は **AMD Zen 1/Zen 2 で PEXT/PDEP が
microcoded 化していて ~250 cy** という現実だった。Zen 1 (2017) / Zen 2 (2019) は
2026 時点で 7〜9 年落ち、Steam HW survey 上のシェアもひと桁台後半まで落ちている。
**runtime dispatch (CPUID family check) を 1 個足せば PEXT 経路を fast path 専用に
焼ける**ので、棚上げ理由はもう成立しない。本稿は解禁を前提に **PEXT/PDEP で hot
path に効く打ち手** を 5 系列、CPUID 検出戦略 3 案、既存 Tier-S/A/B との interaction
表まで整理する。

骨子:

- **P2 (PEXT + inner unroll ×2)** が本命。lane-mask への圧縮で unroll コードが直感的
  になり、Path A の load 並列化と Path B の BLSR ×1 を同時に取れる。机上で
  per_shard=64 / N_cand=0.88 帯で **−3〜−5 cy/scan**、ps=16 はほぼ neutral。
- **P3 (PDEP for `needle_from_hash`)** は依存関係なくクリーン採用候補。call ごと
  −2〜−3 cy。コードも 1 命令化できて読みやすくなる副次効果。
- 前報 **B1 (SoA tag split = 8-bit hash)** とは P2 が **排他**。SoA 化すると
  `cmpeq_epi8` が 1 bit/lane mask を直接出すので PEXT compression が無用になる。
  どちらを取るかは perf-gate + Twitter trace の感度で決める。

## 1. 前提の再確認: PEXT/PDEP latency の uarch 別実態

Agner Fog "The microarchitecture of Intel, AMD and VIA CPUs" + uops.info より:

| uarch | PEXT/PDEP latency | throughput | 採用判断 |
|---|---:|---:|---|
| Intel Haswell (2013) 〜 Tiger Lake | 3 cy | 1/cy | fast |
| Intel Alder Lake〜現行 | 3 cy | 1/cy | fast |
| AMD Zen 1 (2017) | ~250 cy | ~1/250 cy | **microcoded、避ける** |
| AMD Zen 2 (2019) | ~250 cy | ~1/250 cy | **microcoded、避ける** |
| AMD Zen 3 (2020) 〜 | 3 cy | 1/cy | fast |
| AMD Zen 4 / Zen 5 | 3 cy | 1/cy | fast |

実装上は **Zen 1/2 だけが特異点**。CPUID family ≤ 0x17 (Zen 1/+/2) と
family ≥ 0x19 (Zen 3 以降) の 1 ライン分岐で峻別できる。Intel Atom 系で将来別の
slow-PEXT 例外が出る可能性は残るが、現時点では未確認。

## 2. PEXT/PDEP applicable な最適化系列

### P1. mask compression (pair-mask → lane-mask)

`vpcmpeqw + vpmovmskb` の出力は 32-bit pair-mask (lane k で bit 2k と 2k+1 の両方が
立つ)。PEXT で 16-bit lane-mask に圧縮:

```rust
let lane_mask = _pext_u32(mask, 0x55555555) as u16;
```

inner loop:

```rust
while lane_mask != 0 {
    let lane = lane_mask.trailing_zeros() as usize;     // = lane (bit >> 1 不要)
    let tag = *(chunk_u16_ptr.add(lane));
    let off = (tag & ID_MASK) as usize;
    if (*entries_byte_ptr.add(off)).key.borrow() == key {
        return Some((i + lane, tag));
    }
    lane_mask = _blsr_u32(lane_mask as u32) as u16;     // BLSR ×1
}
```

cycle 損益 (fast PEXT 前提):

| 項目 | 現状 (BLSR ×2) | P1 (PEXT + BLSR ×1) | Δ |
|---|---:|---:|---:|
| chunk 入口 setup | 0 | +3 cy (PEXT) | +3 |
| Path B per cand | 2 cy | 1 cy | −1 |

**break-even は cand/chunk = 3**。per_shard=16 / N_cand≈0.7 では net +2.3 cy/scan で
loss、per_shard=64 / N_cand≈0.88 でも net +2.1 cy/scan で loss。**P1 単独は採用不可** —
前報 §C1 で同じ結論を出している (BLSR ×2 が均衡解)。PEXT に置き換えても本質は変わらない。

PEXT が効くのは **次の P2 と組み合わせる時**。

### P2. PEXT + inner unroll ×2 (本命)

前報 §3 A1 (inner unroll ×2) と P1 の lane-mask 化を組み合わせる。lane-mask 化のおかげ
で unroll 時の bit 算術が直感的になる:

```rust
let lane_mask = _pext_u32(mask, 0x55555555) as u16;
let mut lm = lane_mask;
while lm != 0 {
    let lane1 = lm.trailing_zeros() as usize;
    let lm2 = lm & (lm - 1);                            // lane1 cleared
    // tag1 / off1 / entries[id1] load 発行
    let tag1 = *(chunk_u16.add(lane1));
    let off1 = (tag1 & ID_MASK) as usize;
    let p1 = entries_byte_ptr.add(off1) as *const Entry<K, V>;

    if lm2 == 0 {
        // 単発
        if (*p1).key.borrow() == key { return Some((i + lane1, tag1)); }
        break;
    } else {
        // 2 並列
        let lane2 = lm2.trailing_zeros() as usize;
        let tag2 = *(chunk_u16.add(lane2));
        let off2 = (tag2 & ID_MASK) as usize;
        let p2 = entries_byte_ptr.add(off2) as *const Entry<K, V>;
        // ↑ p1 / p2 の load が両方 in-flight (独立 dep chain)
        if (*p1).key.borrow() == key { return Some((i + lane1, tag1)); }
        if (*p2).key.borrow() == key { return Some((i + lane2, tag2)); }
        lm = lm2 & (lm2 - 1);
    }
}
```

**dep chain 図**:

- 旧: `[tag1 load 5] → [cmp1 6] → [tag2 load 5] → [cmp2 6] → ...` 直列、22 cy / 2 cand
- P2: `[tag1 load 5][tag2 load 5]` 並列、`[cmp1 6][cmp2 6]` 並列 → 11 cy / 2 cand

cycle 損益見積:

| 項目 | 現状 (post c-hoist) | P2 (PEXT + unroll ×2) | Δ |
|---|---:|---:|---:|
| chunk setup | 0 | +3 cy (PEXT) | +3 |
| Path A per pair | 2 × 14 = 28 cy | ~17 cy (load 並列) | **−11 / pair** |
| Path B per cand | 2 cy | 1 cy | −1 |

per_shard=64 / N_cand=0.88 / N_chunks_with_match≈0.7 で:

```text
Δscan ≈ +3 × 0.7  (PEXT setup, chunk ヒット時のみ)
      + (-11) × (0.88 / 2)  (pair pipeline 利得)
      + (-1) × 0.88  (BLSR 削減)
      ≈ +2.1 - 4.84 - 0.88 = -3.6 cy/scan ≈ -0.9 ns/op @ 4 GHz
```

per_shard=16 / N_cand=0.69 では unroll 入口に到達する確率が低くて利得が薄まる。
**per_shard=64 帯（j8 退行最大セル）に直接効く**性質的に良い手。`j8-c-hoist` §10.5
で OQ-3 を deprioritize したのは Path B 視点の議論で、P2 は **Path A の load 並列化**
として再評価しているので結論が違ってよい。

### P3. PDEP で `needle_from_hash` を 1 命令化

現状の hash spread は手書き bit shuffle:

```rust
pub(crate) fn needle_from_hash(hash: u64) -> u16 {
    let h = (hash >> 56) as u8;
    let s = Self::ID_SHIFT;
    let spread = if s >= 8 {
        h as u16
    } else {
        let low_mask: u8 = ((1u32 << s) - 1) as u8;
        let low = (h & low_mask) as u16;
        let high = ((h & !low_mask) as u16) << 6;
        low | high
    };
    LIVE | spread
}
```

PDEP で 1 命令化:

```rust
pub(crate) fn needle_from_hash(hash: u64) -> u16 {
    let h = (hash >> 56) as u32;
    // SAFETY: BMI2 は Cache::new の cpu_features 取得時にチェック済み。
    let spread = unsafe { _pdep_u32(h, Self::HASH_MASK as u32) } as u16;
    LIVE | spread
}
```

`HASH_MASK` は `0x3FFF & !ID_MASK` で id 領域を飛び越えた 8 bit 散在パターン。PDEP は
mask の立っている bit 位置に source の low 8 bit を順に置いてくれる — これがまさに
現状の bit spread の定義そのもの。

cycle 損益: 現状 ~5 op (and / shift / and / shift / or) → PDEP 1 op (3 cy)。**1 call
あたり ~2-3 cy 短縮**、コードも明確。get / insert / contains / peek 全てが call site
ごとに 1 回通る経路なので地味に効く。Slot64 のように `s >= 8` で分岐の片側に倒れる
monomorph では効果薄だが、それでも asm 上は分岐が消える方向で codegen が綺麗になる
(`SlotSize` ごとの分岐 vs PDEP に統一)。

ただし `is_x86_feature_detected!("bmi2")` を持っている前提が要る。fast PEXT を持って
いるが BMI2 を持っていない CPU は事実上存在しないので (BMI2 は PEXT/PDEP/SHLX/...
のセット)、cpu_features に `has_bmi2` を 1 bit 足すか、`has_fast_pext` で代用する。

### P4. PEXT-compressed indices + vpgather (賭け、u32 K 限定)

複数 candidate のキーを vpgather で並列 fetch する案:

```rust
// 1) tags vector から id × S::SIZE を一括計算
let id_offsets = _mm256_and_si256(tags_v, id_mask_v);  // 16 lane × u16 byte offset

// 2) lane-mask を PEXT で得る
let lane_mask = _pext_u32(mask, 0x55555555) as u16;

// 3) 一致した lane の id_offset だけを compact (vpermd + 256-entry shuffle table)
let perm = _mm256_load_si256(SHUFFLE_TABLE.as_ptr().add(lane_mask as usize));
let compressed_offsets = _mm256_permutevar8x32_epi32(id_offsets_lo, perm);

// 4) vpgatherdd で entries[id].key を一気に load (u32 K 限定)
let keys = _mm256_i32gather_epi32(entries_ptr, compressed_offsets, 1);

// 5) needle key と 8 lane 並列比較
let cmp = _mm256_cmpeq_epi32(keys, needle_key_v);
let final_mask = _mm256_movemask_epi8(cmp) as u32;
```

制約・コスト:

- **K = u32 限定** (vpgatherdd は 32-bit load)、u64 K は vpgatherqq だが Skylake で激遅
- vpgather 自体 ~10-20 cy on Skylake、Ice Lake 以降は改善。実機依存性が高い
- shuffle table が 2^16 × 32 byte = **2 MB** で L1 から外れる。実用は 2^8 × 8 byte の
  half-decode 二段構成
- 1 chunk あたり candidate が 2 個未満の場合 (per_shard=16 / 32 帯) は overkill

実装難度は高い。勝ち筋は **false-match 多発 + u32 K + per_shard=64** とかなり狭い。
Tier-B 寄り、`senba::Cache` 公開 surface で扱うかは要設計判断。

### P5. PEXT/PDEP の小ネタ (棄却含む)

| ID | 内容 | 評価 |
|---|---|---|
| P5.1 | `id_of(tag) = pext(tag, ID_MASK)` | 現状 `(tag & MASK) >> SHIFT` が 2 op 2 cy、PEXT は 1 op 3 cy。**現状の方が速い**。skip |
| P5.2 | BZHI で len を超える lane を mask off | tags storage 側で EMPTY pad してるので意味なし |
| P5.3 | SHLX/SHRX で flag-free shift | 現状 LLVM が flag 利用してない、改善なし |
| P5.4 | RORX で tag rotate | 用途なし |
| P5.5 | MULX | 用途なし |
| P5.6 | PEXT で 2 chunk 分の mask を u64 に詰めて一気に iter | OoO で勝手にやってる、明示の利得は小 |

実質 hot path に効くのは **P2 (PEXT + unroll ×2)** と **P3 (PDEP needle 構築)** の
2 軸のみ。P1 単独は loss、P4 は要 K 制約。

## 3. ランタイム検出戦略

PEXT 採用するなら Zen 1/2 への safety net が要る。**3 つの実現方式**:

### 方式 A: CPUID vendor/family check (推奨)

V8 / TurboFan などの主要 JIT で使われている方法。**起動時 1 回**の `__cpuid` で
判定し、結果を `Cache` 構築時に flag に焼く。

```rust
fn pext_is_fast() -> bool {
    if !is_x86_feature_detected!("bmi2") {
        return false;
    }
    let v0 = unsafe { __cpuid(0) };
    let vendor: [u32; 3] = [v0.ebx, v0.edx, v0.ecx];
    // "AuthenticAMD" = [0x68747541, 0x69746e65, 0x444d4163]
    let is_amd = vendor == [0x6874_7541, 0x6974_6e65, 0x444d_4163];
    if !is_amd {
        // Intel + Hygon + その他: Haswell 系列以降は fast PEXT
        return true;
    }
    // AMD: family 0x19 (Zen 3) 以降が fast、それ以前 (Zen 1/+/2 = 0x17) は slow
    let v1 = unsafe { __cpuid(1) };
    let base_family = (v1.eax >> 8) & 0xf;
    let ext_family = (v1.eax >> 20) & 0xff;
    let family = base_family + ext_family;
    family >= 0x19
}
```

利点: 起動コストゼロ、判定確実、追加 dependency なし。
弱点: 新しい AMD family 番号は前向きに通る (Zen 5 = 0x1A も `>= 0x19` でカバー) が、
将来 Intel Atom 系で別の slow-PEXT 例外が出てきたら追加判定が要る。

### 方式 B: マイクロベンチで実測

起動時に PEXT を数千回回して latency を測り、閾値超えたら slow flag を立てる:

```rust
fn pext_microbench() -> bool {
    use std::arch::x86_64::*;
    let start = std::time::Instant::now();
    let mut acc = 0xdead_beef_cafe_babeu64;
    for _ in 0..10_000 {
        acc = unsafe { _pext_u64(acc | 1, 0x5555_5555_5555_5555) };
        std::hint::black_box(acc);
    }
    let elapsed = start.elapsed().as_nanos();
    elapsed < 10_000 * 5  // ~5 ns/iter (= ~15 cy at 3 GHz) なら fast
}
```

利点: 確実に正しい判定、未知 uarch にも自動適応。
弱点: 起動コスト ~50 μs、library として若干お行儀悪い。`OnceLock` でキャッシュすれば
call 単位の影響なし。

### 方式 C: feature flag で opt-in

```toml
[features]
fast-pext = []
```

利点: コードが単純、ユーザの意思を尊重。
弱点: ユーザに知識を要求、デフォルト値が library として弱い (採用率が低くなりがち、
特にコピペで使われるとデフォルトのまま放置される)。

---

3 つのうち **方式 A (CPUID family check)** が最もコスパ良い。`Cache::new` は既に
`is_x86_feature_detected!("avx2")` で `has_avx2_bmi1` を採取しているので、そこに
`has_fast_pext` を 1 bit 足すだけ。dispatch 拡張も find 単位で 1 行:

```rust
fn find<Q>(&self, key: &Q, needle: u16, cpu: u8) -> Option<(usize, u16)>
where K: Borrow<Q>, Q: Eq + ?Sized,
{
    #[cfg(target_arch = "x86_64")]
    {
        if cpu & FAST_PEXT != 0 { return unsafe { self.find_avx2_pext(key, needle) }; }
        if cpu & AVX2_BMI1 != 0 { return unsafe { self.find_avx2(key, needle) }; }
    }
    let _ = cpu;
    self.find_scalar(key, needle)
}
```

`bool` を 2 個持ち回すより `u8` の bitset で 1 個持ち回す方がレジスタ・コード両面で
軽い。

## 4. 既存 Tier-S/A/B との相互作用

| 既存提案 (`find-avx2-frontier.md`) | PEXT 系との関係 |
|---|---|
| **S1** (find returns (pos, tag)) | 直交。PEXT 経路でも同じ tuple で return |
| **S2** (entry_ptr raw arith) | 直交。PEXT 関係なし |
| **S3** (Shard field reorder) | 直交 |
| **S4** (Slot16 broadcast hoist) | 直交 |
| **A1** (inner unroll ×2) | **本稿 P2 と同じこと**を BLSR ×4 で書くか PEXT + BLSR ×2 で書くかの違い。PEXT 経路の方が clean、A1 は PEXT 無し版として比較対象に残す |
| **A2** (4-chunk specialization) | 直交、組み合わせ可 |
| **B1** (SoA tag split = 8-bit hash) | **排他**。SoA 化で `cmpeq_epi8` が 1 bit/lane mask を直接出すので PEXT compression は無用 |

P2 と B1 の二者択一は **false-match 倍増 (1/256 → 1/128) を許容しても chunk 数が
半減 (4→2) して勝てるか** という問題に帰着する。理論で詰めきれず、prototype 2 本
実装して測るのが早い。

## 5. 推奨着手順 (PEXT 込み版)

`find-avx2-frontier.md` §6 の優先順を更新する形で:

| 優先度 | 項目 | 期待効果 (机上) | リスク |
|---|---|---|---|
| 1 | S1 / S2 / S3 (前報) | hit path -5〜-7 cy | 低 |
| 2 | **P3** PDEP for `needle_from_hash` | call ごと −2〜−3 cy | **極小** (1 関数の置換、cpu_features に 1 bit 追加のみ) |
| 3 | A2 4-chunk specialization | per_shard=64 で branch -3〜-6 cy/scan | 低 |
| 4 | **P2** PEXT + inner unroll ×2 | per_shard=64 で −3〜−5 cy/scan、ps=16 はほぼ neutral | 中 (runtime dispatch 拡張 + コード +50 行) |
| 5 (排他) | B1 SoA tag split | P2 と二者択一、要 prototype 比較 | 高 |
| 6 (賭け) | P4 PEXT + vpgather | u32 K 限定、shuffle table 設計要 | 高 |

P3 は依存関係なくクリーン採用候補。S1/S2 と一緒の PR でも、cpu_features 拡張だけ
別 PR でも筋が通る。

P2 は S1/S2 commit して perf-gate baseline を取り直してから、**runtime dispatch 拡張
(CPUID family check)** + **PEXT 専用 find 実装** を別 PR で入れて Twitter trace で
per_shard=64 帯の退行が縮むかを見る、という流れ。

## 6. オープン課題

| # | 課題 |
|---|---|
| OQ-1 | 方式 A (CPUID family check) で Intel Atom 系 / VIA / Zhaoxin 等の slow-PEXT 例外が出ないか。出た場合の更新ポリシー (allow-list 形式 vs deny-list 形式) を決める |
| OQ-2 | P2 の dep chain 並列化が LLVM で素直に出るか。cmp1/cmp2 が直列に出る場合は `core::hint::black_box` で reorder block を入れるか、最悪 `asm!` 直書きで並列性を強制する |
| OQ-3 | P3 の PDEP 化が `s == 8` の境界 (`SlotSize::Slot256` 等の HASH_MASK が連続帯になるレイアウト) で oracle テスト一致を保つか。`bit_layout_exclusivity` 系テストの拡張が要る |
| OQ-4 | P2 vs B1 の prototype 比較計画。両方を別ブランチで実装してから perf-gate + Twitter cluster016/018 sweep でクロス比較する手順を別 design doc 化 |
| OQ-5 | senba は publishable surface なので、`Cache::new` に `unsafe { _pext_u32(...) }` を踏む経路が入ると downstream user が `target-cpu=native` でビルドしたとき以外でも動くことを debug_assert で保証する。`is_x86_feature_detected!` は std 依存なので `no_std` 化を将来見据えるなら raw cpuid に統一する選択肢もある |

## 7. まとめ

前報 §C2 で BMI2 PEXT を「Zen 1/2 で激遅」を理由に棚上げしたが、**Zen 1/2 のシェアが
低下した 2026 時点では、CPUID family check 1 個で fast path 専用に焼ける**。これに
よって解禁される打ち手のうち、

- **P3 (PDEP needle 構築)** は依存関係ゼロ、call ごと −2〜−3 cy のクリーン採用
- **P2 (PEXT + inner unroll ×2)** は per_shard=64 帯 (= j8 退行最大セル) を −3〜−5 cy/scan
  で削れる本命

の 2 軸が現実的。P3 は前報 Tier-S と同じ PR / 別 PR どちらでも入れやすく、P2 は
S1/S2 commit 後の perf-gate baseline を踏んでから、CPUID 拡張 + PEXT 実装 + Twitter
sweep で慎重に検証するのが順序的に正しい。前報 B1 (SoA tag split) との二者択一は
prototype 2 本作って測るしかなく、それは更に先のフェーズで扱う。
