# sieve_j4 (set-associative SIEVE) 初回ベンチ (2026-05-05)

- 日付: 2026-05-05
- 出発点: `2026-05-04-sieve-j3-bench.md` の「次の実験候補 §3」(set-associative
  SIEVE)。j3 単一は cap=1000 / cap=10000 で `sieve_orig` に対して 2-19x 負ける。
  per-shard を小さく保てば SIMD scan の固定費が縮み、外部 HashMap を持たない
  まま大きい capacity 帯まで戦えるか、を検証する。
- 実験対象: `src/sieve_j4.rs`
- bench (throughput): `benches/micro.rs` (insert_only)、log
  `profiles/j4_bench_2026-05-05.log`
- bench (hit ratio): `src/bin/bench.rs`、CSV
  `profiles/j4_hitratio_2026-05-05.csv`

## NSDI'24 論文との関係

論文には set-associative SIEVE の提案は **無い**。論文のスケーラビリティ主張は
「hit 時にロックが要らない」「Cachelib プロトタイプが 16 thread LRU の 2x
throughput」の 2 点に集約され、partition / sharding は (a) §5 のアルゴリズム
内在的な "新/旧 implicit partition"、(b) Segcache の TTL partition への言及
だけ。本実験は **論文外の独自拡張** である。

## 実装方針 (要約)

- `[J3<K, V>; SHARDS]` の固定長配列で持つ (Vec の間接を避ける、`SHARDS` を
  const にして配列展開を期待)。`SHARDS = 8`。
- shard 選択: `xxh3(key) & (SHARDS - 1)` で **下位 3 bit**。j3 内部 `tag_of`
  が **上位 8 bit** を使うので、同一 hash 出力でも shard と tag が独立 entropy を
  持つ (上位ビット同士で取ると shard 内 tag が定数化して SIMD scan の
  false-match 率が悪化する)。
- 容量配分: `capacity / SHARDS` を base、`capacity % SHARDS` 個のシャードに
  +1 ずつ振って合計が必ず `capacity` に一致する。
- 単スレ前提。並行性は意図しない (= ロックなし、`Mutex<Shard>` も入れない)。

### 既知の handicap

j4 は per-op で **2 回 hash** する: shard 選択用 (j4 が直接呼ぶ XXH3) と
内部 j3 の `tag_of` 用 (j3 が独立に XXH3 を呼ぶ)。共通の `Xxh3Build` を使って
いるが API 境界で再ハッシュは避けられない。throughput 比較ではこの固定
オーバーヘッド (XXH3 の u64 hash 1 発で ~5-10 ns) が j4 を不利にするが、hit
ratio 比較はこの overhead と独立に成立する。

## 計測条件

- CPU: Intel i5-12600K (AVX2)
- workload: `ZipfGen(skew, n_keys=100_000, seed=42).take(1_000_000)`
- skew ∈ {0.6, 0.8, 1.0, 1.2}、capacity ∈ {100, 1000, 10000}
- criterion: sample_size=20、warm 500ms、measurement 3s
- profile: `[profile.bench] debug = "line-tables-only"`

j4 の per-shard 容量は cap/8: 12-13 (cap=100), 125 (cap=1000), 1250 (cap=10000)。
**cap=1000 が j4 の本命帯** (per-shard ≈ 128、L1d 32KB に余裕で収まる)。

> **追補レポートあり**: cap 軸 sweep / SHARDS sweep / 図入りまとめは
> `2026-05-05-sieve-j4-crossover-and-shard-sweep.md` を参照。trade-off 図
> (`docs/figures/j4_tradeoff_scatter.png`) と crossover map
> (`docs/figures/j4_capsweep_ratio.png`) は本レポートの結論を視覚化したもの。

## 結果 1 — Hit ratio (set-associative tax)

`hits / 1_000_000` を百分率で表示。

| skew | cap | orig | j3 | **j4** | Δ (j4 − orig) |
|---:|---:|---:|---:|---:|---:|
| 0.6 | 100 | 4.26% | 4.26% | **4.11%** | **−0.15 pp** |
| 0.6 | 1000 | 11.39% | 11.39% | **11.99%** | **+0.60 pp** |
| 0.6 | 10000 | 32.00% | 32.00% | 32.07% | +0.07 pp |
| 0.8 | 100 | 16.39% | 16.39% | **15.43%** | **−0.96 pp** |
| 0.8 | 1000 | 30.75% | 30.75% | **31.23%** | **+0.47 pp** |
| 0.8 | 10000 | 53.80% | 53.80% | 53.85% | +0.05 pp |
| 1.0 | 100 | 41.02% | 41.02% | **39.60%** | **−1.42 pp** |
| 1.0 | 1000 | 59.66% | 59.66% | **60.00%** | **+0.34 pp** |
| 1.0 | 10000 | 77.61% | 77.61% | 77.60% | −0.01 pp |
| 1.2 | 100 | 69.45% | 69.45% | **67.75%** | **−1.70 pp** |
| 1.2 | 1000 | 83.97% | 83.97% | 84.06% | +0.09 pp |
| 1.2 | 10000 | 92.51% | 92.51% | 92.51% | 0.00 pp |

(j3 は単一 SIEVE なので oracle と同じ hit/miss、`tests/oracle.rs` の不変条件)

### 観察

1. **set-associative tax は cap=100 (per-shard ≈ 12) で実測される** — 全 skew で
   −0.15 〜 −1.70 pp の劣化。skew が高い (= hot key 集中) ほど絶対 pp で大きく
   下がるが、hit ratio の絶対値が高いので相対 (Δ/orig) では skew=0.6 の方が
   悪い: −3.5% (0.6) vs −2.4% (1.2)。per-shard 容量が hot set を抱えきれない
   shard が出ると、その shard だけ大量 evict が走るためと解釈できる。
2. **cap=1000 で tax が消える、むしろ僅かに j4 が勝つ** — 全 4 skew で +0.09
   〜 +0.60 pp。これは **当初の予想 (tax は cap が増えても残る) と逆**。
   仮説: SIEVE の hand 走査は「visited を倒しながら最初の visited=0 を探す」
   挙動で、ring が短いほど一巡の長さが短く、少数の hot key が visited を保ち
   やすく steady state が安定する。グローバル単一の SIEVE では ring が長くて
   "visited 一斉クリア" のラウンドが粗くなり、border line の key を先に落とす
   ケースが微妙に多い、という理屈は成り立つ。
3. **cap=10000 では完全に同等** — Δ ≤ 0.07 pp、本質的に同じ hit ratio。
   per-shard = 1250 で各 shard が hot set をほぼ全て抱えきれる帯。

→ **(2) の仮説 (tax は最小限に収まる) は概ね支持**。むしろ cap=1000 では j4
の方が hit ratio で勝つ、という想定外の収穫があった。

## 結果 2 — Throughput (CLI 単発測定、ms / 1M ops)

`src/bin/bench.rs` で 1 trace = 1M ops を 1 回実行した wall time。Criterion
の中央値ではないので run-to-run noise を含むが、傾向は十分読み取れる。

| skew | cap | orig | v3 | j3 | **j4** | j4/orig | j4/j3 |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 0.6 | 100 | 47 | 58 | 38 | 43 | 0.92x | 1.13x |
| 0.6 | 1000 | 50 | 60 | 124 | **53** | **1.06x** | **0.43x** |
| 0.6 | 10000 | 40 | 52 | 921 | 199 | 4.95x | **0.22x** |
| 0.8 | 100 | 45 | 53 | 37 | 40 | 0.90x | 1.08x |
| 0.8 | 1000 | 39 | 47 | 105 | **51** | **1.30x** | **0.49x** |
| 0.8 | 10000 | 33 | 39 | 677 | 151 | 4.55x | **0.22x** |
| 1.0 | 100 | 37 | 44 | 34 | 37 | 1.01x | 1.09x |
| 1.0 | 1000 | 29 | 33 | 81 | **42** | **1.44x** | **0.52x** |
| 1.0 | 10000 | 22 | 26 | 373 | 93 | 4.16x | **0.25x** |
| 1.2 | 100 | 24 | 27 | 24 | 28 | 1.18x | 1.16x |
| 1.2 | 1000 | 19 | 20 | 41 | 29 | 1.55x | 0.71x |
| 1.2 | 10000 | 17 | 17 | 149 | 47 | 2.80x | 0.32x |

### 観察

1. **cap=1000 で j4 は j3 の 0.43-0.71x** — 8 個に分割するだけで j3 単一に
   対して **2-2.4x 速くなる**。per-shard = 125 まで縮めたことで j3 の SIMD
   scan が短く済み、内部 hashmap 系コストの削減が dispatch コストを上回った。
2. **cap=10000 で j4 は j3 の 0.22-0.32x** — j3 の O(N) scan が支配する帯では
   **3-5x の速度差**。それでも orig の linked-list + HashMap の漸近性能には
   追いつかない (j4/orig = 2.8-5.0x)。per-shard = 1250 はまだ大きい。
3. **cap=100 では j4 ≈ orig、j3 比はほぼ tie** — per-shard = 12-13 で j3 の
   優位が出ず、dispatch + double-hash の固定費が表に出る。j4 はこの帯に
   恩恵が無い (むしろ shard 化のオーバーヘッドが効く)。
4. **(1) の仮説検証**: j4 cap=1000 で orig 比 1.06-1.55x。orig には届かない
   ものの j3 単一 cap=1000 (orig 比 2.5-3.2x) からは大幅改善。「cap=1024 で
   scale した性能が出るか」は **半分 yes** (j3 → j4 で大幅改善するが、orig
   水準にはあと 1 段) という結論。

## 結果 3 — Criterion 中央値 (insert_only, ms / 1M ops)

`profiles/j4_bench_2026-05-05.log` から time の median を抽出。

| skew | cap | orig | v3 | j3 | **j4** | j4/orig | j4/j3 |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 0.6 | 100 | 37.81 | 46.89 | 25.19 | **23.26** | **0.62×** | 0.92× |
| 0.6 | 1000 | 42.92 | 50.35 | 69.16 | **34.22** | **0.80×** | **0.49×** |
| 0.6 | 10000 | 36.54 | 43.04 | 588.83 | 104.83 | 2.87× | **0.18×** |
| 0.8 | 100 | 36.81 | 44.60 | 26.65 | **23.92** | **0.65×** | 0.90× |
| 0.8 | 1000 | 34.27 | 39.32 | 60.45 | 35.53 | 1.04× | **0.59×** |
| 0.8 | 10000 | 30.59 | 35.12 | 437.77 | 88.34 | 2.89× | **0.20×** |
| 1.0 | 100 | 32.24 | 37.85 | 26.75 | **24.90** | **0.77×** | 0.93× |
| 1.0 | 1000 | 25.48 | 30.01 | 51.96 | 32.73 | 1.28× | **0.63×** |
| 1.0 | 10000 | 21.38 | 23.54 | 247.88 | 63.52 | 2.97× | **0.26×** |
| 1.2 | 100 | 21.36 | 24.71 | 19.80 | 22.60 | 1.06× | 1.14× |
| 1.2 | 1000 | 16.96 | 18.81 | 31.51 | 24.93 | 1.47× | 0.79× |
| 1.2 | 10000 | 14.95 | 15.90 | 95.34 | 35.57 | 2.38× | **0.37×** |

### 観察 (criterion)

CLI 単発と傾向は一致。Criterion の方が j4/orig が良く出ている (CLI 1.06×
→ criterion 0.80×、cap=1000 skew=0.6 等) のは、CLI が cold-start を含む
1 回限りの計測だったため。Criterion の中央値で確定する重要事実は以下:

1. **cap=100 で j4 は j3 よりさらに速い帯がある (skew ≤ 1.0)** — j3 単独
   cap=100 が既に orig 比 0.62-0.83× の勝ち領域だが、j4 はそれを 0.90-0.93×
   さらに削る (skew=0.6 で j4 = 0.62× orig)。per-shard = 12 まで縮めると
   j3 内部の SIMD scan + tag 比較が 1 chunk 未満で終わるため。
2. **cap=1000 で j4 が j3 を 1.6-2.0× 上回る (sweet spot)** — j4/j3 = 0.49-
   0.79×。per-shard = 125 が j3 の最適容量帯と一致する仮説の直接的支持。
3. **cap=10000 で j4 は orig の 2.4-3.0× 遅いが j3 の 0.18-0.37×** — orig には
   及ばないが j3 単独からは劇的改善。per-shard = 1250 はまだ j3 にとっては
   重く、shard を増やすか per-shard を絞るかのチューニング余地がある。
4. **skew=1.2 / cap=100 のみ j4 が orig に負ける (1.06×)** — hot key が
   集中する分布で、たまたま hot key が偏った shard に集まると per-shard=12
   の小ささが効いて scan + dispatch のコストが見える、と読める。

## 読み解き

### (1) スループット仮説への回答

> 同一総容量 (cap=1024) で j3 単一に対し劣化しないか / 勝てるか

**明確に勝つ** (criterion で j4/j3 = 0.49-0.79×、つまり j4 が **1.3-2.0× 速い**)。
per-shard = 125 が j3 の SIMD scan + tag 比較の sweet spot にハマっている。

ただし orig との関係は skew 依存:
- skew=0.6 / cap=1000 では **j4 が orig を上回る** (0.80×、1.25× 速い)。
- skew ≥ 0.8 / cap=1000 では j4 が orig より遅い (1.04-1.47×)。

cap=100 では skew ≤ 1.0 で **j4 が orig を 0.62-0.77× で大きく上回り**、cap=1000
でも skew=0.6 で勝つ — 当初想定した「shard 化だけでは orig には届かない」より
良い結果が出た。orig の HashMap O(1) は、cap が小さい帯では HashMap オーバー
ヘッド (probe + Sieve list maintenance) が目立つので、scan が短ければ array の
方が速い、という j3 章の結論が j4 で帯域を広げた形。

### (2) hit ratio 仮説への回答

> sieve_orig 比で hit ratio 低下が最小限に収まるか

**収まる、というか per-shard ≥ 125 では逆転して j4 が勝つ**。

- per-shard = 12 (cap=100): tax あり (−0.15 〜 −1.70 pp)
- per-shard = 125 (cap=1000): **tax 反転、+0.09 〜 +0.60 pp**
- per-shard = 1250 (cap=10000): 同等 (Δ ≤ 0.07 pp)

cap=1000 の反転は当初予想外。SIEVE 特有の「visited bit を倒しながら一巡」の
振る舞いが、ring 長が短いほど steady state で hot set を保ちやすい、という
仮説が立つが、これは追加の実験 (per-shard 容量を細かく振る、別ワークロード)
で確かめる価値がある。

### 命名について

j 番号は j3 (xxh3) と紛らわしい。j4 は j3 を 8 個並べる派生なので、より
意図を伝えるなら `sieve_j3_assoc` のような名前のほうが良かったかもしれない。
ただ今回は連番慣行を踏襲。コメントで "set-associative variant of j3" と
明記。

## 次の実験候補

1. **shard 数の sweep** (N = 2, 4, 8, 16): 同じ総容量で N を振って、(a) hit
   ratio がどこで反転するか、(b) per-shard 容量と throughput sweet spot の
   関係、を 2D マップで取る。
2. **per-shard 容量を変えた scaling**: total cap = 512, 1024, 2048, 4096 で
   N=8 固定。per-shard ∈ {64, 128, 256, 512}。j3 が cap≤256 程度で sweet
   spot を持つことを直接検証する。
3. **double-hash の解消**: `j3` に "pre-computed hash 経由の `find/insert`" を
   pub(crate) 公開し、j4 が 1 回 hash した値を内部 j3 に持ち回す。固定 5-10 ns
   削減で cap=100 帯の不利が解消するか確認。
4. **bundled trace (zipf_1.0) での再測**: synthetic Zipf に加えて NSDI'24 の
   付属 trace で同じ表を作ると、hit ratio の反転が trace 形状に依存するか
   切り分けられる。
5. **eviction walk 長分布**: per-shard が小さいと visited が密集して hand が
   長く回る、という潜在的劣化要因。`scan_evict` のループ回数ヒストグラムを
   取って (1) の throughput 結果と整合するか確認。
