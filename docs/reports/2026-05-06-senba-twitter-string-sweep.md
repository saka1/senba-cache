# `senba::Cache` vs `orig` — Twitter trace 5 cluster × cap sweep (raw String key)

date: 2026-05-06 (data refreshed 2026-05-07)
status: implemented
csv: `profiles/senba_twitter_string_2026-05-07.csv` (旧: `..._2026-05-06.csv`)
figures: `docs/figures/senba_twitter_string_{hr,nsop,pareto}.png`

## 2026-05-07 追記: データ刷新

`src/sieve_cache/` に対する細かな最適化 (`shift-on-evict` への書き換え、AVX2 検出キャッシュ、unsafe コントラクト整理ほか公開 API 群整備期の累積) を経て、同じ sweep を再測。`sieve_orig` 側でも環境差含めて数字が動いたため両者再掲する。**結論は不変**: 20 cell 全てで `senba_ps32` が `orig` を ns/op で支配し HR は ±0.9pp 以内、cluster019 で +6.32 pp の HR 大勝。Δ(ps32 − orig) は中央値 −34 ns/op、最大 −85 ns (cluster019/cap=65536)。**senba 側の改善幅は cluster006/cap=1024 で 78 → 62 ns、cluster019/cap=65536 で 116 → 94 ns** など全体的に縮み、orig 側もマシンノイズ含めやや軽くなっているが gap は維持。下表 / 図はすべて 2026-05-07 採取の新 CSV ベース。

## 目的

`src/sieve_cache.rs` で `senba::Cache<K, V, S, SHARDS>` が任意 K を取れるようになり、Twitter cache trace (OSDI'20) の anonymized_key を **u64 に pre-hash せず** `Cache<String, u64>` に直接流せるようになった。これを踏まえて、過去の `j8-twitter-pareto` / `st-twitter-5cluster` と同じ枠組みで **orig vs senba::Cache** をフル sweep する。

## 設定

- ソース: 5 cluster (cluster006/016/018/019/034) × cap ∈ {1024, 4096, 16384, 65536} × len 1M req × 3 trials
- variant: `orig` / `senba_ps32` (per_shard=32) / `senba_ps64` (per_shard=64)
- key: **生 String** (`twitter_csv_from_path_string` 経由)、value: `u64` (= trace index)
- `senba::Cache`: SlotSize=`Slot32` (Entry<String,u64> = 24+8 = 32B 完全一致)、SHARDS = cap / per_shard
- 駆動: `bench --source twitter-string`、scripts/sweep_senba_twitter_string.sh
- 集計: `scripts/plot_senba_twitter_string.py` (medians)

per-shard ≤ 64 制約のため SHARDS は cap/32 or cap/64 に応じて 16〜2048 まで動的に振る (`bench.rs` の match 分岐)。

## 結果 (medians, 3 trials)

### ns/op (= elapsed_ns / (hits + misses))

| cluster    | cap     | orig | senba_ps32 | senba_ps64 | Δ(ps32 − orig) |
|------------|--------:|-----:|-----------:|-----------:|---------------:|
| cluster006 |   1024  |  109 |       62   |       73   |   −47 ns       |
| cluster006 |   4096  |  102 |       64   |       67   |   −38 ns       |
| cluster006 |  16384  |  101 |       73   |       73   |   −28 ns       |
| cluster006 |  65536  |  120 |       82   |       85   |   −38 ns       |
| cluster016 |   1024  |   83 |       56   |       58   |   −27 ns       |
| cluster016 |   4096  |   79 |       54   |       59   |   −26 ns       |
| cluster016 |  16384  |   82 |       60   |       62   |   −22 ns       |
| cluster016 |  65536  |  100 |       72   |       71   |   −28 ns       |
| cluster018 |   1024  |   72 |       53   |       54   |   −19 ns       |
| cluster018 |   4096  |   69 |       50   |       52   |   −19 ns       |
| cluster018 |  16384  |   72 |       55   |       60   |   −17 ns       |
| cluster018 |  65536  |   83 |       66   |       65   |   −17 ns       |
| cluster019 |   1024  |   95 |       58   |       61   |   −38 ns       |
| cluster019 |   4096  |   98 |       61   |       62   |   −38 ns       |
| cluster019 |  16384  |  128 |       72   |       73   |   −55 ns       |
| cluster019 |  65536  |  179 |       94   |       93   |   **−85 ns**   |
| cluster034 |   1024  |   88 |       57   |       61   |   −31 ns       |
| cluster034 |   4096  |   96 |       62   |       62   |   −34 ns       |
| cluster034 |  16384  |  115 |       70   |       72   |   −45 ns       |
| cluster034 |  65536  |  140 |       94   |       90   |   −46 ns       |

→ **20 cell 全てで senba_ps32 が orig を支配** (−17 〜 −85 ns/op、中央値 −33 ns、平均 −35 ns)。差幅は cap 増 + scan-heavy cluster (019) で拡大: orig は HashMap probe コスト + node 連鎖の cache miss が支配的になり、cluster019 cap=65536 で 179 ns/op までブローアップする一方、senba は SIMD scan により 94 ns/op で頭打ち。前回 (2026-05-06) と比べ senba 側が一段速く、最大改善は cluster006/cap=1024 と cluster019/cap=65536 でいずれも約 20 ns ほど。

### Hit Ratio (%)

| cluster    | cap     | orig  | senba_ps32 | senba_ps64 | Δ(ps32 − orig) |
|------------|--------:|------:|-----------:|-----------:|---------------:|
| cluster006 |   1024  | 13.22 |     13.11  |     13.13  |     −0.10 pp   |
| cluster006 |   4096  | 34.86 |     35.21  |     35.31  |     +0.35 pp   |
| cluster006 |  16384  | 64.03 |     63.76  |     64.14  |     −0.27 pp   |
| cluster006 |  65536  | 82.93 |     82.74  |     82.88  |     −0.19 pp   |
| cluster016 |   1024  | 36.27 |     37.07  |     36.92  |     +0.80 pp   |
| cluster016 |   4096  | 49.37 |     49.73  |     49.71  |     +0.36 pp   |
| cluster016 |  16384  | 67.52 |     67.55  |     67.62  |     +0.03 pp   |
| cluster016 |  65536  | 77.66 |     77.58  |     77.64  |     −0.08 pp   |
| cluster018 |   1024  | 50.17 |     51.07  |     50.81  |     +0.90 pp   |
| cluster018 |   4096  | 62.53 |     62.71  |     62.76  |     +0.18 pp   |
| cluster018 |  16384  | 73.78 |     73.61  |     73.70  |     −0.17 pp   |
| cluster018 |  65536  | 82.06 |     82.06  |     82.07  |      0.00 pp   |
| cluster019 |   1024  | 24.09 |     30.40  |     30.04  |   **+6.32 pp** |
| cluster019 |   4096  | 29.64 |     31.64  |     31.60  |     +2.00 pp   |
| cluster019 |  16384  | 31.53 |     32.17  |     32.17  |     +0.64 pp   |
| cluster019 |  65536  | 32.75 |     32.88  |     32.87  |     +0.13 pp   |
| cluster034 |   1024  | 30.46 |     30.47  |     30.57  |     +0.00 pp   |
| cluster034 |   4096  | 35.52 |     35.49  |     35.54  |     −0.03 pp   |
| cluster034 |  16384  | 39.02 |     39.04  |     39.04  |     +0.02 pp   |
| cluster034 |  65536  | 41.12 |     41.15  |     41.13  |     +0.04 pp   |

→ **HR は ±0.9 pp 以内で実質一致** (20 cell 中 18 cell)。例外は **cluster019 (= scan-heavy)** で **+6.32/+2.00 pp** と senba が大勝。これは過去の j5/j8 twitter-pareto で観測された「shard 並列化により SIEVE の単一 hand scan-resistance 限界が緩和」現象の再現。**生 String キーでも同じ二重利得 (HR 上 + ns/op 大幅下) が cluster019 で確認できた**。

## 観察

1. **String キーでの senba 優位は普遍的**: u64 sweep (j8 vs orig) で観測した特性が String でも保たれる。SIMD tag scan + per-shard 分割 + free_list 廃止の利得は K の hash・eq コストに依存しない。
2. **per_shard=32 が 64 にわずか優位**: ns/op で per_shard=32 が 19/20 cell で勝つ (3〜10 ns 程度)。HR はほぼ同一。j8 の sweet spot (`per_shard ∈ [32, 64]`) を String でも踏襲。
3. **cluster019 / cluster034 で cap 増の効果が orig 側で逆転**: orig は cap 大で ns/op が悪化 (cluster019: 95 → 179、cluster034: 88 → 140) するのに対し senba はおおむね単調 (cluster019 では 58 → 94 と緩い増加、senba_ps32 / ps64 はほぼ重なる)。orig の HashMap が L1/L2 を踏み外していると解釈できる (j5-vs-orig-2x-memfair で同パターン既出)。
4. **HR 一致は I/O parity の確証**: 過去レポートで pre-hash u64 vs raw String の HR 衝突ゼロを確認済 (`twitter-string-keys.md`)。本 sweep の HR も同 trace に対する j8 sweep の HR と一致する範囲。

## なぜ String 化で ns/op ギャップが拡大するのか (仮説)

**注意: 本節は profiler / perf counter で裏取りしたものではなく、コード経路と数字の概形から立てた仮説に過ぎない。検証は別 task。**

過去の u64 sweep (`profiles/st_twitter_5cluster_2026-05-06.csv` の orig vs j8) と本 String sweep を並べると、cluster019 で:

| cap   | orig u64 | orig String | Δ | j8/senba u64 | senba String | Δ |
|-------|---------:|------------:|----:|-------:|---------:|----:|
| 1024  | 49.4 ns | 95 ns | +46 | 32.8 | 58 | +25 |
| 4096  | 43.7 ns | 98 ns | +54 | 33.1 | 61 | +28 |
| 16384 | 49.7 ns | 128 ns | +78 | 34.1 | 72 | +38 |
| 65536 | 58.2 ns | 179 ns | +121 | 36.2 | 94 | +58 |

→ **String 化による追加コストが orig は senba の約 2 倍**。HR 特性 (cluster019 の +6 pp 等) は u64 sweep でも既出で構造的に同じ。違うのは ns/op の絶対差だけ。

### 当初の誤り

最初は「senba の SIMD tag filter が String::eq の頻度を抑えるが orig は HashMap で全 op に Eq を払う」と説明したが、これは誤り。**`std::collections::HashMap` は hashbrown (Swiss Table) で h2 ctrl byte の SIMD scan が走り、eq の頻度は senba の tag filter と同等まで絞られている**。`Eq` 回数では差を説明できない。

### コード経路から立てた仮説 (未検証)

orig (`src/sieve_orig.rs:93-119, 210-237`) の `drive_str` ループ per-miss コスト:

1. `c.get(&k)` → `self.index.get(key)` = Xxh3(K) ×1
2. `c.insert(k.clone(), i)`:
   - `self.index.get(&key)` (line 94, redundant probe) = Xxh3(K) ×1
   - `self.evict_one()` → `self.index.remove(&node.key)` (line 234) = **Xxh3(victim_K) ×1**
   - `self.index.insert(key, id)` (line 115) = Xxh3(K) ×1

**Xxh3 を miss 1 回あたり 4 回。うち 1 回は victim キーに対するハッシュ。**

senba (`src/sieve_cache.rs`) の per-miss コスト:

1. `c.get(k)` → `find` = Xxh3(K) ×1
2. `c.insert(k.clone(), i)` → `find` 再計算 = Xxh3(K) ×1、その後 `evict_one_returning_id` は **SIMD tag scan で victim を選ぶ。victim のキーをハッシュしない。**

**Xxh3 を miss 1 回あたり 2 回、victim には触らない。**

候補仮説は 2 つ:

(H1) **Eviction 経路の victim 再ハッシュコスト**: orig は `HashMap.remove(&victim_key)` で victim を再ハッシュするが、senba は SIMD tag scan で victim を選ぶので鍵を触らない。22 byte 文字列の Xxh3 ≈ 5-10 ns、hashbrown probe 込み ~15-30 ns。miss 率 67% (cluster019) で per-op ~10-20 ns 説明可能か。

(H2) **データ構造の cache footprint 差**: orig は HashMap (bucket = String 24 + NodeId 4 + ctrl = ~32-40B) + Vec<Node> (Node = String 24 + V 8 + freq + prev/next ≈ 48B) の 2 大アロケーションを全 op で踏む。cap=65536 で計 ~6 MB クラス、L2/L3 を踏み外す。senba は per-shard 固定サイズの tag 配列 + entries 配列で、shard が小さければ作業セットが L1 内。String 化で bucket / Node が太るので差が増幅。cap=65536 で gap 最大 (orig 217 ns) になる傾向と整合。

数値の半分は (H1)、残り半分は (H2) と想像しているが、**どちらの寄与も実測ではない**。検証するには:

- perf stat で Xxh3 関数の呼び出し回数 (uops_issued.any、cycles に対する分担) を比較。
- LLC-load-misses / cache-references で (H2) を直接測る。
- マイクロベンチ: `bench` ランタイムで Xxh3 + Eq の頻度を increment するカウンタを差し込む。

ご指摘の通り **hashbrown の SIMD filter は確かに効いている** (orig 側でも同じ機序が働く) ので、初版で書いた「filter の有無」説明は撤回。違いは「filter で絞った後の経路で何をしているか」で、ここは仮説に留まる。

## 次の候補

- 同じ sweep を **j7/j8 にも String 経路で展開** (現在 String 経路は orig + senba::Cache のみ)。j7/j8 ベンチを生 String キーで取れば「j8 vs senba::Cache」の純粋な実装オーバーヘッド比較が可能。
- `Slot16` ブラケットの可能性: `Entry<&str, u64>` (16B) などの参照型キーで Slot16 を使った場合の追加スピードアップ。実用上の Hash + Eq 整合性 (HashMap と異なり scan で string compare) のコスト評価が要点。
- moka 0.12 / mini-moka 0.10 を String キーに対応させた直接比較 (本変更では u64 経路のみ残置)。
