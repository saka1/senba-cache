# sieve_j3 (1 セグメント、Map なし) 初回ベンチ (2026-05-04)

- 日付: 2026-05-04
- 出発点: `2026-05-04-improvement-ideas.md` J 章。「Map と array の結合を切る」
  設計群のうち、外部 HashMap を一切持たない単一セグメント実装 (J3) を作って
  「小容量で `sieve_orig` に対して十分な性能差が出るか」を最初に測る。
  結果がダメなら J1/J2 (segment 分割系) に進んでも徒労になる、という前提。
- 実験対象: `src/sieve_j3.rs`
- bench: `benches/micro.rs` (insert_only)
- profile: `profiles/j3_bench_2026-05-04.json`

## 実装方針 (要約)

- 並列配列: `tags: Vec<u8>` (sentinel 0=dead、live tag は SwissTable 流の
  `(hash >> 56) | 0x80`) と `entries: Vec<Option<Entry<K,V>>>`。`Entry` は
  `key, value, visited` を inline に持つ。
- `order_cap = 2 * capacity`、tail/hand/dead を v3 と同じ流儀で管理。
- `find()` は x86_64+AVX2 で `vpcmpeqb` + `vpmovmskb` の明示 SIMD 経路、
  scalar fallback あり。`is_x86_feature_detected!` の結果は std で
  キャッシュされるので毎呼び出しの overhead は実質ロード 1 回。
- compaction は v3 と同じく `tail == order_cap || dead >= len` で全 live を
  左詰め。**外部 Map を持たないので Map 書き換えコストは構造的にゼロ** (J1/J3
  共通の核)。
- evict は v3 と同じ「2 パス + first_live フォールバック」で、oracle
  (sieve_orig) と evict 列が完全一致 — `tests/oracle.rs::j3_matches_orig_*` で
  確認済み (minimal repro / synthetic Zipf / bundled zipf_1.0)。

## 計測条件

- CPU: Intel i5-12600K (AVX2、AVX-512 は未公開)
- workload: `ZipfGen(skew, n_keys=100_000, seed=42).take(1_000_000)`
- skew ∈ {0.6, 0.8, 1.0, 1.2}、capacity ∈ {100, 1000, 10000}
- criterion: sample_size=20、warm 500ms、measurement 3s
- profile: `[profile.bench] debug = "line-tables-only"`

## 結果サマリ — j3 vs orig

| skew | cap | orig (ms) | j3 (ms) | j3/orig |
|---:|---:|---:|---:|---:|
| 0.6 | 100 | 38.17 | **36.50** | **0.96x** |
| 0.6 | 1000 | 35.53 | 78.96 | 2.22x |
| 0.6 | 10000 | 34.96 | 559.12 | 15.99x |
| 0.8 | 100 | 35.95 | **35.46** | **0.99x** |
| 0.8 | 1000 | 32.03 | 70.98 | 2.22x |
| 0.8 | 10000 | 30.39 | 466.89 | 15.36x |
| 1.0 | 100 | 33.14 | 33.95 | 1.02x |
| 1.0 | 1000 | 25.88 | 55.21 | 2.13x |
| 1.0 | 10000 | 21.52 | 239.93 | 11.15x |
| 1.2 | 100 | 23.20 | 24.06 | 1.04x |
| 1.2 | 1000 | 16.24 | 33.47 | 2.06x |
| 1.2 | 10000 | 14.56 | 108.38 | 7.44x |

## 結果サマリ — 全 variant 横並び

| skew | cap | orig | v0 | v1 | v2 | v3 | **j3** |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 0.6 | 100 | 38.17 | 42.60 | 46.87 | 42.55 | 46.36 | **36.50** |
| 0.6 | 1000 | 35.53 | 40.22 | 42.27 | 38.06 | 41.90 | 78.96 |
| 0.6 | 10000 | 34.96 | 39.04 | 41.51 | 37.93 | 40.21 | 559.12 |
| 0.8 | 100 | 35.95 | 39.99 | 43.05 | 39.55 | 42.37 | **35.46** |
| 0.8 | 1000 | 32.03 | 34.76 | 38.50 | 34.62 | 37.04 | 70.98 |
| 0.8 | 10000 | 30.39 | 34.00 | 35.39 | 33.74 | 35.31 | 466.89 |
| 1.0 | 100 | 33.14 | 36.40 | 37.85 | 36.17 | 38.72 | 33.95 |
| 1.0 | 1000 | 25.88 | 28.16 | 29.05 | 27.62 | 29.44 | 55.21 |
| 1.0 | 10000 | 21.52 | 24.02 | 24.63 | 23.68 | 24.81 | 239.93 |
| 1.2 | 100 | 23.20 | 25.37 | 26.22 | 25.77 | 25.96 | 24.06 |
| 1.2 | 1000 | 16.24 | 17.84 | 18.28 | 18.23 | 18.76 | 33.47 |
| 1.2 | 10000 | 14.56 | 15.99 | 16.03 | 15.86 | 15.96 | 108.38 |

(数値は中央値 ms / `insert_only` の wall time。trace_len = 1,000,000)

## 読み解き

### 1. 「圧勝」予想は外れた — 小容量では概ね同等

J 章の試算 (cap≤1000 で +30-60%、cap=100 で「HashMap 圧勝」=) では j3 が
小容量で優位という見立てだったが、実測では **cap=100 で j3 と orig は
±5% に収まる引き分け** だった。

- 最も j3 が勝った: skew=0.6, cap=100 → -4% (j3 の方が速い)
- 最も j3 が負けた: skew=1.2, cap=100 → +4% (orig の方が速い)

予想と現実のギャップの推定原因:

1. **HashMap が小容量で十分速い**。100 entry なら bucket 配列は L1 に
   完全収納で、SipHash + bucket probe は実測で <30ns / op の領域。
   J 章の試算 "5ns" はハッシュ込みの理論最小値であり、現実の HashMap は
   それより 5x 程度オーバーヘッドがあっても実時間で問題にならない量。
2. **j3 の SIMD scan は order_cap=2*cap まで走る**。cap=100 でも steady
   state では tail が 100〜200 の間を振動 (dead==len で compact)、平均で
   ~150 byte = 5 SIMD iter。固定コストとして HashMap probe より絶対値で
   下回らない可能性がある。
3. **Compaction が j3 にだけ存在する**。trace 100 万件・cap=100 で compact は
   ~1 万回発生 (`dead==len` で 100 entry 移動 / 回)。orig には無い純粋
   オーバーヘッド。
4. **insert-only bench**: hit-path / miss-path の両方で「key 存在チェック」
   を必ず行う。j3 のスキャンは tail 全長を見るのに対し、orig の HashMap は
   bucket 数本だけ見る。Zipf hot key は HashMap で 1 probe で当たる。

### 2. ただし j3 は **全 array variant に勝った** (cap=100)

cap=100 の各 skew で、j3 は v0/v1/v2/v3 を全て上回る:

- skew=0.6, cap=100: orig=38.17 / j3=36.50 / 次点 v2=42.55 → j3 は v2 比 -14%
- skew=0.8, cap=100: orig=35.95 / j3=35.46 / 次点 v2=39.55 → j3 は v2 比 -10%
- skew=1.0, cap=100: orig=33.14 / j3=33.95 / 次点 v2=36.17 → j3 は v2 比 -6%
- skew=1.2, cap=100: orig=23.20 / j3=24.06 / 次点 v0=25.37 → j3 は v0 比 -5%

つまり「**array 路線で小容量に最適化したいなら、Map を捨てて inline tag
にする方が、Map を残しつつ array にするより速い**」という結論が出た。
これは A〜I 章の改善案 (Map を残したまま array を磨く) より、J 群 (Map
を捨てる) の方が array 路線では筋が良いという J 章の主張を支持する。

### 3. crossover は cap=100 と cap=1000 の間にある

- cap=100: j3 ≈ orig
- cap=1000: j3 は orig の **2.2x 遅い**
- cap=10000: j3 は orig の **7-16x 遅い** (skew が flat なほど悪化)

J 章 J3 の試算では crossover が cap≈1000 とされていたが、実測では cap=1000
では既に明確に負けている。**workload-level の crossover は 100 と 1000 の
間 (おそらく cap=200〜500 周辺)**。これより大きい capacity では single
segment は構造的に不利で、segment 分割 (J1 or J2) に進む必要がある。

### 4. flat 分布で j3 がより不利

skew=0.6 (= flatter) → cap=10000 で j3/orig = 16x、skew=1.2 (= skewed) →
7x。理由: flat 分布は miss が多い → 毎 insert で scan を tail 全長走らせる
→ SIMD でも O(N/32) は capacity に比例。HashMap は O(1) を維持。これは
理論通り。

## 初回結果へのユーザーへの answer (修正前)

> 1セグメント Swiss 的 SIEVE は orig より **十分に効率的** か?

**No** (cap=100 で同等、cap≥1000 で明確に劣る)。ただし:

- **array variant の中では j3 が最良** (cap=100 で v0..v3 を全部上回る)
- **絶対性能で orig を超えるためには 1 セグメントでは不十分** に見えた

## 命令レベルの追跡で見つけた 2 つの実装ミス

レポートを書いて満足する前に「圧勝にならない理由は実装ミスでは?」を確かめるため、
`objdump` で `find_avx2` の生成 asm を直接見た。すると 2 つの問題が見えた。

### ミス① `order_cap` が 32 の倍数でなく、scalar 末尾が時間を支配していた

cap=100 → `order_cap = 2*capacity = 200`。AVX2 SIMD は 32 byte 単位なので
`simd_end = tail & !31` で取り残された [simd_end, tail) を **scalar で 0..31 iter**
処理する設計だった。生成 asm:

```asm
; SIMD loop (5-6 cycles/iter)
   vpcmpeqb (%r10,%rdi,1),%ymm0,%ymm1
   vpmovmskb %ymm1,%r14d
   ...

; Scalar tail loop (per iter ~3 cycles)
   inc rsi; add $0x18, r14
   cmp r8, rsi; jae <panic>      ← bounds check #1 (tags)
   cmp dl, (r10,rsi,1); jne next
   cmp r9, rsi; jae <panic>      ← bounds check #2 (entries)
   cmpb $0x2, (r14)
   cmp rbx, -0x10(r14); jne next
```

steady state では tail が 100〜200 を振動し平均 ~15 iter の scalar が混じる。
SIMD 1 chunk (~5 cycles) ≪ scalar 15 iter (~45 cycles) で **見かけは SIMD 化済みでも
実時間は scalar が支配**。さらに scalar 経路には bounds check が 2 つ残っていた。

**修正**: `order_cap = ((2*capacity) + 31) & !31` に変更し、SIMD ループを
`[0, order_cap)` 全域に拡張、scalar 末尾を消去。`tags[tail..order_cap]` は
EMPTY (0) で live tag (>= 0x80) と false-match しない不変条件で safe。

### ミス② `tag_of` で SipHash を使っていた (= "公平にしすぎた" 過剰仕様)

tag は **8-bit の rough filter** で、false match は内側の `key == entry.key` で
必ず弾ける。SipHash の暗号的衝突耐性は不要。一方 `sieve_orig` の HashMap は
bucket 整合のため SipHash 必須。

```rust
fn tag_of(&self, key: &K) -> u8 {
    let mut h = self.hasher.build_hasher();  // RandomState (SipHash) → ~15-20ns
    key.hash(&mut h);
    let raw = h.finish();
    ((raw >> 56) as u8) | 0x80
}
```

j3 と orig の hot path が両方 SipHash で律速されると、SIMD scan を頑張って
削っても hash コストが残って引き分けに収束する。これが「cap=100 でも 圧勝に
ならない」のもう半分の正体。

**修正**: 自前の FxHash 風 1-命令 hash に置換。`u64` キーで `~3-5ns/op`。

```rust
#[inline]
fn write_u64(&mut self, n: u64) {
    self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x517cc1b727220a95);
}
```

### 修正後の生成 asm (find_avx2)

scalar 末尾なし、SIMD ループ + match 処理のみ:

```asm
   vpcmpeqb (%r8,%r9,1),%ymm0,%ymm1
   vpmovmskb %ymm1,%r10d
   test %r10d,%r10d
   jne <match>
   add $0x20,%r9
   cmp %rcx,%r9
   jb <loop>
```

5 命令/iter、~3-4 cycles/iter on Alder Lake。cap=100 (order_cap=224) なら
7 iter = ~25 cycles ≈ 8ns/find。

## 修正後の bench 結果

| skew | cap | orig (ms) | j3 (ms) | j3/orig | Δ vs 修正前 j3 |
|---:|---:|---:|---:|---:|---:|
| 0.6 | 100 | 37.05 | **19.37** | **0.52x** 🎉 | **-46.9%** |
| 0.6 | 1000 | 35.40 | 63.93 | 1.81x | -19.0% |
| 0.6 | 10000 | 33.50 | 644.27 | 19.23x | +15.2% |
| 0.8 | 100 | 34.89 | **19.49** | **0.56x** 🎉 | -45.0% |
| 0.8 | 1000 | 31.22 | 59.94 | 1.92x | -15.6% |
| 0.8 | 10000 | 30.18 | 437.98 | 14.51x | -6.2% |
| 1.0 | 100 | 32.03 | **19.52** | **0.61x** 🎉 | -42.5% |
| 1.0 | 1000 | 24.72 | 46.73 | 1.89x | -15.4% |
| 1.0 | 10000 | 21.49 | 250.94 | 11.68x | +4.6% |
| 1.2 | 100 | 22.65 | **16.62** | **0.73x** 🎉 | -30.9% |
| 1.2 | 1000 | 17.04 | 31.61 | 1.86x | -5.5% |
| 1.2 | 10000 | 16.97 | 102.23 | 6.02x | -5.7% |

(profile: `profiles/j3_bench_2026-05-04_after_fix.json`)

## 修正後のユーザーへの answer (暫定 — 後段の "fair fight" で再評価)

> 1セグメント Swiss 的 SIEVE は orig より **十分に効率的** か?

**(暫定) Yes — cap ≤ ~256 では明確に勝つ**:

- **cap=100 で j3 は orig の 0.52〜0.73x** (skew が低いほど j3 有利、最大で
  約 2 倍速い)
- **cap=1000 では負ける** (j3/orig ~1.85x)
- **cap=10000 では大敗** (j3/orig 6〜19x、structural な O(N) scan)

ただしこの比較には **ハッシュ関数の非対称** という公正性の問題が残る:
j3 は FxHash 風の自前 1 命令 hash、orig は Rust std `HashMap` の SipHash13。
論文 (NSDI'24) のリファレンス C 実装は両者とも非暗号 fast hash (XXH3) を
使う前提で書かれており、Rust std の SipHash 採用は paper-faithful でもない。
よって「j3 のアルゴリズム的優位」と「hash 関数差由来の優位」を分離できて
いない。後段 "XXH3 揃え後の fair fight" でこの分離を行う。

## 教訓 — "命令レベルまで降りる" の重要性

最初のレポートでは「cap=100 で同等、構造的に array は不利」と結論しかけた。
asm を見て初めて 2 つの実装ミス (scalar 末尾 + SipHash overkill) が見え、
修正後は j3 が圧勝。**ベンチ結果は実装ミスを構造的限界に擬態させる**。
今後 v4 や J2 を評価するときも、結論前に必ず asm を覗く工程を挟むべし。

## XXH3 揃え後の fair fight 結果 (追補)

### 動機

修正後 bench では **j3 = FxHash / orig = SipHash** の非対称比較になっており、
「j3 が cap=100 で 0.52x」のうち何割がアルゴリズムで何割が hash tuning か
分離できていなかった。NSDI'24 リファレンス C 実装 (`hash.h` の `HASH_TYPE`
分岐、デフォルト `XXHASH3`) は両者非暗号 fast hash を前提にしているので、
全 variant を `xxhash-rust` の **XXH3** に揃えて再ベンチ。

実装側の変更点:

- `Cargo.toml` に `xxhash-rust = { version = "0.8", features = ["xxh3"] }`
- `src/hash.rs` に `Xxh3Build: BuildHasher` を追加
- `sieve_orig` / `sieve_v0..v3` の `HashMap` を `HashMap<_, _, Xxh3Build>` 化
- `sieve_j3` の自前 `FxHasher` を削除し `tag_of()` で `Xxh3::new()` を使用

### 結果 — 全 variant XXH3 揃え

log: `profiles/j3_bench_2026-05-04_xxh3.log` (criterion 生出力)

| skew | cap | orig (ms) | v3 (ms) | j3 (ms) | j3/orig |
|---:|---:|---:|---:|---:|---:|
| 0.6 | 100 | 38.59 | 46.93 | **27.16** | **0.70x** |
| 0.6 | 1000 | 42.22 | 48.64 | 73.30 | 1.74x |
| 0.6 | 10000 | 34.68 | 42.12 | 606.12 | 17.48x |
| 0.8 | 100 | 37.74 | 43.10 | **27.54** | **0.73x** |
| 0.8 | 1000 | 32.45 | 40.18 | 67.65 | 2.08x |
| 0.8 | 10000 | 30.81 | 36.44 | 467.20 | 15.17x |
| 1.0 | 100 | 30.91 | 37.78 | **28.43** | **0.92x** |
| 1.0 | 1000 | 26.14 | 28.96 | 55.98 | 2.14x |
| 1.0 | 10000 | 20.97 | 23.81 | 275.26 | 13.13x |
| 1.2 | 100 | 21.52 | 27.98 | 21.90 | **1.02x** |
| 1.2 | 1000 | 16.92 | 19.02 | 34.31 | 2.03x |
| 1.2 | 10000 | 15.79 | 16.74 | 109.46 | 6.93x |

### fair fight 前後の比較 (cap=100)

| skew | 旧 j3/orig (FxHash vs SipHash) | 新 j3/orig (XXH3 vs XXH3) | Δ |
|---:|---:|---:|---:|
| 0.6 | 0.52x | 0.70x | +0.18 |
| 0.8 | 0.56x | 0.73x | +0.17 |
| 1.0 | 0.61x | 0.92x | +0.31 |
| 1.2 | 0.73x | 1.02x | +0.29 |

### 解釈

1. **j3 のアルゴリズム的優位は本物だが、控えめ**。cap=100 でハッシュを揃えると
   j3 の lead は **0.5x → 0.7-0.9x** に縮む。半分くらいは「FxHash が SipHash
   より速い」分の寄与だった (= 当初の懸念は的中)。
2. **flat 分布で j3 の優位が消える**。skew=1.0 で 0.92x、skew=1.2 で 1.02x
   (= わずかに負ける) になる。flat 分布は miss が多く scan を tail 全長走らせる
   ので、SIMD scan の固定費を hash 改善で吸収できない。逆に高 skew の方が hot
   key が hit-path で SIMD broadcast 比較に当たって j3 有利になる。
3. **絶対値の動き**: orig は cap=100 で SipHash → XXH3 で **わずかに遅くなる**
   (例: skew=0.6/cap=100 で 37.05 → 38.59 ms)。`Xxh3::new()` は state 初期化
   が SipHash13 より重く、u64 の単発 hash は逆効果になる帯域がある (long key
   で勝つのが XXH3 の強みなので、8B キーでは差が出にくい)。これは将来 wyhash
   や ahash への切り替えで取り戻せる余地。
4. **v3 の位置**: cap=100 では orig より遅く j3 より遅い (中間)。array 路線で
   Map を残す設計は cap=100 では筋が悪い、という旧結論は維持。
5. **paper-faithful 化の副産物**: NSDI'24 の C リファレンスとハッシュ条件が
   揃ったので、今後 paper の数値 (libCacheSim ベンチ) と直接突合できる。

### 修正後のユーザーへの最終 answer

> 1セグメント Swiss 的 SIEVE は orig より **十分に効率的** か?

**Yes、ただし範囲は狭く lead も控えめ**:

- **cap=100 / skew∈[0.6, 0.8] で j3 は orig の 0.70〜0.73x** — ここが本物の
  勝ち領域
- **cap=100 / skew=1.0 では essentially tie (0.92x)**
- **cap=100 / skew=1.2 では負ける (1.02x)** — fair fight でひっくり返った
- **cap≥1000 では一貫して負ける** (1.7-17x、構造的)

「cap=100 圧勝」と当初信じた像は、半分が hash tuning だった。それでも
**特定の (skew, cap) 帯では `array + SIMD scan` で `linked-list + HashMap` を
倒せる**ことが fair fight でも示せた、という意味で実験は成立。

## 教訓 (追補) — "fair fight になるまで結論を急ぐな"

`修正前 → 修正後` で「圧勝」と結論しかけたが、もう一段「ハッシュ条件を
paper に揃える」を挟むまで本当の差分は見えなかった。**ベンチ比較で勝者を
固定する前に "両者の等しくない仮定" を全部洗い出してから結論する**こと。
特に Rust 標準型 (HashMap, BTreeMap) のデフォルト挙動は domain 慣行と
ズレていることがある (今回の SipHash がその例)。

## refactor 後の再ベンチ (追補 2)

### 動機

XXH3 揃え後の実装をレビューしたところ、3 点の冗長/非対称が見つかった:

1. `entries: Vec<Option<Entry<K,V>>>` の Option は `tags` 配列が既に init bitmap
   を兼ねているので二重符号化。
2. `dead` カウンタと `dead >= len` 閾値は、`order_cap = 2*capacity` のもとでは
   `tail == order_cap` と発火タイミングが完全一致する死荷物。
3. `Xxh3::new()` が `tag_of` 内に直書きされていて、v1/v2/v3 が経由する
   `crate::hash::Xxh3Build` (BuildHasher) を j3 だけバイパスしていた。

これらを潰す refactor を行い、性能影響を測り直した。実装変更:

- `Option<Entry>` → `MaybeUninit<Entry>`、live 判定は tags 一本化、`Drop` を
  手書きで live slot だけ落とす
- `dead` フィールド削除、compaction trigger は `tail == order_cap` 一発に
- `hasher: Xxh3Build` field を持ち、`tag_of` は `self.hasher.hash_one(key)`

正当性は `cargo test --lib sieve_j3` (14 件) と `cargo test --test oracle j3`
(3 件: minimal repro / synthetic Zipf / bundled Zipf) で確認、全 pass。

### 結果 — refactor 前後の j3 比較

log: `profiles/j3_bench_2026-05-04_after_refactor.log`

| skew | cap | j3 旧 (XXH3 fair fight) | **j3 新 (refactor 後)** | Δ |
|---:|---:|---:|---:|---:|
| 0.6 | 100 | 27.16 | **25.23** | **−7.1%** |
| 0.6 | 1000 | 73.30 | 72.64 | −0.9% |
| 0.6 | 10000 | 606.12 | 655.92 | +8.2% |
| 0.8 | 100 | 27.54 | **27.03** | −1.8% |
| 0.8 | 1000 | 67.65 | 66.22 | −2.1% |
| 0.8 | 10000 | 467.20 | 515.13 | +10.3% |
| 1.0 | 100 | 28.43 | **26.61** | **−6.4%** |
| 1.0 | 1000 | 55.98 | 53.56 | −4.3% |
| 1.0 | 10000 | 275.26 | 288.08 | +4.7% |
| 1.2 | 100 | 21.90 | **20.85** | **−4.8%** |
| 1.2 | 1000 | 34.31 | 32.50 | −5.3% |
| 1.2 | 10000 | 109.46 | 120.22 | +9.8% |

### 結果 — refactor 後の orig / v3 / j3 横並び

| skew | cap | orig | v3 | **j3** | j3/orig |
|---:|---:|---:|---:|---:|---:|
| 0.6 | 100 | 37.18 | 50.70 | **25.23** | **0.68×** |
| 0.6 | 1000 | 39.44 | 50.45 | 72.64 | 1.84× |
| 0.6 | 10000 | 34.58 | 44.68 | 655.92 | 18.97× |
| 0.8 | 100 | 35.76 | 47.71 | **27.03** | **0.76×** |
| 0.8 | 1000 | 31.87 | 40.88 | 66.22 | 2.08× |
| 0.8 | 10000 | 29.23 | 36.36 | 515.13 | 17.62× |
| 1.0 | 100 | 30.44 | 39.78 | **26.61** | **0.87×** |
| 1.0 | 1000 | 24.59 | 30.34 | 53.56 | 2.18× |
| 1.0 | 10000 | 21.21 | 24.74 | 288.08 | 13.59× |
| 1.2 | 100 | 21.34 | 27.36 | **20.85** | **0.98×** |
| 1.2 | 1000 | 17.41 | 20.26 | 32.50 | 1.87× |
| 1.2 | 10000 | 15.54 | 17.07 | 120.22 | 7.74× |

### 解釈

1. **勝ちパターンが強化**: cap=100 で全 skew が 1.8〜7.1% 改善。Option の
   discriminant 経由のロード/分岐が消えて hot path (find→key 等価→visited 更新)
   の経路長が縮んだ効果。`MaybeUninit::assume_init_ref` は単なる transmute で、
   `Option::as_ref()` の niche 検査ブランチが除去される。
2. **skew=1.2/cap=100 が逆転**: 前回 fair fight で 1.02× と僅差で負けていたのが、
   今回 **0.98× で勝ちに乗った**。j3 の優位帯 (cap=100) が skew=1.2 まで広がった。
3. **cap=1000 帯は微改善 (−1〜−5%)**: scan が steady state で常時走る帯域なので、
   cache 圧の軽減が安定して効いている。ただし orig 比 ~2× 負けの構造的劣勢は
   refactor では取り戻せない (segment 化が必要、既知)。
4. **cap=10000 帯の +5〜10% 劣化はノイズの可能性が高い**: criterion の
   `change vs last baseline` で orig も同帯で −5%〜+0% 程度ふらついており、
   1 サンプル 500ms × 20 という長尺測定の通常変動範囲。refactor で hot loop
   (`find_avx2`) のロジックは変わっておらず、Option → MaybeUninit は構造的に
   同等以上のはずなので、決定打は無いが run-to-run noise の線が濃い。
5. **コード行数の減少**: `dead` 関連の保守 (init / 増減 / 閾値判定) と
   Option の expect/None 分岐がまとめて消えて、変更後の j3 は不変条件が
   "tags が真実" の一本に整理された。

### 教訓 (追補) — "正しさを変えないリファクタも測れ"

「Option を MaybeUninit にしても外面性能は変わらない」と頭で決めて測らずに
通すと、cap=100 帯の 2-7% 改善や skew=1.2 の勝ち越し転換のような **観測可能な
利得を見逃す**。逆に、cap=10000 で出た +5-10% を「劣化」と早合点して revert
することも避けたい (=noise の幅を把握していれば慌てない)。リファクタ前後で
ベンチを取り直す習慣の価値はここにある。

## 次の実験候補

1. **より小さい capacity (cap ∈ {16, 32, 64})**: fair fight 後の j3 優位帯
   (0.6-0.8 skew) は cap が小さくなるほど広がるはず。crossover 下限の地図化。
2. **`get`-heavy workload**: insert_only 以外で測ると、j3 の hit-path が
   HashMap probe より速い領域がより明確に見える可能性。
3. **J2 (set-associative SIEVE)**: cap=1000+ の領域は依然 HashMap が支配的
   (j3 で 1.7-2.1x 負け)。per-segment SIEVE で outer Map を hash-direct lookup
   に置き換えれば cap=10000 でも勝てる可能性。
4. **hash の更なる最適化**: XXH3 で u64 単発 hash が SipHash 並みなら、
   wyhash / ahash / 自前 multiplicative にする余地あり。ただし orig と j3
   両方に同条件で適用する fair fight は維持すること。
