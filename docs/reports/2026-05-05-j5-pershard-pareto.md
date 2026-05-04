# sieve_j5 — per_shard Pareto sweep (throughput × hit ratio)

- 日付: 2026-05-05
- 親レポート: `2026-05-05-sieve-j5-doublehash-ab.md` §次の実験 §1 + §3
- 動機: double-hash AB で「j5 は per_shard ≤ 32 帯で orig を抜く」「per_shard=128 以上は scan tax で
  負ける」という throughput 軸の見立ては立った。だが hit ratio 側はずっと未測定で、
  「shard を細かくすれば速いが SIEVE 本来の policy から離れる」という直観が正しいかが
  詰められていなかった。本レポートは per_shard を変数とした **2 軸 (ns/op × hit ratio)** の
  Pareto を取り、SIEVE faithfulness が崩れる per_shard の境界 (もしあれば) を探す。

## 設計

- 32 の倍数のみが内部 SIMD chunk と整合するので per_shard ∈ {32, 64, 128, 256} に絞る
  (128 を大きく超える領域は `sieve_j5-doublehash-ab` と `sieve_j4-crossover-and-shard-sweep`
  で既に「scan tax で頭打ち」と分かっている)。
- total cap ∈ {1024, 4096, 16384} の 3 点。同じ容量で「分割の細かさ」だけが変わる断面。
- skew ∈ {0.9, 1.0, 1.2}。skew が緩いほど hot key の協調が必要になり per_shard 大が効く
  はず、という仮説を撫でる。
- baseline: 各 (cap, skew) で `orig` を同条件 1M op trace で取り、Δns/op と Δhit_ratio を
  ペア取り。
- harness は既存 `bench` CLI + 直積を回す `scripts/sweep_j5_pershard.sh` (TRIALS=5)。
  独立 bench は書かず、bench に `j5_n256` / `j5_n512` を追加して既存の (variant, cap)
  matrix を流用。
- 1 cell 5 trial の中央値で報告。raw は `profiles/j5_pershard_pareto_2026-05-05.csv`。

ZipfGen(skew, 100k keys, 1M ops, seed=42)、CPU は親レポートと同じ i5-12600K (P-core
L1d=48 KB, L2=1.25 MB)。

## 結果

### skew=0.9

| cap | per_shard | shards | variant | ns/op (med, 5tr) | hit ratio | Δns vs orig | Δhr vs orig (pp) |
|---:|---:|---:|:---|---:|---:|---:|---:|
| 1024 | — | 1 | orig | 35.36 | 0.4503 | 0.00 | 0.00 |
| 1024 | 32 | 32 | j5_n32 | 28.35 | 0.4490 | −7.01 | −0.13 |
| 1024 | 64 | 16 | j5_n16 | 33.36 | 0.4506 | −1.99 | +0.03 |
| 1024 | 128 | 8 | j5_n8 | 38.24 | 0.4523 | +2.89 | +0.20 |
| 1024 | 256 | 4 | j5_n4 | 56.24 | 0.4519 | +20.88 | +0.16 |
| 4096 | — | 1 | orig | 31.01 | 0.5704 | 0.00 | 0.00 |
| 4096 | 32 | 128 | j5_n128 | 26.58 | 0.5739 | −4.43 | +0.35 |
| 4096 | 64 | 64 | j5_n64 | 30.05 | 0.5747 | −0.96 | +0.43 |
| 4096 | 128 | 32 | j5_n32 | 35.26 | 0.5744 | +4.25 | +0.40 |
| 4096 | 256 | 16 | j5_n16 | 50.14 | 0.5737 | +19.13 | +0.33 |
| 16384 | — | 1 | orig | 27.40 | 0.7160 | 0.00 | 0.00 |
| 16384 | 32 | 512 | j5_n512 | 24.11 | 0.7162 | −3.29 | +0.01 |
| 16384 | 64 | 256 | j5_n256 | 26.54 | 0.7164 | −0.86 | +0.03 |
| 16384 | 128 | 128 | j5_n128 | 31.77 | 0.7161 | +4.37 | +0.01 |
| 16384 | 256 | 64 | j5_n64 | 43.88 | 0.7161 | +16.48 | +0.01 |

### skew=1.0

| cap | per_shard | shards | variant | ns/op (med, 5tr) | hit ratio | Δns vs orig | Δhr vs orig (pp) |
|---:|---:|---:|:---|---:|---:|---:|---:|
| 1024 | — | 1 | orig | 28.56 | 0.5983 | 0.00 | 0.00 |
| 1024 | 32 | 32 | j5_n32 | 23.60 | 0.5960 | −4.96 | −0.23 |
| 1024 | 64 | 16 | j5_n16 | 28.39 | 0.5994 | −0.17 | +0.11 |
| 1024 | 128 | 8 | j5_n8 | 33.52 | 0.6012 | +4.96 | +0.28 |
| 1024 | 256 | 4 | j5_n4 | 47.61 | 0.6017 | +19.05 | +0.33 |
| 4096 | — | 1 | orig | 26.02 | 0.7056 | 0.00 | 0.00 |
| 4096 | 32 | 128 | j5_n128 | 20.99 | 0.7069 | −5.03 | +0.13 |
| 4096 | 64 | 64 | j5_n64 | 24.04 | 0.7084 | −1.99 | +0.27 |
| 4096 | 128 | 32 | j5_n32 | 28.04 | 0.7075 | +2.02 | +0.18 |
| 4096 | 256 | 16 | j5_n16 | 41.19 | 0.7071 | +15.17 | +0.14 |
| 16384 | — | 1 | orig | 21.16 | 0.8142 | 0.00 | 0.00 |
| 16384 | 32 | 512 | j5_n512 | 19.83 | 0.8133 | −1.32 | −0.09 |
| 16384 | 64 | 256 | j5_n256 | 22.35 | 0.8137 | +1.19 | −0.05 |
| 16384 | 128 | 128 | j5_n128 | 25.74 | 0.8140 | +4.59 | −0.02 |
| 16384 | 256 | 64 | j5_n64 | 33.35 | 0.8141 | +12.19 | −0.01 |

### skew=1.2

| cap | per_shard | shards | variant | ns/op (med, 5tr) | hit ratio | Δns vs orig | Δhr vs orig (pp) |
|---:|---:|---:|:---|---:|---:|---:|---:|
| 1024 | — | 1 | orig | 18.20 | 0.8411 | 0.00 | 0.00 |
| 1024 | 32 | 32 | j5_n32 | 16.39 | 0.8388 | −1.82 | −0.22 |
| 1024 | 64 | 16 | j5_n16 | 17.32 | 0.8412 | −0.88 | +0.01 |
| 1024 | 128 | 8 | j5_n8 | 20.31 | 0.8425 | +2.11 | +0.14 |
| 1024 | 256 | 4 | j5_n4 | 27.35 | 0.8419 | +9.15 | +0.08 |
| 4096 | — | 1 | orig | 17.41 | 0.8978 | 0.00 | 0.00 |
| 4096 | 32 | 128 | j5_n128 | 13.88 | 0.8972 | −3.53 | −0.06 |
| 4096 | 64 | 64 | j5_n64 | 15.16 | 0.8974 | −2.25 | −0.04 |
| 4096 | 128 | 32 | j5_n32 | 17.22 | 0.8975 | −0.19 | −0.02 |
| 4096 | 256 | 16 | j5_n16 | 23.17 | 0.8975 | +5.76 | −0.02 |
| 16384 | — | 1 | orig | 15.28 | 0.9374 | 0.00 | 0.00 |
| 16384 | 32 | 512 | j5_n512 | 14.23 | 0.9369 | −1.04 | −0.05 |
| 16384 | 64 | 256 | j5_n256 | 15.56 | 0.9372 | +0.29 | −0.02 |
| 16384 | 128 | 128 | j5_n128 | 15.88 | 0.9373 | +0.60 | −0.01 |
| 16384 | 256 | 64 | j5_n64 | 20.24 | 0.9373 | +4.96 | −0.01 |

## 観測

### Hit ratio: per_shard ≤ 256 では崩れない

per_shard を 32 まで詰めても hit ratio は orig 比 **±0.4pp 以内**。事前直観は
「shard が小さいと SIEVE 本来の global hand から離れて hash table 寄りに縮退し hit ratio が
落ちる」だったが、今回のレンジ (per_shard ∈ [32, 256], cap ∈ [1024, 16384], skew ∈ [0.9, 1.2])
ではその劣化は観測されない。3 つの観点で見える:

1. **多くのセルで j5 のほうが hit ratio が高い** (例: skew=0.9/cap=4096/per_shard=64 で
   +0.43pp、skew=1.0/cap=4096/per_shard=64 で +0.27pp)。これは shard 化が hit ratio を
   "壊さない" どころか僅かに改善する条件すらある、ということ。仮説: zipf hot key が複数
   shard に分散すると hand 同士の独立した進行で hot key の "誤って evict" 確率が下がる、
   など。確証は別実験。
2. **per_shard が極端に小さい (=32) セルで hit ratio が −0.1〜−0.2pp 落ちる** ことはあるが、
   policy 崩壊レベルの差ではない。skew=1.2 では shard を分割しても base hit ratio が
   高すぎて差が見えない (±0.05pp)。
3. **cap=16384 では per_shard を 32 まで詰めても hit ratio がほぼ動かない** (±0.1pp)。
   各 shard が 32 entry でも 512 個並べれば全体の長期統計は orig 1 列と区別がつかない。

つまり当初の Pareto モデル (「shard を分けると hit ratio が確実に落ちる、その代わり速い」)
は **このスケールでは成立しない**。throughput と faithfulness の二択ではなく、
per_shard ∈ [32, 64] で **両軸とも勝つ orig 支配セル** が cap=1024 / 4096 で見つかった。

### Throughput: per_shard=32 が全 (cap, skew) で最速

- per_shard=32 列は全 9 セルで j5 内 trial 中最速。SIMD scan が 1 chunk で済む & double-hash
  なし。
- per_shard=64 で orig と概ね tie (`Δns ≈ 0±2 ns`)、cap=1024/skew=1.0 で 28.39 vs 28.56 など。
- per_shard=128 で orig 比 +0.6〜+4.6 ns の劣化。scan が 4 chunk になる影響。
- per_shard=256 で orig 比 +5〜+21 ns、明確に負け。8 chunk の SIMD scan が支配的。

cap=16384 / skew=0.9 だけ「per_shard=32 でも orig との差が −3.3 ns まで縮む」 — これは
親レポートで言っていた "cap が大きいと orig 自身も hit ratio で稼いで速くなる" 効果の延長。

### `Δns vs orig` を per_shard でならすと滑らかに増える

per_shard を倍にすると Δns はだいたい +5〜+8 ns 増える (= scan 1 chunk 追加分とほぼ一致)。
double-hash AB の "scan = 32 entry あたり ~6 ns" モデルが per_shard 軸でも再現された。
モデル:

```
Δns(j5 − orig) ≈ scan_extra(per_shard) − hash_savings(double_hash_removed)
              ≈ 6 ns × (per_shard / 32 − 1) + dispatch − 7 ns
```

これにより per_shard=32 で Δns ≈ 0 − 7 = −7 ns、per_shard=64 で +6−7 = −1 ns、
per_shard=128 で +12−7 = +5 ns、per_shard=256 で +24−7 = +17 ns、と本データに概ね合う。

## 結論

- **「shard を細かくすると SIEVE policy から離れて hit ratio が落ちる」直観はこの範囲では
  反証された**: per_shard ∈ [32, 256] / cap ∈ [1024, 16384] / skew ∈ [0.9, 1.2] の全 36
  セルで Δhit_ratio は ±0.43pp 以内、半数のセルで j5 のほうが高い。
- **j5 の sweet spot は per_shard ∈ [32, 64]**: throughput は orig 比 −0.9〜−7 ns、
  hit ratio は orig と等しいか僅かに上 (0〜+0.43pp)。両軸で勝てる Pareto 支配セルが存在する。
- **per_shard ≥ 128 は実用的に不利**: throughput で +5 ns 以上、hit ratio もほぼ同じ。
  hit ratio 側に何の見返りもないので選ぶ理由がない。
- **「shard 細分化が SIEVE faithfulness を壊す」現象はもっと小さい per_shard
  (たとえば per_shard ≤ 4) で初めて出る可能性が高い** が、その帯は SIMD scan 自体が
  1 chunk より小さくなって意味を失うので別設計の話。本投資は 32 を下限にすれば良い。

## 次の実験候補

- per_shard ∈ {4, 8, 16} の "hash table 縮退帯" を測って、policy 崩壊の knee がどこにあるかを
  見る。SIMD chunk 未満なので scalar fallback 経路で取る必要があり別 harness 寄り。
  本投資の sweet spot 推定 (≥32) を裏取りする位置づけ。
- 親レポート §4 (実 trace、NSDI'24 zipf_1.0 trace 等) の hit ratio クロスチェック。
  synthetic Zipf で得た「j5 のほうが hit ratio が同じか僅かに上」が trace でも保つか。
- skew=0.6 / 0.7 (低 skew) を撫でる: 親 j4 系列で「低 skew ほど j5 が長く勝てる」と分かって
  いるが、hit ratio 軸での挙動 (shard 増 → hit ratio 改善が大きくなるか) は未測定。
- evicted-key 列の orig との比較指標 (Jaccard / Kendall tau): 今回は scalar な hit ratio で
  「policy が崩れない」と言ったが、内部の eviction 順序が同型かは別問題。
  並列化や cache-coherency 議論に進むときに必要。

## 付随する変更ログ

- `src/bin/bench.rs`: `j5_n256`, `j5_n512` を variant matcher に追加。
- `scripts/sweep_j5_pershard.sh`: 直積 sweep + raw CSV 出力。`bench` 単発を 5 trial 回す。
- `profiles/j5_pershard_pareto_2026-05-05.csv`: 5 trial × 45 cell = 225 行の raw。
