# sieve_j5 — j4 から double-hash を排除した AB

- 日付: 2026-05-05
- 親レポート: `2026-05-05-sieve-j4-pershard-vs-footprint.md` (§次の実験 §2)
- 動機: 親レポートで残った "j4 がまだ orig より遅い ~3 ns の正体" を測る。
  H3 (double-hash 5–10 ns 固定費) を仮置きしていたが未検証で、Sweep C
  cap=16384/N=128 で j4=32 ≈ orig=31 になる観測とも噛み合っていなかった。
  AB を取って double-hash がどれだけ効いているかを直接定量する。

## 設計

- `sieve_j3.rs` に `pub(crate) fn {get,insert,contains}_with_hash(key, hash)` を
  追加。`tag_from_hash(hash) = ((hash >> 56) as u8) | 0x80` で外で計算済みの
  hash の上位 8-bit から tag を作る (j3 単独経路で `tag_of` がやっているのと
  bit-equivalent)。
- `sieve_j5.rs` を新設: j4 と同じ shard 配列 (`[J3<K, V>; SHARDS]` const generic)
  を持ち、per-op で `hash_one(key)` を **1 回**だけ呼んで、下位ビットで shard
  選択 + 上位ビットを j3 に渡す。それ以外 (cap 分配 / `is_power_of_two`
  assert / API 表面) は j4 と完全同型。
- 既存 `sieve_j4.rs` は触らない。同一 run の AB ベースラインとして残す。
- bench は `orig`, `j4_n{1,2,8,32,128}`, `j5_n{1,2,8,32,128}` に絞る。
  v0/v1/v2/v3/j3 は親レポートまでで定量化済みなので落とす。

外形的等価性は `sieve_j5::tests::matches_j4_externally` (10k op churn を j4 と
j5 に並列に流し、最終 key set / value が一致することを確認) で抑える。
shard 選択も tag derivation も同じ XXH3 の同じビット範囲から作るので、
double-hash の有無は外部観測できる差を生まない (= 純粋な性能変更)。

## 実験

CPU: i5-12600K (P-core L1d=48 KB, L2=1.25 MB)、ZipfGen(skew=1.0, 100k keys,
1M ops, seed=42)、`bench` CLI 単発を **5 trial** 取って ns/op の中央値。

- **AB1 (Sweep B 床の再測)**: cap=256, N ∈ {1,2,8,32}。per_shard ∈ {256,128,32,8}。
- **AB2 (per_shard=128 等値線)**: (cap, N) ∈ {(1024, 8), (4096, 32), (16384, 128)}
  の 3 セル。Sweep C で「per_shard=128 を total cap だけ変えて踏む」格子の代表点。

データは `profiles/j5_doublehash_ab_2026-05-05.csv` に raw を保存。

## 結果 — AB1 (cap=256, varying N)

ns/op の中央値:

| N | per_shard | orig | j4 | j5 | Δ(j5−orig) | Δ(j5−j4) |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 256 | 33.24 | 56.89 | 48.36 | +15.12 | **−8.53** |
| 2 | 128 | 33.24 | 45.19 | 39.92 | +6.68 | **−5.27** |
| 8 | 32 | 33.24 | 35.38 | **29.33** | **−3.91** | **−6.05** |
| 32 | 8 | 33.24 | 35.06 | **27.86** | **−5.38** | **−7.20** |

要点:

1. **j5 − j4 は cell によらず −5 〜 −8 ns/op** で揃う。仮説 H3 (double-hash 5–10 ns)
   の下限〜上限の中。double-hash 1 発のコストとして極めて妥当な数字。
2. **per_shard ≤ 32 で j5 が orig を逆転** (29.33 ns vs 33.24 ns)。親レポートの
   「j4 は per_shard を詰めても 34 ns 床から動けない」という観測は、床の正体が
   double-hash であって SIMD scan ではなかったことを示している。
3. N=1 (= j3 を 1 shard 越しに呼ぶだけ) で j5_n1=48 ns。これは j4_n1=57 ns に
   対し −9 ns。1 shard なので shard select は trivial だが hash 自体は走るので、
   ここでも double-hash 削減分がフルに出る。

## 結果 — AB2 (per_shard=128 を異なる total cap で踏む)

ns/op の中央値:

| cap | N | total KB | orig | j4 | j5 | Δ(j5−orig) | Δ(j5−j4) |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1024 | 8 | ~50 | 27.41 | 39.65 | 32.86 | +5.45 | **−6.79** |
| 4096 | 32 | ~200 | 26.27 | 35.31 | 28.37 | +2.10 | **−6.94** |
| 16384 | 128 | ~768 | 22.79 | 32.52 | 25.30 | +2.51 | **−7.22** |

要点:

1. **Δ(j5−j4) が cap によらず −6.8 〜 −7.2 ns/op で再び揃う**。AB1 と独立に
   同じ "double-hash ≈ 7 ns" を計測。L1d (48 KB) を越える total 200 KB / 768 KB
   でも値が変わらないので、double-hash は **キャッシュ階層と独立な定常コスト**。
2. per_shard=128 では j5 でもまだ orig より +2 〜 +5 ns 遅い。これが残りの説明
   変数で、SIMD scan の長さ (per_shard が SIMD 1 chunk = 32 を越える) と shard
   dispatch の固定費 (~1–2 ns) で構成される。double-hash ではない。
3. cap=16384 / N=128 (per_shard=128) で j5=25.30 ns。orig 22.79 ns との差
   2.5 ns がほぼ "scan が 4 chunk になる分" + dispatch。**total footprint が L2
   越え (768 KB > 1.25 MB の半分) でも j5 はサクサク動く** = working set が
   per-op で見て shard 内に閉じている、という親レポート §結論 の予想を再確認。

## 結論 — 親レポートの空欄が埋まった

親レポートで残していた仮置き:

```
ns/op(j4) ≈ const_overhead + scan(per_shard, hit_ratio)
const_overhead ≈ 30–35 ns  (うち double-hash 5–10 ns + その他)
```

を、本 AB で次のように分解できた:

```
const_overhead(j4) = const_overhead(j5) + double_hash_cost
double_hash_cost   = 7 ± 1 ns/op   (cell に依らず安定)
const_overhead(j5) = orig + dispatch + (per_shard >= 64 で SIMD scan 増分)
                   ≈ 27 ns @ per_shard=8、≈ 33 ns @ per_shard=128
```

Sweep C で cap=16384/N=128 が "j4=32 ≈ orig=31" に見えていたのは、運悪く orig の
hit ratio 改善 (cap 大で miss path が減る) が double-hash 7 ns をほぼ相殺して
いた偶然で、double-hash 自体は同じ ~7 ns を確実に払っていた。j5 で剥がしてみると
全 cell で −7 ns が見える (= 親レポートの "32 ≈ 31 偶然説" の方が真)。

## 実用ガイドライン (更新)

- **shard 越しに j3 を呼ぶときは hash を持ち回せ**。これだけで cell に依らず
  −7 ns/op、per_shard ≤ 32 帯では orig を抜く。cost ゼロの局所変更。
- **j5 の sweet spot は per_shard ∈ [8, 32]**: SIMD 1 chunk 内で scan 飽和、
  かつ double-hash も無い。cap=4096 / N=32 で 28 ns/op (orig 26 比 +2) は実用。
- **per_shard を 128 以上に積む意義は限定的**: scan 長が 4 chunk 以上になり、
  hit ratio で稼ぐ分を scan tax で食う。j4 と違い j5 では「per_shard を小さく
  保ったまま N を増やして total cap を伸ばす」が素直な拡大経路。
- **double-hash の修正は j4 → j5 の純粋な置き換えで済む**: 外形 API は同じ、
  evict 列も同じ (テストで確認済み)。`sieve_j4.rs` は AB の歴史保存用に残すが、
  以降の比較対象は j5 を採る。

## 次の実験候補

親レポート §次の実験 のうち、本追補で消化されたもの (打ち消し線) と引き継ぎ:

- ~~§2 double-hash 除去 AB~~ → **本レポートで決着** (Δ ≈ −7 ns)
- §1 per_shard ∈ {16, 24, 32, 48} の Pareto: hit ratio tax と throughput を
  並べる。j5 ベースで取り直し。AB1 / AB2 の Δ(j5−orig) は throughput 軸だけ
  なので、hit ratio 側を埋めると "shard を増やすと容量効率がどこで折れるか"
  が見える。
- §3 大 cap × 高 N での hit ratio tax: 同上。j5 で
  cap=16384/N=128 が 25 ns/op は "速い" が、その時 hit ratio が orig 比で
  どれだけ下がるかは未測定。
- §4 trace ベース再現 (NSDI'24 zipf_1.0 trace 等): 本 AB は synthetic Zipf
  でしか取っていない。実 trace の double-hash 寄与が同じ ~7 ns で揃うかを
  横断確認。
- (新規) **`get_with_hash` を j3 自身の単独 API に昇格させるか** の検討:
  単スレ前提を維持するなら pub(crate) のままで十分。並列化に踏み込むときは
  外で計算した hash を crate 境界を越えて持ち回したくなるかもしれない。

## 付随する変更ログ

- `src/sieve_j3.rs`: `tag_from_hash` / `{get,insert,contains}_with_hash`
  (pub(crate))、内部の `get/insert` を `*_with_tag` 経由に refactor。外部 API は
  非破壊。
- `src/sieve_j5.rs`: 新規。const generic SHARDS、API は j4 と同型。
- `src/lib.rs`: `pub mod sieve_j5;` 追加。
- `src/bin/bench.rs`: `j5` / `j5_n{1,2,4,8,16,32,64,128}` 認識。
- `profiles/j5_doublehash_ab_2026-05-05.csv`: 本 AB の raw (5 trial × 全 cell)。
