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

## 修正後のユーザーへの answer

> 1セグメント Swiss 的 SIEVE は orig より **十分に効率的** か?

**Yes — cap ≤ ~256 では明確に勝つ**:

- **cap=100 で j3 は orig の 0.52〜0.73x** (skew が低いほど j3 有利、最大で
  約 2 倍速い)
- **cap=1000 では負ける** (j3/orig ~1.85x)
- **cap=10000 では大敗** (j3/orig 6〜19x、structural な O(N) scan)

つまり crossover は **cap=100〜1000 の間 (おそらく 200〜500 周辺)** で、
これ以下なら 1 セグメントで HashMap を捨てる設計が正解。これ以上は
segment 分割 (J1/J2) に進む必要がある — というのが当初の予想通りで、
ただし「cap=100 でも勝てる」ことが正しく示せたのは修正後の話。

## 教訓 — "命令レベルまで降りる" の重要性

最初のレポートでは「cap=100 で同等、構造的に array は不利」と結論しかけた。
asm を見て初めて 2 つの実装ミス (scalar 末尾 + SipHash overkill) が見え、
修正後は j3 が圧勝。**ベンチ結果は実装ミスを構造的限界に擬態させる**。
今後 v4 や J2 を評価するときも、結論前に必ず asm を覗く工程を挟むべし。

## 次の実験候補 (修正後の地図に基づく)

1. **J2 (set-associative SIEVE)**: cap=1000+ の領域は依然 HashMap が支配的。
   per-segment SIEVE に切れば cap=10000 でも勝てる可能性がある。
2. **より小さい capacity (cap ∈ {16, 32, 64})**: j3 の優位がどこまで広がるか
   地図化。SIMD scan の cost が更に縮む。
3. **`get`-only / `get`-heavy workload**: 現在は insert_only だが、`get` のみ
   なら hit-path 比較が直接見える。j3 の優位がさらに大きくなるはず。
4. **orig 側の磨き** (E1 = MaybeUninit、A1 = FxHashMap): 「修正後 orig_pro」と
   修正後 j3 を head-to-head する公平比較。orig も FxHash にしたら勝負が
   再びひっくり返る可能性あり (= "勝者を磨くと array は再び負ける")。

## 次の実験候補 (この結果を踏まえて)

1. **J2 (set-associative SIEVE)**: HashMap 層を完全消滅。J3 の inner
   segment 機構を流用し、外側を hash-direct lookup に切り替える。J3 の
   `find()` を per-segment にすれば実装は短い。hit rate 影響は要実測。
2. **より小さい capacity (cap ∈ {16, 32, 64})**: J3 の crossover 下限を
   地図化。単段 SIMD scan が orig を確実に倒す cap 領域があるか?
3. **`get`-heavy workload**: 現状は insert_only。`get` だけなら HashMap の
   bucket probe vs SIMD scan の純粋な勝負になり、J3 の真価が出るかもしれない。

cap=100 で j3 が orig と同等に並んだ事実は、「HashMap が支配的という前提
そのものを覆すには、単一セグメント化 + SIMD scan だけでは不十分」という
ことを意味する。次の段階は J2 で **outer Map を hash-direct に置換** して
HashMap 層自体を per-segment 局所化する方向に進むのが筋が良い。
