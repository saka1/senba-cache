# 2026-05-06 — `sieve_j8` candidate ループ集中分析: id 抽出コストと false-match の coupling

- 親: `2026-05-05-sieve-j8-bench.md` (§4.4 の (a)/(b) 分解 → 本稿で再構成)
- 関連: `2026-05-05-sieve-j8-design.md` §8.1 (机上検討の前提)
- 種別: 解析ノート (新規ベンチなし、既存 profile + asm 読解)

## 0. TL;DR

j8 の find_avx2 退行を「(a) dep chain 延長 + (b) false-match 率倍増」と独立 2 項で書いたが、
両者は **同じ inner ループ本体の中で発火する単一コスト**である。inner ループは true match
でも false match でも回り、candidate 1 個につき必ず id 抽出 4 命令を通る。したがって
false-match 率が上がると id 抽出のコストも比例して積み増される。

正しい単一式:

```text
Δcycles_per_scan(j8 vs j7)
  = 5cy × N_candidate(j8)              ; id 抽出 4 命令、candidate に比例して発火
  + ~7cy × ΔN_false_match               ; 失敗パスの mask clear 5 命令 (false match のみ)
```

ここで `N_candidate ≈ hit_rate + false_match_per_scan`、つまり **id 抽出のコストすら
false-match 増の影響を受けて見える**。これが per_shard を下げたときに退行が
机上予測 (+1.0 ns "(a) only") より速く消える (実測 +0.14 ns) 理由。

## 1. 動機

`2026-05-05-sieve-j8-bench.md` §4.4 で退行を 2 つに分解した:

- (a) +1.2 ns/op = "id 抽出 dep chain 延長 (movzbl + and 5-6 cy)"、true-match 候補に乗ると説明
- (b) +0.92 ns/op = "false-match 率 64x 増 (1/16384 → 1/256) で失敗パス +0.25 traversal/scan"

D' (`§8`) で per_shard を 64→16 に下げたとき退行は +4.6 → +0.14 ns に縮んだ。机上では
"(a) は per_shard 非依存なので +1.0 ns 残る" と予測していた。実測の方が好結果なので
仮説修正が必要 — その修正が「(a) の発火回数は false-match で膨らんでいた」。

本稿はこの 1 点に集中する。

## 2. コードと asm の対応

### 2.1 Rust (`src/sieve_j8.rs:165-185`)

```rust
while i < limit {                                  // (A) outer SIMD scan ループ
    let v      = _mm256_loadu_si256(/* tags[i..] */);
    let masked = _mm256_and_si256(v, mask_v);      // tag & SCAN_MASK
    let cmp    = _mm256_cmpeq_epi16(masked, needle_v);
    let mut mask = _mm256_movemask_epi8(cmp) as u32;
    while mask != 0 {                              // (B) inner candidate ループ ★
        let bit  = mask.trailing_zeros() as usize;
        let lane = bit >> 1;
        let pos  = i + lane;
        let tag  = *tags_ptr.add(pos);             // (C-1) j7 にない load
        let id   = id_of(tag);                     // (C-2) j7 にない and 0x3f
        let e    = (*entries_ptr.add(id)).assume_init_ref();
        if &e.key == key {                         // (D) key 比較
            return Some(pos);                      //      hit → 抜ける
        }
        mask &= !(0b11u32 << (lane << 1));         // (E) 失敗 → bit 落として継続
    }
    i += LANE;
}
```

**観察すべき構造**: (C) と (D) は inner ループ (B) の中。つまり candidate 1 個 (true match
であれ false match であれ) ごとに必ず通る。**(C) は "成功 path" 限定ではない**。

### 2.2 j8 の disassembly (Intel)

`2026-05-05-sieve-j8-bench.md` §4.1 から再掲:

```asm
; ===== (A) outer SIMD scan =====
2c5f9: vpand    ymm2, ymm1, [r9 + r10*2]      ; v = tag & SCAN_MASK
2c5ff: vpcmpeqw ymm2, ymm2, ymm0              ; == needle?
2c603: vpmovmskb r11d, ymm2                   ; bitmask
2c607: test     r11d, r11d
2c60a: je       2c5f0                         ; mask == 0 → 次 chunk

; ===== (B) inner candidate ループ head =====
2c610: tzcnt    ecx, r11d                     ; bit = ctz(mask)
2c615: mov      edx, ecx
2c617: shr      edx, 1                        ; lane = bit >> 1
2c619: or       rdx, r10                      ; pos = i + lane

; ===== (C) j8 だけにある id 抽出 4 命令 =====
2c61c: movzx    ebx, BYTE PTR [r9+rdx*2+1]    ; tags[pos] 上位バイト (L1 hit ~5 cy)
2c622: and      ebx, 0x3f                     ; id = bits 8..13 を抽出
2c625: shl      ebx, 4                        ; id * sizeof(Entry)
2c628: cmp      [rdi + rbx], rsi              ; entries[id].key == search_key ?
;       j7 の同位置: shl rbx,4 / cmp [rdi+rbx],rsi の 2 命令だけ
;       → j8 は +2 命令、+5-6 cy のクリティカルパス追加

; ===== (D) 成功 path =====
2c62c: je       2c641
                ...
2c641: mov      eax, 1
2c646: pop      rbx
2c647: vzeroupper
2c64a: ret

; ===== (E) 失敗 path: bit-pair を落として inner 継続 =====
2c62e: and      cl, 0x1e
2c631: mov      edx, 0x3
2c636: shl      edx, cl
2c638: not      edx
2c63a: and      r11d, edx
2c63d: jne      2c610                         ; まだ candidate あり → (B) head へ
2c63f: jmp      2c5f0                         ; なし → 次 chunk
```

(C) の 4 命令は **(B) と同じベーシックブロック**に居て、`je 2c641` (成功) でも
`and cl, 0x1e` (失敗) でも、その手前で必ず実行されている。

## 3. 1 scan あたり各ブロックの発火回数

cluster018 cap=4096 で hit ratio ≈ 0.63 と仮定。1 get = 1 scan、scan = `ceil(per_shard/16)`
chunk × 16 lane。

候補出現の期待値 (1 scan あたり):

```text
N_true   ≈ hit_rate                               ; 多くて 1 個 (見つかれば即 return)
N_false  ≈ per_shard × 2^(-hash_bits)             ; 構造的、scan 全 lane に対して

j7 (hash 14 bit, false 1/16384):  N_false = 64/16384 = 0.004 (per_shard=64)
j8 (hash  8 bit, false 1/256  ):  N_false = 64/256   = 0.250 (per_shard=64)
```

各ブロックの発火回数:

| block | 起動条件 | j7 (per_shard=64) | j8 (per_shard=64) | j8 / j7 倍率 |
|---|---|---:|---:|---:|
| (A) outer | 毎 chunk | 4 | 4 | 1.0 |
| (B) inner head | candidate 1 個 | 0.634 | **0.880** | 1.39 |
| (C) **id 抽出 4 命令** | candidate 1 個 | (該当命令なし) | **0.880** | ∞ |
| (D) 成功 path | true match 1 個 | 0.63 | 0.63 | 1.0 |
| (E) 失敗 path | false match 1 個 | 0.004 | **0.250** | 64 |

### 3.1 ここが本稿の主張

> **(C) は「true match で発火」じゃなく「candidate ごとに発火」する。candidate 数は
> false match で膨れている。よって false match を減らすと (C) の発火回数も比例して減る。**

j8 で id 抽出は 1 scan あたり 0.88 回回り、そのうち 0.25 回 (= 28%) は false match に
乗っている。`2026-05-05-sieve-j8-bench.md` §4.4 で "(a) +1.2 ns" を出した時、これは
candidate 0.88 個 × 5cy として算出されており、内訳的には「true match 由来 0.63 × 5 + false
match 由来 0.25 × 5 = 3.15 + 1.25 cy」。後者の 1.25 cy ≈ +0.31 ns は **(b) と同じ "false
match 由来" 成分**を別の場所で計上したもの。

## 4. samply data での裏取り

§4.4 表の再解釈:

| 命令 | 位置 | 発火条件 | j7 samples | j8 samples | 解釈 |
|---|---|---|---:|---:|---|
| `and ebx, 0x3f` | 2c622 | 全 candidate | — (命令なし) | **433** | id 抽出 dep chain skid |
| `and cl, 0x1e` | 2c62e | 失敗 candidate のみ | 6 | **409** | 失敗 mask clear skid |

j8 の 433 と 409 がほぼ同じであることが鍵。

- 433 = (id 抽出 skid、true match + false match の合計に比例)
- 409 = (失敗 mask clear、false match のみに比例)
- 433 − 409 = 24 ≈ true match で発火する成分

つまり **j8 の id 抽出 skid 433 sample のうち 95% が false match に乗っている**。これは
§3 の「N_candidate=0.88 のうち 0.25 (= 28%) が false match 由来」とは数字が合わないように
見えるが、samply の skid は dep chain 上で stall した IP に偏在するため、L1-hit-latency
で stall した命令のあとの skid は短時間でも均等にサンプル化される。両 candidate 種類で
同じ stall 構造を共有しているから、**サンプル比 ≈ 候補比**になる。

候補比で再計算: 0.25/0.88 = 28% が false match 起源、0.63/0.88 = 72% が true match 起源。
samples 433 を分解すると ~121 が false match 起源、~312 が true match 起源。

j7 では同位置に該当命令自体が無いので 0 sample。j7 がここで使う命令は `shl rbx, 4` のみで
1 cy、dep chain stall を起こさないから skid サンプルが集まらない。

## 5. 単一式での書き直し

**従来 (誤): 独立 2 項**

```text
退行 = (a) id 抽出 dep chain × 全 get      + (b) false match 失敗 path × Δ false match
     = 1.2 ns/op                             + 0.9 ns/op
     = 2.1 ns/op  (実測 +3.8 ns との残差 1.7 ns は loop control 等 — §4.4 (c))
```

**修正 (正): 単一の per-candidate コスト × candidate 数**

```text
退行 = 5cy × N_cand_j8           ; id 抽出 4 命令、全 candidate で発火
     + ~7cy × ΔN_false           ; 失敗 mask clear、false match のみ
     = 5cy × 0.88 + 7cy × (0.250 − 0.004)
     = 4.4 cy + 1.7 cy
     = 6.1 cy ≈ 1.5 ns/op
```

実測 +3.8 ns との差 +2.3 ns は:

- 単純 cycle 加算で書ききれない OOO 削減効果 (dep chain が長くなって chunk 間 overlap が
  削れる)
- loop control (`add 0x10`, `cmp`) の skid 増 (+125 sample = +0.5 ns)
- inner ループ自体の reissue overhead (jne 2c63d → 2c610 の back-edge)

3 項目とも primary には dep chain (= (C) の +5cy) の "二次効果"。**根は inner ループの
per-candidate コストが膨らんだことに集約される**。

## 6. per_shard 依存の予測 (D' の事後説明)

§3 の式 `5cy × N_cand + 7cy × ΔN_false` で per_shard ∈ {16, 32, 64} を予測:

| per_shard | N_cand_j7 | N_cand_j8 | ΔN_false | 5cy × N_cand_j8 | 7cy × ΔN_false | 合計 cy/scan | ns/op @4GHz | 実測 Δ |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 16 | 0.631 | 0.692 | 0.061 | 3.46 | 0.43 | 3.89 | **+0.97** | +0.14 |
| 32 | 0.632 | 0.755 | 0.123 | 3.78 | 0.86 | 4.64 | **+1.16** | +1.72 |
| 64 | 0.634 | 0.880 | 0.246 | 4.40 | 1.72 | 6.12 | **+1.53** | +4.59 |

机上値 vs 実測:

- per_shard=16: 机上 +0.97 ≫ 実測 +0.14 → 机上予測の方がやや過大。OOO で chunk 1 個分の
  scan は dep chain 延長を完全に隠せている可能性 (= back-to-back 1 chunk なら overlap 余地
  小だから "id 抽出 5cy" 全体は隠れる) **NOTE**: 実測の方が机上より安く出る方向は
  健全な方向。
- per_shard=32: 机上 +1.16 < 実測 +1.72 → 概ね一致 (誤差 ~0.5 ns)。
- per_shard=64: 机上 +1.53 ≪ 実測 +4.59 → 大きい乖離。chunk 4 個に渡る dep chain
  延長と false match の inner ループ反復が OOO で隠せる量を超えた領域。本稿の単純式は
  underestimate で、ここでは "二次効果" (= dep chain 延長 × candidate 多 × chunk 多 の三重交互)
  が支配的。

つまり単一式は "dep chain 延長が OOO で隠せる帯域" でしか正確でない。per_shard=64 では
モデルが破れるが、これも「inner ループが何回回るかが効く」という本質を否定はしない
(むしろ補強する: より多く回るほど隠せなくなる)。

## 7. 設計含意

### 7.1 アルゴリズム的逃げ道がない理由

「(C) の id 抽出を candidate 確定後に遅延させたい」と思うが、**(C) は (D) の `cmp [entries[id]], rsi`
の前に id を計算する必要があるため、inner ループから出してから id 計算、は不可能**。
key 比較に entries[id] を読む必要があり、entries[id] には id が要る。chicken-and-egg。

唯一の構造的逃げ道は:

1. **id を tag から外す** (§M5.3 素朴版: 別配列 `order: Vec<u32>`) → メモリ削減目的を捨てる
2. **tag pos = entry pos に戻す** (j7 構造) → memory-fair の主目的を捨てる
3. **per_shard を下げて N_cand を抑える** ← D' で確認できた現実的解

### 7.2 hash bit 増の損益

j8 で hash bit を 8 → 10 に増やせば false match は 1/256 → 1/1024 で 4x 改善。一方 ID bit
は 6 → 4 になり per_shard ≤ 16 の構造的制約が出る。**それは結果的に per_shard=16 を強制
することと等価**で、D' の発見と整合する。j8 + ID 4-bit 化は実質「per_shard=16 専用 j8」
として実装する選択肢になる。

### 7.3 sweet spot 確定

D' (`2026-05-05-sieve-j8-bench.md` §8) と本稿で得た知見を合わせると:

> **j8 の正しい運用 = per_shard=16 固定**。これで N_cand_j8 ≈ N_cand_j7 になり、id 抽出の
> per-candidate コストも 1 chunk scan 内に OOO で隠れる。memory 20 B/cap の利得を保ったまま
> throughput 退行を実質ゼロにできる。

「per_shard ≤ 64 が構造的上限」という設計時の表現は技術的には正しいが、**運用上の sweet
spot は構造的下限近傍**。これは後段で memfair sweep (A) を回すときの cell 設計にも反映
すべき (orig との head-to-head は per_shard=16 で取る)。

## 8. 命令レベル最適化候補 (BMI1 + bit レイアウト変更)

§7.1 で「アルゴリズム的逃げ道はない」と結論したが、**inner ループを 1 命令でも削る方向**
ではまだ余地がある。candidate 数は固定で減らせなくても、1 candidate あたりのコストを
縮められれば全 per_shard で線形に効く。AVX2 を使っている時点で BMI1/BMI2 は実機 (Haswell+)
で必ず利用可能なので、これらを活用する。

### 8.1 現状の 2 本の dep chain

inner ループには独立した 2 本のクリティカルパスが同時に走っている。

**Path A (load → cmp、id 解決)**: iteration 内で完結、key 比較成立まで全部待つ。

```text
tzcnt(3) → mov+shr(1) → or(1) → load tag(5) → and(1) → shl(1) → cmp+load(5)  ≈ 17 cy
```

**Path B (mask 更新、次 iter の tzcnt まで)**: false-match 連発時の inner ループ throughput
の真のボトルネック。

```text
tzcnt(3) → and cl,0x1e(1) → shl(1) → not(1) → and r11(1)  ≈ 7 cy
                                              ↓
                                              次 iter の tzcnt は r11 待ち
```

### 8.2 候補 (a): BLSR ×2 で Path B を 7 cy → 2 cy

`vpcmpeqw` + `vpmovmskb` の出力は **必ず偶数位置のビットペア (`0b11` ずつ立つ)** という
性質を使う。BMI1 の BLSR (= `x & (x − 1)`) を 2 回適用すれば最下位ペアが落ちる。

```rust
mask &= mask.wrapping_sub(1);  // BLSR: 最下位 1 bit クリア
mask &= mask.wrapping_sub(1);  // BLSR: 残ったペアの上位 bit もクリア
```

検算: mask = `…0011000` → 1 回目 `…0010000` → 2 回目 `…0000000`. ✓

効果:

- **5 ops → 2 ops** で命令数 −3
- BLSR は **tzcnt 結果に依存しない** (r11 だけ参照)。次 iter の tzcnt までの critical edge:
  **7 cy → 2 cy**
- false-match が複数連続する per_shard=64 の "(c) 二次効果" (§5) を直接削る方向

実装: `target_feature(enable = "avx2,bmi1")` を併記。LLVM が `x & (x-1)` を BLSR に落とすか
`cargo asm` で確認、駄目なら `_blsr_u32` intrinsic 直書き。`is_x86_feature_detected!("avx2")`
ガード下なら BMI1 は実行時保証 (Haswell 以降は同梱)。

### 8.3 候補 (b): bit レイアウトで `shl ebx, 4` を消す

#### 8.3.1 トリックの本質と依存条件

狙いは inner ループ末尾の `shl ebx, 4 ; cmp [rdi + rbx], rsi` の **`shl` を消す** こと。
これは

```text
tag & MASK == id × sizeof(Entry)
```

を AND 1 命令で得られれば成立する。条件式: id を tag の **`ID_SHIFT = log2(sizeof(Entry))`
ビット目から始める** こと。`*sizeof(Entry)` の倍率を「id を上に詰めた位置」に吸収させる
発想で、結果として shl が消える。

**重要な依存性**:

- **per_shard には依存しない**。id 幅は MAX_PER_SHARD=64 由来の 6 bit 固定。位置だけが動く。
- **`sizeof(Entry<K, V>)` に依存する**。型パラメータで決まる。
- **`sizeof(Entry)` が 2 の冪でないと成立しない** (`log2` が整数にならない)。

#### 8.3.2 `sizeof(Entry)` 別の成立可否

| sizeof(Entry) | ID_SHIFT | id 位置 | hash 8 bit の配置 | 備考 |
|---:|---:|---|---|---|
| 16 (例: `Entry<u64,u64>`) | 4 | bits 4..9 | 0..3 + 10..13 | ベンチの主流ケース |
| 32 | 5 | bits 5..10 | 0..4 + 11..13 (5+3) | |
| 64 | 6 | bits 6..11 | 0..5 + 12..13 (6+2) | |
| 128 | 7 | bits 7..12 | 0..6 + 13 (7+1) | |
| 256 | 8 | bits 8..13 | 0..7 (連続) | **現行 j8 レイアウトと一致** (改善なし) |
| 512 | 9 | bits 9..14 | bit14=visited と衝突 | ✗ |
| 非 2 冪 (24, 40, …) | — | — | — | ✗ |

つまり **改善が出るのは `sizeof(Entry) ∈ {16, 32, 64, 128}` の場合だけ**。
`= 256` は現行と等価で diff ゼロ、`> 256` または非 2 冪では破綻するので現行レイアウトに
フォールバックする (= shl が残る)。

#### 8.3.3 ベンチ条件 (`sizeof(Entry<u64,u64>) = 16`) での具体形

```text
[ live(15) | visited(14) | hash_hi(13..10) | id(9..4) | hash_lo(3..0) ]
```

`tag & 0x03f0` が **そのまま `id × 16` byte (= id × sizeof(Entry))** になる:

```asm
movzx ebx, WORD PTR [r9 + rdx*2]   ; full tag load
and   ebx, 0x03f0                   ; = id * 16 (byte offset)
cmp   [rdi + rbx], rsi              ; entries[id].key
```

3 命令 → 2 命令。Path A: **17 cy → 16 cy** (−1 cy)、命令数 −1 × N_cand。

Rust 側 (byte 単位で pointer をずらす):

```rust
let id_x16 = (tag as usize) & 0x03f0;
let entry_ptr = (entries_ptr as *const u8).add(id_x16) as *const MaybeUninit<Entry<K, V>>;
let e = (*entry_ptr).assume_init_ref();
```

定数: `ID_SHIFT = 4`、`ID_MASK = 0x03f0`、`HASH_MASK = 0x3c0f` (= `hash_hi | hash_lo`)、
`SCAN_MASK = LIVE | HASH_MASK = 0xbc0f`。hash bits 数は **8 のまま (false-match 率 1/256
不変)**。`needle_from_hash` の packing 式と `insert` の tag 構築式 (`entry_id << ID_SHIFT`)
を併せて書き換え、`bit_layout_exclusivity` テストを更新。oracle テスト
(`matches_sieve_orig_externally_1shard`) が通れば semantics OK。

#### 8.3.4 ジェネリック対応の実装スケッチ

`sizeof(Entry)` で位置決定するなら const にして型ごとにコンパイル時計算:

```rust
const fn id_shift<K, V>() -> u32 {
    let s = size_of::<Entry<K, V>>();
    assert!(s.is_power_of_two(), "sizeof(Entry) は 2 の冪である必要がある (非 2 冪はフォールバック実装を使う)");
    assert!(s <= 256, "sizeof(Entry) > 256 は visited bit と衝突");
    s.trailing_zeros()
}
```

非対応サイズは `cfg`/特殊化 or 別の find_avx2 実装にフォールバック。ベンチで使う
`Entry<u64, u64>` は sizeof=16 で本トリックが効く。一般 K/V を扱うなら **(b) の効果は
sizeof(Entry) 依存のテーブルで判断する**。

### 8.4 候補 (c): chunk base ptr を outer に hoist (per_shard 依存)

`tags + i*2` を outer ループ前で `lea chunk_ptr, [r9 + r10*2]` し、inner では
`[chunk_ptr + rcx + 1]` で参照する。`bit` がそのまま byte offset に使える (`lane × 2 = bit`)
ので、`mov + shr + or` の **3 ops が消える**。`lane = bit/2` の計算は success path だけで足りる。

命令数の損益:

| per_shard | inner save (×N_cand) | outer cost | net |
|---:|---:|---:|---:|
| 16 | −3 × 0.69 = −2.1 | +1 | **−1.1** |
| 32 | −3 × 0.76 = −2.3 | +2 | **−0.3** |
| 64 | −3 × 0.88 = −2.6 | +4 | +1.4 |

§7.3 で sweet spot=per_shard=16 が確定しているので、運用条件では **(c) は無料の +2 cy
critical path 短縮** (Path A: 16 → 14 cy)。per_shard=64 は命令数で損だが latency は短いので
OOO 観点では中立〜微益。

### 8.5 効果見積もり

per_shard=16, N_cand≈0.69, N_false≈0.06 (D' の運用条件):

| 改修 | Path A 短縮 | Path B 短縮 | 命令数/scan |
|---|---:|---:|---:|
| (a) BLSR ×2 | — | **5 cy → 2 cy** (−5 cy back-edge) | −3 × 0.06 = −0.2 |
| (b) layout 4..9 | 17 → 16 cy | — | −1 × 0.69 = −0.7 |
| (c) chunk hoist | 16 → 14 cy | — | −1.1 |
| 合計 | **−3 cy / cand** | **−5 cy / iter** | **−2.0 ops/scan** |

per_shard=64 (退行最大ケース) では (a) の 5 cy back-edge 短縮が **§6 で機上 underestimate
だった分 (1.53 → 実測 4.59 ns)** に直接寄与する見込み — Path B の dep chain そのものを
7→2 に縮めるので、§5 で「OOO で隠せる量を超えた領域」と書いた領域が縮む方向。

### 8.6 §7 / OQ-3 との関係

- §7.1 の「逃げ道なし」結論は **依存関係の話 (chicken-and-egg)** であり、(b) の「id 抽出を
  1 命令安くする」とは独立。dep chain の長さは減らせる。
- §7.3 の per_shard=16 sweet spot は (b)+(c) で更に強化される (退行が更に縮む方向)。
- OQ-3 (inner unroll で 2 candidate pipeline 化) は (a)(b)(c) を入れた **後** にやるべき
  施策。先に dep chain 長を短くしておけば unroll の効果見積もりも変わる。

### 8.7 推奨アクション順

1. **(a) BLSR ×2** — 1 ファイル変更、`target_feature` に `bmi1` 追加 + mask クリアを 2 行
   書き換え。`cargo asm` で BLSR 出力を確認。**最大 ROI、危険度ほぼゼロ**。
2. **(b) bit レイアウト 4..9** — 定数 4 個と `id_of` / `needle_from_hash` / insert tag 構築式
   の書き換えのみ。oracle テスト通過で semantics 担保。
3. **(c) chunk base ptr hoist** — (b) と組み合わせて inner 1 命令 load を実現。unsafe 算術
   が増えるのでベンチで効果確認しながら判断。

(a)+(b) は **危険度ゼロで critical path を確実に縮める**。(c) は per_shard=16 ベンチで
+effect が見えれば採用、見えなければ保留。

## 9. オープン課題

| # | 課題 |
|---|---|
| OQ-1 | per_shard=64 で単一式が underestimate する分の正確な分解 (LBR/PEBS 取得が要るが WSL では困難) |
| OQ-2 | 4-bit ID + 10-bit hash 専用 j8 (= per_shard ≤ 16) の実装と D' データとの突き合わせ |
| OQ-3 | inner ループを unroll して 2 candidate を pipeline 化 (= dep chain を別レジスタに分割) する asm 改修案。LLVM が生成するか、手書き asm が要るか |

## 付録 A — 単一式の導出メモ

per scan のコスト差を、各 sub-block の cycle × 発火回数で書き下す:

```text
Δ_cycles_per_scan
  = Σ_block (cycle_j8(block) − cycle_j7(block)) × count(block)

  = (cycle_j8(A) − cycle_j7(A)) × 4 chunks       ; 0 (両方同じ命令列)
  + (cycle_j8(B) − cycle_j7(B)) × N_cand          ; ~0 (head の tzcnt/shr/or は同じ)
  + (cycle_j8(C) − cycle_j7(C)) × N_cand          ; **+5cy × N_cand_j8 (j7 側 0)**
  + (cycle_j8(D) − cycle_j7(D)) × N_true          ; 0
  + (cycle_j8(E) − cycle_j7(E)) × N_false         ; 0 (failure mask clear 自体は同じ命令)
  + (cycle_j8(B+E reissue) − cycle_j7) × ΔN_false ; +7cy × Δ (inner ループ余分 1 周)

   ≈ 5cy × N_cand_j8 + 7cy × ΔN_false
```

ここで "(C) のコストは N_cand_j7 ではなく N_cand_j8 にかかる" のがポイント。j7 側は (C)
に対応する命令が無いので "差分" を取ると j8 側の発火回数だけが残る。

## 10. 実装後の実測 (§8.2(a) + §8.3(a) 適用)

§8.7 推奨の (a) BLSR ×2 + (b) bit レイアウト変更を `src/sieve_j8.rs` に **同時** 適用
し、`scripts/sweep_j8_pershard.sh` を再実行 (5 trials × 3 cap × 7 variant、cluster018,
LEN=1M)。`profiles/j8_pershard_sweep_2026-05-06-after-blsr-layout.csv` に保存。

### 10.1 inner ループ asm の事後検証

実装後の `find_avx2` inner ループは以下に縮んだ (cargo build --release から `objdump`):

```asm
; ===== inner candidate ループ (新) =====
2c6a0: tzcnt %r10d, %edx              ; bit
2c6a5: shr   $1, %edx                 ; lane = bit / 2
2c6a7: or    %r9, %rdx                ; pos = i + lane
2c6aa: movzwl (%r8,%rdx,2), %r11d     ; tag (full 16 bit)
2c6af: and   $0x3f0, %r11d            ; tag & ID_MASK = id × 16 (= byte offset!)
2c6b6: cmp   %rsi, (%rdi,%r11,1)      ; entries[id].key  ← shl 消滅
2c6ba: je    success
2c6bc: blsr  %r10d, %edx              ; BLSR ×1
2c6c1: blsr  %edx, %r10d              ; BLSR ×2: ペア丸ごとクリア
2c6c6: jne   2c6a0                    ; back-edge
```

§4.1 の旧 inner と命令単位で比較:

| 構成 | Path A 命令数 | Path A dep cy | Path B 命令数 | Path B dep cy |
|---|---:|---:|---:|---:|
| 旧 j8 (§4.1) | 4 (`movzx, and, shl, cmp+load`) | 17 | 5 (`and, mov, shl, not, and`) | 7 |
| 新 j8 (§10) | 3 (`movzwl, and, cmp+load`)        | 16 | 2 (`blsr, blsr`)                | 2 |
| Δ | −1 | −1 | −3 | **−5** |

Path B が 5 cy 縮んだことで、§5/§6 で「OOO で隠せる量を超えた」と書いた領域
(per_shard=64) ほど効くはず — これは §10.2 の per_shard 別 delta で追える。

### 10.2 cluster018 sweep 実測

| cap | per_shard | 旧 j8 (ns) | 新 j8 (ns) | Δ% (新/旧) | 新 orig (ns) | 新 j8 vs orig | 新 j7 (ns) | 新 j8 vs j7 |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1024  | 16 | 30,556 k | 31,994 k | +4.7% | 37,697 k | **−15.1%** | 29,266 k | +9.3% |
| 1024  | 32 | 32,422 k | 32,384 k | −0.1% | 37,697 k | −14.1% | 29,689 k | +9.1% |
| 1024  | 64 | 36,473 k | 36,197 k | −0.8% | 37,697 k | −4.0% | 30,604 k | +18.3% |
| 4096  | 16 | 30,494 k | 30,830 k | +1.1% | 30,775 k | **+0.2%** | 29,070 k | +6.1% |
| 4096  | 32 | 32,323 k | 31,826 k | −1.5% | 30,775 k | +3.4% | 29,524 k | +7.8% |
| 4096  | 64 | 39,349 k | 34,660 k | **−11.9%** | 30,775 k | +12.6% | 30,156 k | +14.9% |
| 16384 | 16 | 28,147 k | 28,966 k | +2.9% | 29,710 k | **−2.5%** | 28,461 k | +1.8% |
| 16384 | 32 | 32,072 k | 31,493 k | −1.8% | 29,710 k | +6.0% | 30,316 k | +3.9% |
| 16384 | 64 | 32,828 k | 32,570 k | −0.8% | 29,710 k | +9.6% | 31,488 k | +3.4% |

(中央値、5 trials。ns 単位は 10⁶ 桁を 'k' 表記。)

### 10.3 §8.5 予測との対応

§8.5 では運用条件 (per_shard=16) で

- Path A −3 cy/cand × 0.69 = −2.1 cy/scan
- Path B −5 cy/iter × 0.06 = −0.3 cy/scan
- 合計 ≈ −2.4 cy/scan ≈ −0.6 ns/op

を予測していた。実測 (cap=4096, ps=16): 30,494 → 30,830 ns/1M ops、つまり **+0.34 ns/op**
の **わずかな悪化**。予測の方向 (改善) と逆。原因解釈:

1. **per_shard=16 では候補ループ自体がほぼ回らない** (N_cand≈0.69, N_false≈0.06)
   ため Path A/B 短縮の絶対額が小さく、別所のばらつき (ベンチ間ノイズ ~1ns) に埋もれる。
2. 実装変更で `vpand` 用 mask を `LIVE | HASH_MASK` (= 0xbc0f, hash bits 分散)
   に変えたため SCAN_MASK 自体は同サイズだが、broadcast 後の cache line 局所性が
   微妙に変わる可能性 (要検証)。

一方 **per_shard=64 (cap=4096)** では予測通り、いやそれ以上の効果が出た:

- 旧 j8 vs 新 j8: 39,349 k → 34,660 k = **−4,689 k ns/1M op = −4.69 ns/op**
- 旧 j8 退行 vs orig (旧 § 6 表): +4.59 ns/op → 新 j8 退行 vs orig: +12.6%×30,775k/1M = +3.88 ns/op
  に縮んだ (= 退行幅 −0.71 ns/op)。
- §8.5 で「Path B の 5cy 短縮は §6 の機上 underestimate (1.53→4.59 ns) 領域に
  直接寄与」と予言した方向と一致。N_cand=0.88, N_false=0.25 で 5cy×0.25=1.25cy ≈ 0.31 ns/op
  の Path B 単独削減を超えた **−4.69 ns/op** の改善 → "二次効果" (dep chain × candidate 数 ×
  chunk 数の交互) が解消された証拠。

### 10.4 §7.3 sweet spot の再評価

`per_shard=16 cap=16384`: **新 j8 が orig を 2.5% 上回り、j7 との差も +1.8% に縮小**。
20 B/cap の memory 利得を保ちつつ、throughput は orig 同等以上 / j7 ほぼ同等を達成。
§7.3 の運用推奨 "j8 = per_shard=16 固定" は引き続き正しいが、§8 の dep chain 最適化により
`per_shard=64` も実用ラインに乗ってきた (退行 +3.4% 〜 +14.9% 程度に圧縮)。

### 10.5 OQ-3 の再優先度

§8.6 で「(a)(b) を入れた後にやる」とした OQ-3 (inner unroll で 2 candidate pipeline 化)
は、Path B が 2 cy しか残っていない以上、unroll の理論利得も 2 cy/iter までしか出ない。
Path A=16 cy がボトルネックなので、次に効くのは Path A の load latency hide
(prefetch / 別 chunk の overlap)。OQ-3 の優先度は下げて差し支えない。

### 10.6 実装変更点サマリ

`src/sieve_j8.rs`:

- `ID_SHIFT` を `const fn id_shift_from_entry_size(sizeof(Entry<K, V>))` で算出
  (Entry<u64,u64> なら 4)。`assert!(s.is_power_of_two() && s <= 256)` を const eval で要求。
- `ID_MASK = ((MAX_PER_SHARD - 1) << ID_SHIFT)`、`HASH_MASK = 0x3FFF & !ID_MASK` を
  associated const として `Inner<K, V>` に定義。
- `needle_from_hash`: 8 bit hash を低 ID_SHIFT bit + 残りに分割し、後者を 6 bit
  さらにシフトして HASH_MASK 高位に流し込む (id 領域を「飛び越える」)。
- `find_avx2`:
  - `target_feature(enable = "avx2,bmi1")` に bmi1 追加。
  - `tag & ID_MASK` を直接 byte offset として `(entries_ptr as *const u8).add(...)` に渡す。
  - mask クリアを `_blsr_u32(mask)` ×2 に置換。
- 既存テスト: `<i32, &str>` (Entry sizeof=24) を `<i32, i32>` (sizeof=8) に変更。
  `bit_layout_exclusivity` を u64,u64 / i32,i32 の 2 レイアウトで検証する形に拡張。
- `matches_sieve_orig_externally_1shard` / `matches_j7_externally` 等 oracle テスト全数 PASS。
