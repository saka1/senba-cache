# NSDI'24 SIEVE 論文の workload レシピと senba-cache とのギャップ

- 作成日: 2026-05-03
- 出典: Zhang et al., *"SIEVE is Simpler than LRU: an Efficient Turn-Key Eviction Algorithm for Web Caches"*, NSDI'24 (<https://yazhuozhang.com/assets/publication/nsdi24-sieve.pdf>)
- 目的: 論文が SIEVE を評価するときに使った workload の構造を 1 ページにまとめ、senba-cache のベンチが何を再現できていて何を再現できていないかを明示する。新しい variant を入れたあとで「で、どう測ればよいのか」と毎回迷わないための reference。

## 1. 論文の access pattern モデル (§2.1)

Web cache の workload は **一般化 Zipfian (power-law)** に従うと観測されている。

- i 番目に人気のあるオブジェクトの相対頻度は 1/i^α
- α は workload の skew (偏り) を決めるパラメータ
- 論文が引用している実測 α 値:

| 出典 | α の範囲 |
|---|---|
| [26] | 0.6 – 0.8 |
| [49] | 0.56 |
| [51] | 0.71 – 0.76 |
| [20] (Meta) | 0.55 – 0.9 |
| [97] (Twitter KV) | 0.6 – 1.5 |

つまり **実 web workload の α は 0.55 – 1.5 で、CDN 系は 1.0 未満が支配的**。論文が後の合成実験で α=0.8/1.0/1.2 を主軸に使うのはこの分布から来ている。

## 2. 評価用 trace と再生モデル (§4.1)

論文本体の評価 (Fig 3 など) は合成 Zipf ではなく **1559 本の実 production trace** を使っている。

| dataset | 種別 | trace 数 | requests | objects |
|---|---|---:|---:|---:|
| CDN 1 (proprietary) | object | 1273 | 37,460 M | 2,652 M |
| CDN 2 (proprietary) | object | 219 | 3,728 M | 298 M |
| Tencent Photo | object | 2 | 5,650 M | 1,038 M |
| Wiki CDN | object | 3 | 2,863 M | 56 M |
| Twitter KV | KV | 54 | 195,441 M | 10,650 M |
| Meta KV | KV | 5 | 1,644 M | 82 M |
| Meta CDN | object | 3 | 231 M | 76 M |

- 再生方法: **closed system + instant on-demand fill**。すなわち get → ヒットなら何もしない / ミスなら直ちに insert で埋める。**「全 insert を投げる」モデルではない**。
- キャッシュサイズ: trace の **footprint (= ユニーク object 数) の % で指定**。論文が代表として可視化するのは **0.1% (small) と 10% (large)** の 2 点 (8 サイズの中から)。

## 3. Synthetic Zipf 実験 (§5.3)

「実 trace は複雑すぎるので調節可能な合成 Zipf も使った」というセクション。SIEVE の挙動の質的分析はこちらが主役。

- 分布: そのまま power-law (Zipf)
- skew α の振り幅: **{0.2, 0.4, 0.6, 0.8, 1.0, 1.2, 1.4, 1.6}** (Fig 9)
- 代表値: **α = 1.0** で cache size を変えながらスイープ (Fig 8)
- adaptivity 実験 (Fig 10): **2 本の Zipf(α=1.0) workload を 50% 地点で接合**してオブジェクト集団が突然切り替わる trace を作る → SIEVE/LRU/ARC/LFU の interval miss ratio を時系列で可視化

### 評価指標

- **miss ratio** (FIFO 比の reduction で正規化することが多い)
- **popular object ratio** = `|H ∩ A_t| / C` ただし H = workload 全体での top-C 頻度キー、A_t = 時刻 t でキャッシュ内にあるキー集合、C = cache size
  - これが高い = "本当に人気の C 個" をどれだけキャッシュに留められているか
- **hand position** (Fig 9c): SIEVE 固有の挙動分析。論理時刻に対して hand が tail から head に向かってどのくらい進んでいるかを描画

## 4. Instruction-count マイクロベンチ (§6.1, Fig 11)

- workload: **合成 Zipf, 100 M requests, 1 M unique objects** (= 100 ops/object 平均)
- α: **0.8, 1.0, 1.2** の 3 点
- cache size: 0.1% (1k objects) と 10% (100k objects)
- 計測: `perf stat` で命令数 / req を集計、no-op cache (hit/miss どちらでも何もしない) のオーバヘッドを引き算
- 結果: SIEVE は LRU 比で最大 -40%、FIFO 比で最大 -24% の命令数

senba-cache の今後の "命令数あるいは ns/req" 比較を paper に並べる場合、**この (100M req, 1M obj, α∈{0.8,1.0,1.2}, cap∈{1k, 100k})** が直接の対応点になる。

## 5. ウチの現状とのギャップ

`benches/micro.rs` (現行 `insert_only` のみ) を論文のレシピと並べて比較:

| 項目 | 論文 (§5/§6) | senba-cache 現状 | ギャップの内容 |
|---|---|---|---|
| **分布** | 一般化 Zipf | 一般化 Zipf (`rand_distr::Zipf`) | 形は OK |
| **α 範囲** | 0.2 – 1.6 (実 workload は 0.55 – 1.5、CDN 主流は < 1.0) | 1.05, 1.2 のみ | **`rand_distr::Zipf` が α > 1 を要求するため α ≤ 1.0 をサンプルできない**。実 web workload が支配的な領域がカバーできていない |
| **trace 長 / footprint** | 100 M req / 1 M obj (= 100×) | TRACE_LEN = 50,000 / ZIPF_KEYS = 50,000 (= 1×) | trace 長が footprint と同じオーダなので、各キーが平均 1 回しか触られない。論文は 100 倍触る前提 |
| **キャッシュ容量比** | footprint の **0.1% / 10%** | 1024 / 16384 vs ZIPF_KEYS=50,000 = **2% / 33%** | 「small」より大きく「large」より大きい。footprint の 33% は eviction がほとんど発生しない領域 |
| **操作モデル** | closed system + on-demand fill (`get` → miss なら `insert`) | 全 `insert` (旧 `mixed_80r_20w` は意味不明として削除済み) | 実 cache の典型動作 (read-through) を測れていない。ヒット時のホットパス (HashMap.get + visited bit set) と eviction 起動レートが乖離 |
| **指標** | miss ratio + popular object ratio + hand position + instructions/req | wall time (ns/op, Mops/s) のみ | quality 指標が無い。oracle test で eviction 列の一致は確認済みなので "正しさ" は担保されているが、"効率" の量的可視化が無い |
| **データソース** | 1559 本の実 trace + 合成 Zipf | 合成 Zipf (rand_distr) + bundled `zipf_1.0` trace 1 本のみ | 実 trace ローダ無し。ただし論文の合成実験はここでも代用可能 |
| **adaptivity 実験** | 2 本の Zipf(α=1.0) を接合 (Fig 10) | 無し | workload-shift トレース生成器が無い |

## 6. ギャップを埋めるために必要な実装パーツ

優先度順:

1. **α ≤ 1.0 をサポートする finite-N Zipf サンプラ** (`src/workload/zipf.rs` を拡張、または `zipf_finite.rs` を追加)
   - CDF テーブル (長さ N の `Vec<f64>`) を 1 度作って、uniform → 二分探索で O(log N)/sample
   - N=1M なら 8MB で前計算 ms オーダ
   - 既存の `ZipfGen` (rand_distr ラッパ、α > 1 制限) は残してもよいし、置き換えてもよい
2. **footprint 比でキャッシュ容量を指定するベンチパラメータ**
   - `benches/micro.rs` の `CAPS = &[1024, 16384]` を、`N_KEYS` を起点に `cap = N_KEYS * 0.001 / 0.01 / 0.1` のように相対化
   - `ZIPF_KEYS` を 1M に上げ、`TRACE_LEN` を ≥ 10M に伸ばすと論文の 100 ops/obj に近づく (ベンチ 1 ケースが秒オーダになることに注意)
3. **on-demand fill モデルのベンチグループ** (`bench_demand_fill`)
   - 全 `insert` ではなく「`get` → ミスなら `insert`」のループ
   - 既存 `insert_only` は残す (現在の比較資産があるので)
4. **(オプション) workload-shift トレース** (Fig 10 再現)
   - `workload::shift::two_zipfs(α, n1, n2, seed1, seed2)` のような generator
   - これはマイクロベンチではなく、`src/bin/` の analysis CLI で interval miss ratio を吐かせて `gnuplot`/python でプロットする方が paper の Fig 10 にそろえやすい
5. **(オプション) popular object ratio 指標**
   - eviction 列の検証は oracle test で済んでいる。並行する variant 群はみな同じ eviction を出すので、相互比較としては冗長
   - 論文と並べた quality 主張をする場面があれば追加検討

## 7. 直近の動き方

このノートは reference であって todo list ではない。実際の作業は以下のいずれかから着手する:

- **(c) 最小コスト案**: 上記 1 + 2 だけを入れて、現 `insert_only` のパラメータを `α ∈ {0.6, 0.8, 1.0, 1.2}` × `cap = footprint * {0.001, 0.01, 0.1}` に組み直す。on-demand fill は当面導入しない。
- **(b) 中規模案**: (c) に加えて 3 (`bench_demand_fill`) を導入。
- **(a) 全部**: 1 – 4 全部。adaptivity の図まで再現する。

論文と「同じ条件で並べた」と主張したいなら最低限 1, 2, 3 が要る。1 と 2 だけだと "Zipf を投げ込んだ insert スループット" の話で、現 `insert_only` の延長線。
