# 2026-05-08 — `find_avx2` PDEP/PEXT 投入 → 両方 revert (教訓レポート)

- 親: `2026-05-08-find-avx2-pext.md` (机上で P2 + P3 を提案、`-3〜-5 cy/scan` 見積)
- 関連: `2026-05-08-find-avx2-frontier.md` §6 推奨着手順、`2026-05-08-find-avx2-caller-merge.md`
- 種別: **実装試行 + 実測 + revert + 教訓**。code は HEAD (`2c7160a`) に戻し、本稿のみ
  記録として残す。

## 0. TL;DR

前報 `find-avx2-pext.md` の **P3 (PDEP needle 構築)** と **P2 (PEXT で pair-mask →
lane-mask + inner unroll ×2)** を順に投入し、perf-gate 6 シナリオ + asm レビューで
評価した結果、**両方とも採用基準を満たさず revert** した。

- **P3**: 命令数 7 → 4、依存チェーン 5 → 3 cy 短縮の asm レベルでの利得は確かに
  ある。しかし perf-gate は 6 シナリオすべて criterion "within noise threshold"
  判定で、改善 3 / 悪化 2 / no change 1 の振れ方は **machine noise の典型レンジ**
  と区別がつかなかった。続く P2 の baseline 取り直しで同機械が ±5% 揺れることが
  判明したことから、最初の P3 単独評価で観測した −1.6〜−2.5% は noise floor 越え
  ぎりぎりの偶然と読むのが正しい。
- **P2**: 6 シナリオ中 3 シナリオで **+3.5〜+4.9% の regression** (criterion
  "regressed" 判定)。CLAUDE.md の 5% 閾値ぎりぎり下とはいえ、明確に net 後退。
  asm 確認で **(a) LLVM が `cmp1`/`cmp2` を across した load 並列化を出さなかった**、
  **(b) PEXT prelude を毎 chunk 払うが、per_shard=64 の cand 分布上 unroll ×2 の
  発火率がほぼゼロ**、の 2 点が原因と判明。

両方を revert し、`senba::Cache` 公開 surface には何も残さない。前報机上見積が
過大評価だった原因 (3 種) と、次に同種の最適化を企てるときに踏まえるべき手順を
本稿の §3 / §4 にまとめる。

## 1. P3 (PDEP needle 構築) の評価

### 1.1 実装

`needle_from_hash` を `find_avx2` 入口で 1× `pdep` に置き換え:

```rust
#[target_feature(enable = "bmi2")]
unsafe fn needle_from_hash_pdep(hash: u64) -> u16 {
    let h = (hash >> 56) as u32;
    let spread = _pdep_u32(h, Self::HASH_MASK as u32) as u16;
    LIVE | spread
}
```

`Shard::find` のシグネチャを `(needle: u16)` → `(hash: u64)` に変え、scalar 経路は
従来 `needle_from_hash` のまま、AVX2 経路だけ `_pdep_u32` で needle を作る形。

### 1.2 asm 比較 (Slot32, `HASH_MASK = 0x381f`)

PDEP 版 (4 命令、dep chain 3):
```asm
shr   rdi, 56
mov   eax, 14367      ; mask immediate
pdep  eax, edi, eax
or    eax, 32768      ; LIVE
```

スカラー版 (7 命令、dep chain 5):
```asm
shr   rdi, 56
mov   eax, edi
and   eax, 31
and   edi, -32
shl   edi, 6
add   eax, edi
add   eax, 32768
```

asm レベルでは確かに短くなっている。

### 1.3 perf-gate

`cargo bench -p senba-research --bench sieve_cache_perf -- --baseline before-p3`:

| シナリオ | change (time) | criterion 判定 |
|---|---:|---|
| insert_u64/384         | +0.31% | No change |
| mixed_u64/384          | +1.21% | within noise |
| insert_string/256      | −2.32% | within noise |
| insert_u32_slot16/384  | −1.56% | within noise |
| get_heavy_u64/384      | −2.46% | within noise |
| mixed_lowskew_u64/384  | +2.44% | within noise |

すべて "within noise threshold" 判定、net で見れば±5% 内に収まる。

### 1.4 なぜ asm 利得が perf に出てこないか

(1) **needle 構築は AVX2 load の影に隠れている**: `find_avx2` 入口は `vmovdqa`
    (4-5 cy) → `vpbroadcastw` (3 cy) → `vpand` (1 cy) → `vpcmpeqw` (1 cy) →
    `vpmovmskb` (3 cy) で 12-13 cy の vector 直列依存。一方 needle 構築は scalar
    ALU で並列発行され、port 競合が無ければ 7 op で throughput 2-3 cy に消化される。
    OoO がすでに hide していた分は PDEP 化しても見えない。

(2) **needle は call ごと 1 回、scan は chunk ごと**: per_shard=64 / N_chunks=4 / Path
    A 0.88 で full op cost ~70-80 cy。−2〜−3 cy は ~3% の theoretical 上限で、
    criterion default の noise floor (~3-5%) と同オーダー。

(3) **GPR → vector domain crossing が真の bottleneck**: `needle: u32 →
    vpbroadcastw` の domain crossing (Skylake 系で 3 cy 追加 latency) が critical
    path。PDEP の出口 GPR が broadcast に流れる依存は scalar 版でも PDEP 版でも
    同じで、命令数を減らしても critical path は変わらない。

### 1.5 後段 P2 baseline 取り直しでの判明

P3 commit 後に baseline `before-p2` を取り直すと、同シナリオが ±2-3% の絶対値で
揺れた (machine state 変動)。**最初の P3 単独評価で観測した −1.6〜−2.5% は、
その揺れ幅と区別できない**。「improvement に見えた偶然の上振れ」と解釈するのが
妥当で、前報 §P3 の "クリーン採用候補" 評価は時期尚早だった。

### 1.6 P3 単独でも cost が benefit を上回る

- 新規関数 `needle_from_hash_pdep` (`unsafe` + `target_feature`)
- `Shard::find` シグネチャ変更、callers 9 箇所書き換え
- 新規テスト 1 本
- Zen 1/2 で `pdep` microcoded → 公開 surface に「Zen 1/2 では遅い」例外
- `senba` (publishable) の attack surface が広がる

統計的に有意な改善が出ない以上、これらは pure cost。**revert 妥当**。

## 2. P2 (PEXT + inner unroll ×2) の評価

### 2.1 実装の構造

```rust
let pair_mask = _mm256_movemask_epi8(cmp) as u32;
let mut lm = _pext_u32(pair_mask, 0x5555_5555);  // pair-mask -> 16-bit lane-mask

while lm != 0 {
    let lane1 = lm.trailing_zeros() as usize;
    let lm2 = _blsr_u32(lm);
    let tag1 = ...; let p1 = entries_byte_ptr.add(off1) ...;
    if lm2 == 0 {
        if (*p1).key.borrow() == key { return ...; }
        break;
    }
    let lane2 = lm2.trailing_zeros() as usize;
    let tag2 = ...; let p2 = entries_byte_ptr.add(off2) ...;
    // 期待: p1 / p2 の load を OoO に並列発行させる
    if (*p1).key.borrow() == key { return ...; }
    if (*p2).key.borrow() == key { return ...; }
    lm = _blsr_u32(lm2);
}
```

### 2.2 perf-gate

`--baseline before-p2`:

| シナリオ | change | criterion 判定 |
|---|---:|---|
| insert_u64/384         | **+4.14%** | regressed |
| mixed_u64/384          | **+4.89%** | regressed |
| insert_string/256      | **+3.50%** | regressed |
| insert_u32_slot16/384  | −0.37% | No change |
| get_heavy_u64/384      | −0.47% | No change |
| mixed_lowskew_u64/384  | −2.88% | improved |

3 シナリオで明確な regression、1 シナリオで improvement、2 シナリオで no change。

### 2.3 失敗原因 1: LLVM が cmp 越しの load 並列化を出さなかった

bench binary monomorph (`Shard<u64, u64, Slot32>::find_avx2`) の asm:

```asm
.LBB172_6:
    tzcntl  %ebx, %eax                  ; lane1
    movzwl  (%r11,%rax,2), %edx          ; tag1 load
    movl    %edx, %r14d
    andl    $2016, %r14d                 ; off1
    blsrl   %ebx, %ebx                   ; lm2
    movq    (%rdi,%r14), %r14            ; load *p1.key
    je      .LBB172_7                    ; if lm2 == 0 → 単発処理へ
    cmpq    %r9, %r14                    ; cmp1
    je      .LBB172_12                   ; match
    tzcntl  %ebx, %eax                   ; lane2  ← cmp1 retire 後
    movzwl  (%r11,%rax,2), %edx          ; tag2 load (cmp1 後)
    movl    %edx, %r14d
    andl    $2016, %r14d
    cmpq    %r9, (%rdi,%r14)             ; cmp2 + fused load
    je      .LBB172_12
    blsrl   %ebx, %ebx
    jne     .LBB172_6
```

ソースで p1 と p2 の load を `if cmp1 { return }` の前に並べたが、LLVM は
`cmp1 → conditional branch` を control flow boundary として扱い、load p2 を
**cmp1 の後ろ** に流した。前報 OQ-2 で予告していた hazard そのもの。OoO で
speculatively 重なる可能性は残るが、命令スケジュール上は直列のままで、期待
していた `[load5][load5]` 並列の dep chain 短縮 (−11 cy/pair) は出ない。

### 2.4 失敗原因 2: per_shard=64 の cand 分布で unroll ×2 がほぼ発火しない

per_shard=64 / 14-bit hash → 衝突確率 ≈ 0.004/lane → chunk あたり期待 cand 数
≈ 0.06 (match chunk なら +1 で ~1.06)。**圧倒的に "0 or 1 cand chunk" が支配的**。

| ケース | 旧 (BLSR ×2) | 新 (PEXT+unroll) | Δ |
|---|---|---|---:|
| 0 cand chunk | movemask → test → 出口 | + **PEXT 3 cy** + test → 出口 | +3 cy/chunk |
| 1 cand chunk | tzcnt/load/and/cmp/blsr×2 | + PEXT + lm2==0 分岐、blsr 1 個減 | +1〜2 cy/chunk |
| 2+ cand chunk | 直列 5×N | unroll で並列化 (実現せず) | ~0 |

per_shard=64 で 4 chunks scan、cand 分布は典型的に "3 chunks が 0-cand + 1 chunk
が 1-cand" → **+12 cy 程度の固定コスト増、unroll の utility ほぼゼロ**。

実測の +4-5% regression は、この見立てとオーダーで整合する。

### 2.5 机上見積が overestimate になった理由

前報 `find-avx2-pext.md` §P2 の試算:

```text
Δscan ≈ +3 × 0.7  (PEXT setup, chunk ヒット時のみ)   ← 誤
      + (-11) × (0.88 / 2)  (pair pipeline 利得)     ← 誤
      + (-1) × 0.88  (BLSR 削減)                     ← 部分的に正
      ≈ -3.6 cy/scan
```

3 つの誤り:

(a) **PEXT setup を「chunk ヒット時のみ」と仮定**: 実装上は cmp の直後で毎 chunk
    払う。chunk ヒット率 0.7 ではなく 1.0 で計算すべきだった。

(b) **N_cand=0.88 を全 chunk 平均と取り違えた**: 0.88 は match chunk 内の cand 数
    (1 + 衝突期待値)。**全 chunks 平均は ~0.06 + 0.25 = 0.3 程度**。pair pipeline 利得
    の発火率は `(0.88/2) = 0.44` ではなく `0.06/2 ≈ 0.03` で、計算式の支配項を
    桁レベルで間違えた。

(c) **LLVM の load 並列化を仮定した**: OQ-2 で予告した hazard が現実化。`asm!`
    直書きや `core::hint::black_box` での強制が無い限り、cmp 越しの load hoist は
    LLVM のヒューリスティクスでは出ない。

## 3. 教訓

### T1. 命令数の節約 ≠ throughput の改善

P3 で命令数 7 → 4 と短縮しても OoO スケジューラはすでに 7 op を 2-3 cy throughput
で消化していた。**asm 行数で「効きそう」と判断するのは危険**。critical path 上の
直列依存 (本件では `vpbroadcastw` の domain crossing) を律速要因として特定して
からでないと、見かけの命令数削減は OoO に吸収される。

### T2. cand 分布は match chunk と非 match chunk で桁違い

per_shard=64 / 14-bit hash の cand 分布は、match chunk: ~1.06、非 match chunk: ~0.06。
**「全 chunks 平均」と「match chunks 平均」を混同しない**。前報の N_cand=0.88 は
後者を前者と取り違えたもので、unroll 系の利得計算の支配項を桁レベルで誤った。

### T3. LLVM の hoist 期待は asm 確認後にしか信じない

「load 並列化を期待して unroll する」設計は、LLVM が cmp/branch 越しに hoist を
出さない限り絵に描いた餅。**前報 OQ-2 で hazard と認識していたのに見切り発車した**
のが本件の手痛いミス。asm prototype を先に作って並列化が実際に出るかを確認して
から perf-gate に進むべきだった。

### T4. baseline は最低 2 回取って drift を測る

P3 単独評価で観測した −1.6〜−2.5% は、後の `before-p2` baseline 取り直しで同機械が
±2-3% 揺れたことから "noise floor 越えぎりぎりの偶然" と判明した。**1 回の baseline
だけで「improvement 」と結論を急がない**。最低 2 回取って自己分散を測ってから
"signal vs noise" を判定する運用が必要。CLAUDE.md memory `feedback_perf_gate_diversity.md`
の精神は perf-gate の自己分散測定にも適用される。

### T5. publishable surface は「効くと検証できたものだけ」

`senba` は crates.io 出荷予定の publishable crate。`unsafe` + `target_feature` 関数を
追加するたびに公開 attack surface が広がり、Zen 1/2 例外のような長期負債も背負う。
「机上で良さそう」「asm が綺麗」だけでは投入基準として不十分で、**統計的に有意な
perf 改善 + 設計上の必然性** の両方が揃わない限り入れない方針が library として
正しい。

## 4. 次に試すなら

本稿で revert した P3 / P2 を再挑戦する場合の前提条件:

- **B1 (SoA tag split = 8-bit hash 専用配列)** との比較を先に行う。`cmpeq_epi8` が
  直接 1 bit/lane mask を出すので PEXT compression が無用になる方向。前報
  `find-avx2-pext.md` §4 で P2 と排他と整理されていた案。
- 仮に B1 が劣勢で P2 系に戻るなら、`asm!` 直書きで `cmp1 → load p2` の hoist を
  強制するか、`core::hint::black_box` で reorder block を挟んで OQ-2 の hazard を
  自前で潰してから perf-gate に進む。
- needle 構築 (P3) はそもそも全 op cost の上限 ~3% で、**priority は低**。SIMD
  ループ本体の最適化が頭打ちになってから戻ってくれば良い。

なお Zen 1/2 検出は本件では省略してきたが、再挑戦時に PEXT/PDEP を入れる場合は
CPUID family check の選択肢 (前報 §3 方式 A) を再評価する余地あり。

## 5. まとめ

`find_avx2` の PDEP/PEXT 投入は **P3 が利得不確定 (noise 同オーダー)、P2 は明確に
net 後退** で、両方 revert した。原因は (a) OoO がすでに scalar shuffle を hide
していたこと、(b) cand 分布で unroll ×2 がほぼ発火しないこと、(c) LLVM が cmp
越しの load 並列化を出さなかったこと、の 3 つ。前報机上見積が overestimate だった
ポイント (PEXT 毎 chunk コスト / N_cand 取り違え / LLVM hoist 期待) を §3 教訓として
残し、次回同種の最適化に着手するときの前提条件を §4 にまとめた。code は HEAD
(`2c7160a`) に戻し、本稿のみが本セッションの artifact となる。
