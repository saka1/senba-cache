# sieve_j4 — per_shard vs total footprint 切り分け (2026-05-05 追補)

- 日付: 2026-05-05
- 親レポート: `2026-05-05-sieve-j4-set-associative.md`、
  `2026-05-05-sieve-j4-crossover-and-shard-sweep.md`
- 動機: 「per_shard ≈ 128 が j3 単独の sweet spot だったのに、j4 で 8 個積むと
  なぜ orig に勝てないのか」という直感のずれを定量で説明する。最初の議論で
  立てた仮説「8 shards × 6 KB ≈ 50 KB が L1d (48 KB) を越えるから kink が
  出る」は、新しい sweep に照らすと **過大評価**だった。本レポートで訂正する。

## 仮説と検証設計

立てた候補:

- **H1 (per_shard 仮説)**: 性能はほぼ per_shard の関数。total cap は per_shard
  経由でしか効かない。j4 で N を増やせば per_shard を保ったまま total cap を
  伸ばせる。
- **H2 (total footprint 仮説)**: 集約フットプリントが L1d (48 KB) を越えると
  shard 切替の L1 miss が支配項になる。per_shard を据え置いても total が
  L1d を越える瞬間に kink を踏む。
- **H3 (定常 overhead 仮説)**: j4 は j3 比で double hash + dispatch が ~10 ns
  乗る固定費。per_shard を揃えれば差はその固定費だけ。

H1 と H2 は **「同じ per_shard で total cap だけ変える」** 実験で分離できる。
SHARDS=8 / cap=1024 (per_shard=128, total 50 KB) と SHARDS=128 / cap=16384
(per_shard=128, total 768 KB) を比べて時間が同程度なら H1、後者が劇的に遅け
れば H2。

## 実験

CPU: Intel i5-12600K (P-core L1d=48 KB, L2=1.25 MB)、`bench` CLI 単発、
ZipfGen(skew, 100k keys, 1M ops, seed=42)。

- **Sweep A**: SHARDS=8 固定、cap ∈ {64..16384}。orig / j3 / j4_n8 を並べる。
  → `profiles/j4_capsweep_n8_2026-05-05.csv` /
    `docs/figures/j4_l1d_sweepA_capfine.png`
- **Sweep B**: cap=256 固定 (total ≈ 12 KB、すべての N で L1d 内)、N ∈ {2..32}。
  → `profiles/j4_smalltotal_shardsweep_2026-05-05.csv` /
    `docs/figures/j4_l1d_sweepB_smalltotal.png`
- **Sweep C**: cap ∈ {1024, 4096, 8192, 16384}、N ∈ {8, 32, 64, 128}。
  per_shard は 8..2048 に散らばり、同じ per_shard を異なる total cap で
  踏める格子を作る。
  → `profiles/j4_pershard_isolation_2026-05-05.csv` /
    `docs/figures/j4_l1d_sweepC_pershard_isolation.png`
- **perf counter**: WSL2 default kernel に `linux-tools` が無く `perf` が
  使えなかったため断念。直接 L1 miss 率を観測する代わりに throughput
  curve の形状で間接判定する。

## 結果 — Sweep A (SHARDS=8, cap 軸)

skew=1.0、ns/op:

| cap | per_shard | total KB | orig | j3 | **j4_n8** |
|---:|---:|---:|---:|---:|---:|
| 64 | 8 | 6 | 34 | 32 | 39 |
| 128 | 16 | 13 | 35 | 37 | 39 |
| 256 | 32 | 25 | 32 | 46 | 35 |
| 512 | 64 | 50 | 30 | 56 | 38 |
| 1024 | 128 | 100 | 27 | 84 | 41 |
| 2048 | 256 | 200 | 29 | 137 | 50 |
| 4096 | 512 | 400 | 26 | 234 | 67 |
| 8192 | 1024 | 800 | 25 | 421 | 86 |
| 16384 | 2048 | 1600 | 23 | 794 | 115 |

j4_n8 は **kink らしい段差を持たず、per_shard の倍々に対してなだらかに増える**。
"L1d boundary で急に劣化する" という予想とは違う形。orig は cap によらず
21–34 ns でほぼ flat (cap が増えると hit ratio が上がって miss path が減る分
むしろ速くなる)。j3 は per_shard ≡ cap なので最も急峻に伸びる。

## 結果 — Sweep B (cap=256, N 軸)

cap=256 / skew=1.0 で N を振る (total は常に ~12 KB ⊂ L1d):

| N | per_shard | ns/op |
|---:|---:|---:|
| orig (ref) | — | 31 |
| 1 (=j3) | 256 | 46 |
| 2 | 128 | 41 |
| 4 | 64 | 39 |
| 8 | 32 | 34 |
| 16 | 16 | 34 |
| 32 | 8 | 34 |

per_shard ≤ 32 で 34 ns に **頭打ち**。これが j4 の "scan を 1 SIMD chunk
未満まで縮め切った後に残る固定費" — double hash + shard dispatch + insert
path 等。orig (31 ns) との差 ~3 ns はほぼ double hash 1 発分。

## 結果 — Sweep C (per_shard 切り分け)

skew=1.0、ns/op:

| cap \ N | 8 | 32 | 64 | 128 |
|---:|---:|---:|---:|---:|
| 1024 | **41** (ps=128) | 32 (ps=32) | 31 (ps=16) | 31 (ps=8) |
| 4096 | 67 (ps=512) | 33 (ps=128) | 30 (ps=64) | 28 (ps=32) |
| 8192 | 86 (ps=1024) | 41 (ps=256) | 32 (ps=128) | 29 (ps=64) |
| 16384 | 115 (ps=2048) | 54 (ps=512) | 39 (ps=256) | **32** (ps=128) |

太字は per_shard=128 を異なる total cap で踏んだケース:

- cap=1024 / N=8 (per_shard=128, total ≈ 50 KB): **41 ns**
- cap=16384 / N=128 (per_shard=128, total ≈ 768 KB): **32 ns**

**total を 15× にしても per_shard が同じなら 32 ns で済み、むしろ速い**
(高 cap で hit rate が上がり miss path コストが減るため)。total footprint が
L1d を越える/越えないでの段差は見えない。H2 は棄却される。

横方向に見ると、対角線上 (per_shard 一定) で値が揃い、per_shard を半分に
すると概ね 30–60% 速くなる、という滑らかな反比例関係。Sweep C のプロットで
4 本の curve が per_shard 軸の上にほぼ重なるのが視覚的に確認できる
(`j4_l1d_sweepC_pershard_isolation.png`)。

## 結論 — モデル更新

採用するのは H1 + H3:

```
ns/op(j4) ≈ const_overhead + scan(per_shard, hit_ratio)
```

- `const_overhead` ≈ 30–35 ns。double hash (5–10 ns) + shard dispatch
  (1–2 ns) + insert/eviction の per-op 平均コスト + arena 操作。
  **orig との 3–5 ns 差** は実質ここに集中している。
- `scan(per_shard, hit_ratio)` は per_shard と miss rate の積で増える。SIMD は
  per_shard ≤ 32 で 1 chunk 未満になるので飽和。per_shard ≥ 64 から線形に
  立ち上がる。
- L1d/L2 階層は **modulator にすぎない**。per op で実際に触る working set は
  「1 shard ぶんの tags + entry array」+「scan で踏むキャッシュライン」で、
  これは per_shard だけで決まる。N が多くても他の shard を **同 op の中では
  触らない** ので、total footprint は cold-cache 的な影響しか持たず、定常
  state では shard 単位の work が支配する。

最初の会話で「8 shards × 6 KB が L1d を越えるから kink」と書いたのは過大評価
だった。実際にはユーザの直感どおり **per_shard が j4 の唯一支配的な性能変数**
であり、cap=1024 / SHARDS=8 で orig に届かなかった理由は

1. per_shard=128 がまだ scan 飽和点 (per_shard ≈ 32) より上にあること
2. j4 が漏れなく払う ~5–10 ns の double-hash 固定費

の二点に集約される。L1d 越えは支配項ではない。

## 実用ガイドライン (今回の発見の帰結)

- **total cap を伸ばしたいときは shard を増やす**。per_shard を据え置く限り
  ns/op はほぼ一定 (むしろ hit rate 改善で速くなる)。Sweep C cap=16384/N=128
  (per_shard=128) で orig 比 1.4× まで詰まったのは、これを実証している。
- **per_shard の sweet spot は ≤ 32** (scan が SIMD 1 chunk 未満)。それより
  小さくしても利益は (ほぼ) 出ない一方、hit ratio tax が顕著になる
  (per_shard=12 で −1.7 pp、親レポート §結果 1)。実用帯は per_shard ∈ [32, 128]。
- **double hash 解消 (親レポート §次の実験 §3)** が効くのは per_shard が小さい
  帯。Sweep B の "34 ns 床" のうち 5–10 ns を削れる。per_shard が大きい帯では
  scan が支配項なので相対効果は小さい。
- **L1d boundary を避けようとして N を膨らませる必要は無い**。i5-12600K で
  cap=16384 / N=128 = total 768 KB > L2 でも 32 ns/op が出る。"shard 単位で
  L1d 内" はまったく要件ではない。

## 次の実験候補 (差し替え)

親レポート §次の実験 のうち、本追補の知見で優先度が変わったもの:

1. **per_shard を本当の sweet spot まで詰めた tradeoff スイープ**: per_shard ∈
   {16, 24, 32, 48} で hit ratio tax と throughput を一緒に取り、Pareto 前縁
   を引く。N と cap を独立に振って per_shard の等値線で集約。
2. **double-hash 除去の AB**: j3 に `pub(crate) fn get_with_hash(&mut self, key,
   hash) -> ...` を追加し、j4 が外側で計算した hash を持ち回す。Sweep B で
   34 ns → 28 ns 程度になれば orig と同等になる予想。
3. **大 cap × 高 N での hit ratio tax**: cap=16384 / N=128 (per_shard=128) は
   throughput では勝ったが hit ratio は未測定。Zipf skew ∈ {0.6, 0.8, 1.0}
   で oracle (orig) との Δ pp を取り、"shard を増やしても trace 全体の
   working set には十分なキャパが残っているか" を確認する。
4. **trace ベース再現** (`zipf_1.0` など NSDI'24 付属 trace) で同じ per_shard
   等値線が出るかを横断確認。Synthetic Zipf 特有でないことを示す。

(親レポートの §「次の実験 §1 shard 数 sweep」「§2 per_shard scaling」は本
追補で先食いされた形。"§5 eviction walk 長分布" は引き続き未着手だが、本
追補の per_shard モデルから予想される形 — per_shard に比例して walk 長が
伸びる — はそのまま生きる。)
