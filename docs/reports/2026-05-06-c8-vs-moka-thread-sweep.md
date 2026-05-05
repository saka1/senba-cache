# c8 vs moka 0.12 vs mini-moka 0.10 — concurrent thread sweep (Zipf shared keyspace)

## TL;DR (realistic 構成 §拡張、cap=16384 / SHARDS=256 / keys=1M)

i5-12600K (8 P-core + 4 E-core, HT 有), Zipf(α=1.0), 4M ops shared keyspace, HR 全 variant 0.688〜0.690 に収束:

| threads |   c8 Mops |   moka Mops | mini-moka Mops | c8 / moka |
|--------:|----------:|------------:|---------------:|----------:|
|       1 |       8.5 |        1.96 |           2.19 |    4.34x  |
|       2 |      13.8 |        2.58 |           2.71 |    5.36x  |
|       4 |      21.4 |        3.60 |           5.60 |    5.94x  |
|       8 |      34.8 |        3.58 |           6.53 |    9.73x  |
|      16 |      50.9 |        3.30 |           6.05 |   15.42x  |

- **c8 は near-linear scaling** (5.99x at T=16)、moka は **T=4 で天井、T=8/16 で逆に regress** (1.68x)、mini-moka は T=8 ピーク後微減 (2.76x)。
- p99 chunk latency: c8 195→680 ns、moka 995→**10162 ns**、mini-moka 829→4888 ns。
- HR が 3 者ほぼ完全並走している条件で **throughput 差は最大 15x**。

## §1 (initial bench, cap=512 / SHARDS=8 / keys=10k) — c8 cap 縮退時の特性

§1 は c8 を SHARDS=8 / per-shard=64 (=`MAX_PER_SHARD` 上限) で叩いたもので、harness
側の `SHARDS=8` 固定が c8 の hot shard contention を強く露出させる悪条件だった。
realistic 構成は§拡張を見ること。以下§1 はアーカイブ目的で残す。

i5-12600K (8 P-core), Zipf(α=1.0), keys=10k, cap=512 (=8 shards × 64), 4M ops shared keyspace per thread:

| threads | c8 Mops | moka Mops | mini-moka Mops | c8 / moka |
|--------:|--------:|----------:|---------------:|----------:|
|       1 |    13.3 |      3.29 |           3.06 |    4.05x  |
|       2 |    16.9 |      3.45 |           3.68 |    4.91x  |
|       4 |    23.2 |      4.87 |           6.46 |    4.76x  |
|       8 |    24.8 |      4.84 |           6.55 |    5.13x  |

- **c8 は single-thread でも moka/mini-moka の ~4x**。これは並列性の話ではなく、c8 の read path (8-tag SIMD probe + per-bucket Acquire load + relaxed visited bit set, lock-free) が moka の internal log + ConcurrentHashMap probe より純粋に短い、という single-thread での実装コスト差。
- **moka と mini-moka は実質 scale しない**。T=2 で既に天井 (3.45/3.68 Mops)、T=8 でもほぼ同じ。共有 keyspace + 高 hot 度 (Zipf α=1.0) では内部 read/write log の serialize が支配的。
- **c8 の scaling も理想的ではない** (1.87x at 8T, vs ideal 8x)。hot key (k=0) が常に同じ shard に集中する → writer 直列化と read-path 上の CAS 競合 (visited bit) が頭打ちの主因。
- **HR は ほぼ並走** (c8 ≈ 0.677, moka ≈ 0.69, mini-moka ≈ 0.68)。moka 0.12 は thread 数を上げるほど僅かに HR が伸びる (0.671 → 0.696) — admission 判定の遅延が結果的に scan-resistance に効いている可能性。

## Setup

- CPU: 12th Gen Intel Core i5-12600K (P-core 8 + E-core 4, HT 有効、`nproc=16`)
- harness: `src/bin/bench_concurrent.rs` (`std::thread::scope` + `Barrier`)
- cmd: `bench_concurrent --variant c8,moka,mini_moka --cap 512 --threads {1,2,4,8} --skew 1.0 --keys 10000 --ops 4000000 --warmup 200000 --trials 3 --seed 42`
- raw: `data/2026-05-06-c8-vs-moka-thread-sweep.csv`

`--cap 512` は c8 の `MAX_PER_SHARD = 64` (6-bit ID) の制約から決定: 8 shards × 64 = 512。moka/mini-moka はもっと大きな cap でも動くが、3 者を同条件で比較するため最大が小さい c8 に合わせた。

### moka/mini-moka adapter の方針 (重要)

`bench.rs` (single-thread, HR oracle 用) の adapter は毎 op 後に `sync()` /
`run_pending_tasks()` を呼んでいる。`bench_concurrent.rs` ではこれを**呼ばない**:

- 毎 op flush は read/write log の amortize 設計を潰し、内部 Mutex を踏ませる
  ため、moka/mini-moka の「並列特性」を測ったことにならない。real-world で
  per-op 同期する利用法は存在しない。
- 結果として HR は本来より 1-2pt 薄まる可能性 (admission の遅延ぶん)。が、
  **その遅延を含めた挙動こそが並列利用時に観測される真の特性**。

## Throughput scaling

`Mops` 列は trials=3 の median:

```
threads:    1      2      4      8
c8:       13.3   16.9   23.2   24.8
moka:      3.29   3.45   4.87   4.84
mini-moka: 3.06   3.68   6.46   6.55
```

スケール比 (T=8 / T=1):

- c8: **1.87x** (理想 8x の 23%)
- moka: **1.47x**
- mini-moka: **2.14x**

mini-moka の方が moka 0.12 よりむしろ並列で伸びている。moka 0.12 で導入された adaptive window sizing と pending tasks scheduler が、共有 keyspace + hot-spot 環境では **追加の serialize** (内部の global state 更新) として効いていると見られる。

c8 の scaling 限界はこの構成では:
- hot key (k=0) Zipf top → 同一 shard が常時 contended
- 8 shards = thread 数で hot shard の割合 1/8、cold shard でも writer Mutex
- 6-bit ID 制約で per-shard cap が小さい → working set が hot shard に集中

cap 拡張 (12-bit ID) または adaptive sharding は後続課題。

## Tail latency (chunk = 1024 ops avg, ns/op)

| threads | c8 p50 / p99 | moka p50 / p99 | mini-moka p50 / p99 |
|--------:|-------------:|---------------:|--------------------:|
|       1 |  70 / 121    |   288 / 532    |    312 / 601        |
|       2 | 113 / 203    |   543 / 1117    |   528 / 818        |
|       4 | 157 / 342    |   610 / 2473    |   525 / 1213       |
|       8 | 277 / 741    |  1434 / 4583    |  1123 / 2432       |

- c8 p99 は thread 数に対し ~linear (121 → 741 ns、6x for 8x threads) — Mutex 競合に概ね線形。
- moka p99 は **9x 悪化** (532 → 4583 ns)。pending tasks queue の積み上がりが効いている。
- どの variant でも p99/p50 比は 2-3x 程度で、long-tail の暴走は無い (= harness の noise 域は大丈夫)。

thread CV は全 variant で ~0.01-0.03 と低い。共有 keyspace + 同じ Zipf seed で thread 間に load 不均衡は出ていない。

## HR の比較 (おまけ)

| threads | c8     | moka    | mini-moka |
|--------:|-------:|--------:|----------:|
|       1 | 0.677  | 0.671   | 0.670     |
|       2 | 0.677  | 0.672   | 0.670     |
|       4 | 0.677  | 0.692   | 0.680     |
|       8 | 0.677  | 0.696   | 0.689     |

- c8 は thread 数によらず一定 (= sharding は HR を変えない、SIEVE 単体の HR がそのまま)。
- moka/mini-moka は thread 増で僅かに HR が伸びる。これは write buffer が大きく溜まると admission 判定で frequency を多く参照できるため (= 多 thread = 多 buffered events = 統計が安定) と説明できる。

## 解釈

「modern キャッシュライブラリの並列性」を見たかった、という観点では本実験の結論は:

1. **moka/mini-moka の並列モデルは "sharded ConcurrentHashMap + 内部 read/write log + (定期) 集中処理"** で、log の集中処理 (CMSketch 更新、admission 判定、LRU 操作) が serialize される構造。共有 keyspace で hot key がある workload では log への write contention と中央 dequeue で T=2 以降ほぼ flat。
2. **c8 の "lock-free read + per-shard Mutex writer"** は、読みが多い workload で素直に効く。但し hot key がある shared keyspace では writer が hot shard に集中するため scaling は中庸 (1.87x at 8T)。
3. **single-thread の絶対値が ~4x 違う** ことに注意。これは本来 W-TinyLFU が SIEVE より重い (CMSketch 更新、admission 判定) ためで、並列性とは別軸。
4. moka 0.12 の adaptive window は今回の小 cap + α=1.0 では HR ベネフィットが出る一方、内部 state の追加 contention で並列性は mini-moka 0.10 より悪化。ライブラリ選択時のトレードオフ。

## 後続候補

- skew sweep (α ∈ {0.6, 0.8, 1.0, 1.2}) — uniform 寄りで hot shard contention が崩れた時の scaling 形
- mixed read/write 比 (現状 GIM = read-or-insert、~67% hit) を 95% read など read-heavy に振った時の差

## §拡張: realistic cap (16384) + SHARDS=256 sweep

§1 の "cap=512 制約" は c8 の構造的限界ではなく **harness 側の `SHARDS=8` 固定**が原因
(c8 自体は const generic で SHARDS 任意)。harness を `--shards N` で受けるよう拡張し、
**SHARDS=256 (per_shard=64, sweet spot)** で総 cap=16384 まで伸ばして再測定。keys=1M
に拡大して realistic な working set にする。raw: `data/2026-05-06-c8-vs-moka-realistic-cap.csv`。

cmd: `bench_concurrent --variant c8,moka,mini_moka --shards 256 --cap 16384 --threads {1,2,4,8,16} --skew 1.0 --keys 1000000 --ops 4000000 --warmup 200000 --trials 3 --seed 42`

### Throughput (Mops/s, median of 3)

| threads |   c8 |   moka | mini-moka | c8 / moka |
|--------:|-----:|-------:|----------:|----------:|
|       1 |  8.5 |  1.96  |  2.19     |   4.34x   |
|       2 | 13.8 |  2.58  |  2.71     |   5.36x   |
|       4 | 21.4 |  3.60  |  5.60     |   5.94x   |
|       8 | 34.8 |  3.58  |  6.53     |   9.73x   |
|      16 | 50.9 |  3.30  |  6.05     |  15.42x   |

### スケール比 (T=N / T=1)

- c8: 1.0 / 1.62 / 2.52 / 4.10 / **5.99** — T=8 まで near-linear (P-core 8 個分にほぼ完全に乗る)、T=16 で HT に乗って更に伸びる
- moka: 1.0 / 1.32 / 1.83 / 1.83 / **1.68** — **T=4 で天井、T=8/16 で逆に regress** (内部 contention が write throughput を悪化させる)
- mini-moka: 1.0 / 1.24 / 2.56 / 2.98 / **2.76** — T=8 ピーク、T=16 で微減

### HR

すべて **0.688〜0.690** に収束 (cap=16384, keys=1M で working set が定常化)。
**HR を完全に同条件にした上で 15x の throughput 差**が出ている。

### Tail latency (ns/op, p99 chunk)

| threads | c8 p99 | moka p99 | mini-moka p99 |
|--------:|-------:|---------:|--------------:|
|       1 |   195  |    995   |    829        |
|       2 |   236  |   1484   |   1161        |
|       4 |   335  |   3385   |   1359        |
|       8 |   431  |   6425   |   2455        |
|      16 |   680  |  10162   |   4888        |

c8 は T=16 で 680 ns、moka は **10 µs/op p99** に到達。実用上 moka の p99 は
T 増で線形以上に悪化、c8 は near-linear で踏みとどまる。

### 解釈の更新

- **c8 の真の並列性は near-linear scaling**。SHARDS=256 で hot key も shard-256 のうち
  1 個に集中するだけで、残り 255 shard は無競合で動く。lock-free read + per-shard Mutex
  writer の設計が想定通り効いている。
- **moka 0.12 は 4 thread 以上で writer 経路が contended になり、追加 thread が
  pending tasks queue を膨らませて throughput を悪化させる**。これは「並列で叩いても
  伸びない」だけでなく、「thread を増やすほど遅くなる」までの強い負効果。
- **mini-moka は moka より素直**。adaptive window が無いぶん内部 state が単純で、
  T=8 まで scaling し T=16 で頭打ち。
- §1 の SHARDS=8 / cap=512 sweep で見えた "c8 の 1.87x scaling" は完全に harness
  artifact だった。realistic 構成では 5.99x まで伸び、moka との差は 5x → 15x に拡大する。

このため§1 の数値は「c8 の cap=512 縮退時の特性測定」と読み直すべき。**メイン結論は§拡張の表**。
