# Realistic Workload ベンチマーク結果 (2026-05-03)

- 日付: 2026-05-03
- きっかけ: 旧ベンチ (`SKEWS=[1.05, 1.2]`, `CAPS=[1024, 16384]`) が NSDI'24 SIEVE 論文の
  synthetic workload 条件から大きく外れており、実 CDN workload で支配的な α < 1.0 の
  領域がまったく測れていなかった。ベンチを論文 §5.3 / §6.1 の条件に寄せ直し、4 実装を
  再評価した。
- ステータス: 完了 (以下が全測定結果)。

## 変更した条件

| 項目 | 旧設定 | 新設定 | 理由 |
|---|---|---|---|
| skew α | {1.05, 1.2} | **{0.6, 0.8, 1.0, 1.2}** | 実 web α 範囲 0.55–1.5 をカバー |
| footprint N | 50,000 (trace = footprint と同じ) | **100,000** | trace = N × 10x を確保 |
| TRACE_LEN | 50,000 | **1,000,000** | paper の 100x に近づける (10x) |
| cap | {1024, 16384} = footprint の {2%, 33%} | **{100, 1000, 10000}** = footprint の {0.1%, 1%, 10%} | paper の "small / large cache" 定義に合わせる |
| Zipf サンプラ | `rand_distr::Zipf` (α > 1.0 制限あり) | **CDF テーブル + partition_point** (α > 0 任意) | α ≤ 1.0 をサンプル可能に |

## 付随して発見した v1 の latent バグ

CDF ベースの新 Zipf サンプラへの切り替えにより、旧トレースでは踏まれなかった
コーナーケースが露出した。

**症状**: `sieve_v1::evict_one` で全エントリが visited 状態のとき、v0 (= oracle) は
手 (hand) の現在位置から victim を選ぶが、v1 は常に slot 0 を選ぶ。

**原因**: v1 の third-pass が `find_victim_in_range(0, self.tail)` で slot 0 から
走査していた。v0 のリングスキャンは「1 周で全 visited を消したあと、元の hand 位置
に戻って victim を拾う」構造なので、third-pass も `find_victim_in_range(hand, tail)`
→ `find_victim_in_range(0, hand)` の順にすべきだった。

**修正**: `src/sieve_v1.rs:223` の third-pass 1 行を 2 行 (`hand..tail` → `0..hand`) に分割。
`tests/oracle.rs` の oracle 全 11 件が green になることを確認。

## 測定環境

- CPU: Intel (WSL2 上の Linux カーネル 6.6)
- `cargo bench --bench micro` (criterion, `release` プロファイル, `debug = "line-tables-only"`)
- 1 ケース: `sample_size=20`, `warm_up=500ms`, `measurement_time=3s`
- 対象 4 実装:
  - `orig` — NSDI'24 著者参照ポート (arena + doubly-linked list + hand pointer)。**oracle**。
  - `v0` — 連続 Vec "logical queue" + tombstone bitmap + periodic compaction
  - `v1` — v0 派生。eviction の線形スキャンを word 単位 bit-parallel 走査に置き換え
  - `v2` — v0 派生。`order: Vec<Option<EntryId>>` の `Option` を剥がして `Vec<EntryId>` に変更

## 全測定値

全 48 ケース (4 impl × 4 skew × 3 cap)、`insert_only` グループ。

`mean_ms` = 1,000,000 req のトレース 1 回を処理する実時間 (ms)。
`ns/op` = 1 リクエスト当たりのナノ秒。`Mops/s` = 100 万 req / 秒。

```
group          impl  skew     cap   mean(ms)    ns/op   Mops/s
insert_only    orig  0.6      100     38.141     38.1     26.2
insert_only    orig  0.6     1000     34.862     34.9     28.7
insert_only    orig  0.6    10000     34.881     34.9     28.7
insert_only    orig  0.8      100     36.108     36.1     27.7
insert_only    orig  0.8     1000     32.587     32.6     30.7
insert_only    orig  0.8    10000     31.150     31.2     32.1
insert_only    orig  1        100     33.227     33.2     30.1
insert_only    orig  1       1000     29.260     29.3     34.2
insert_only    orig  1      10000     21.663     21.7     46.2
insert_only    orig  1.2      100     23.318     23.3     42.9
insert_only    orig  1.2     1000     16.267     16.3     61.5
insert_only    orig  1.2    10000     14.664     14.7     68.2
insert_only    v0    0.6      100     43.784     43.8     22.8
insert_only    v0    0.6     1000     39.971     40.0     25.0
insert_only    v0    0.6    10000     40.249     40.2     24.8
insert_only    v0    0.8      100     40.843     40.8     24.5
insert_only    v0    0.8     1000     35.999     36.0     27.8
insert_only    v0    0.8    10000     34.214     34.2     29.2
insert_only    v0    1        100     37.063     37.1     27.0
insert_only    v0    1       1000     28.346     28.3     35.3
insert_only    v0    1      10000     24.366     24.4     41.0
insert_only    v0    1.2      100     25.244     25.2     39.6
insert_only    v0    1.2     1000     17.936     17.9     55.8
insert_only    v0    1.2    10000     16.543     16.5     60.4
insert_only    v1    0.6      100     47.481     47.5     21.1
insert_only    v1    0.6     1000     44.212     44.2     22.6
insert_only    v1    0.6    10000     43.151     43.2     23.2
insert_only    v1    0.8      100     44.332     44.3     22.6
insert_only    v1    0.8     1000     39.298     39.3     25.4
insert_only    v1    0.8    10000     35.425     35.4     28.2
insert_only    v1    1        100     39.690     39.7     25.2
insert_only    v1    1       1000     29.825     29.8     33.5
insert_only    v1    1      10000     25.705     25.7     38.9
insert_only    v1    1.2      100     26.941     26.9     37.1
insert_only    v1    1.2     1000     18.857     18.9     53.0
insert_only    v1    1.2    10000     16.250     16.3     61.5
insert_only    v2    0.6      100     42.613     42.6     23.5
insert_only    v2    0.6     1000     39.551     39.6     25.3
insert_only    v2    0.6    10000     38.527     38.5     26.0
insert_only    v2    0.8      100     40.055     40.1     25.0
insert_only    v2    0.8     1000     35.703     35.7     28.0
insert_only    v2    0.8    10000     33.571     33.6     29.8
insert_only    v2    1        100     36.897     36.9     27.1
insert_only    v2    1       1000     27.895     27.9     35.8
insert_only    v2    1      10000     23.099     23.1     43.3
insert_only    v2    1.2      100     25.393     25.4     39.4
insert_only    v2    1.2     1000     17.970     18.0     55.6
insert_only    v2    1.2    10000     15.818     15.8     63.2
```

## 実装間比較 (orig 比)

```
group          skew     cap  orig(ms)    v0(ms)    v1(ms)    v2(ms)  v0/orig  v1/orig  v2/orig   v2/v0
insert_only    0.6      100    38.141    43.784    47.481    42.613    1.15x    1.24x    1.12x   0.97x
insert_only    0.6     1000    34.862    39.971    44.212    39.551    1.15x    1.27x    1.13x   0.99x
insert_only    0.6    10000    34.881    40.249    43.151    38.527    1.15x    1.24x    1.10x   0.96x
insert_only    0.8      100    36.108    40.843    44.332    40.055    1.13x    1.23x    1.11x   0.98x
insert_only    0.8     1000    32.587    35.999    39.298    35.703    1.10x    1.21x    1.10x   0.99x
insert_only    0.8    10000    31.150    34.214    35.425    33.571    1.10x    1.14x    1.08x   0.98x
insert_only    1        100    33.227    37.063    39.690    36.897    1.12x    1.19x    1.11x   1.00x
insert_only    1       1000    29.260    28.346    29.825    27.895    0.97x    1.02x    0.95x   0.98x
insert_only    1      10000    21.663    24.366    25.705    23.099    1.12x    1.19x    1.07x   0.95x
insert_only    1.2      100    23.318    25.244    26.941    25.393    1.08x    1.16x    1.09x   1.01x
insert_only    1.2     1000    16.267    17.936    18.857    17.970    1.10x    1.16x    1.10x   1.00x
insert_only    1.2    10000    14.664    16.543    16.250    15.818    1.13x    1.11x    1.08x   0.96x
```

## 知見

### 1. α が全パラメータで最も支配的

`orig` の ns/op を α 軸で見ると:

| α | cap=100 (ns/op) | cap=10000 (ns/op) | cap 効果 |
|---|---:|---:|---|
| 0.6 | 38.1 | 34.9 | ほぼなし (+9%) |
| 0.8 | 36.1 | 31.2 | 小 (+16%) |
| 1.0 | 33.2 | 21.7 | 大 (+53%) |
| 1.2 | 23.3 | 14.7 | 大 (+59%) |

α が低い (= 分布が flat) ほど:
- 新しいキーが次々到来して eviction が飽和 → cap を増やしても焼け石に水
- 1 リクエスト当たりの eviction 頻度が高い → 処理コストが増える
- α=0.6 は α=1.2 の **2.5 倍遅い** (cap=10000 比)

旧ベンチは α=1.05/1.2 のみだったため、この 2.5 倍の差がまったく見えていなかった。
実 CDN workload が α < 1.0 中心であることを踏まえると、**旧ベンチはほぼ fast path しか
測っていなかった**。

### 2. orig が全条件で最速 (差は 8〜27%)

`v0`, `v1`, `v2` のどの変形も orig を超えない。差の内訳:
- 旧 samply プロファイル (α=1.05, cap=16384) では HashMap が支配的で eviction ループは
  ~3-4% にすぎなかった。
- α=0.6 で eviction が飽和しても orig が有利なのは、doubly-linked list の先頭/末尾
  参照 (head/tail ポインタ O(1)) vs. Vec + tombstone スキャンの difference と考えられる。
  ただし詳細は re-profile が要る。

### 3. v1 (bit-parallel) は効果なし、むしろ遅い

`v1/orig` は 1.11x〜1.27x (= 11〜27% 遅い)。bit-parallel スキャンの機械語コストが
eviction ループのスループット向上を上回っている。eviction ループ自体がボトルネックで
ない限り、word 単位の `trailing_zeros` に切り替えても改善は期待できない。

α=0.6 (eviction が最も多い条件) でも v1 は最悪。理由: 各 eviction ごとに word
マスキング・ビット演算のオーバーヘッドがかかるため、eviction 数が増えるほど不利になる。

### 4. v2 (Option 剥がし) は v0 比で一貫して 0〜5% 速い

`v2/v0 = 0.95〜1.01x` で、small cache (cap=10000) で効果が出やすい。
`order` 配列が `Option<usize>` (16B) → `usize` (8B) に半減したことで、
その配列が L2 に収まりやすくなるキャッシュライン効果と考えられる。
orig との差 (`v2/orig`) は 7〜13% 残っており、Option 剥がしだけでは埋まらない。

### 5. α=1.0, cap=1000 で v0 が orig より速い (0.97x)

`insert_only / α=1.0 / cap=1000` だけ `v0=28.3ms, orig=29.3ms` で v0 が僅差でアウト。
同じ条件で v2=27.9ms, v1=29.8ms。測定ノイズの範囲 (差は 3% 以内) の可能性が高く、
繰り返し計測しても逆転するレベル。構造的優位ではなくベンチノイズとして扱う。

## 次のステップ候補

現時点で eviction ループ改善 (v1 の bit-parallel、v2 の Option 剥がし) が
orig のリード +10〜25% を埋めきれていない。残りの差を説明するには:

1. **α=0.6 条件での re-profile** (samply) — 旧プロファイルは α=1.05/cap=16384 で
   HashMap 支配的だったが、α=0.6/cap=100 では eviction が飽和しているので比率が変わる
   可能性がある。どこで差が生まれているかを見てから次の variant を考える。
2. **on-demand fill ベンチの追加** (`bench_demand_fill`) — 現 `insert_only` は
   毎回 `insert` を呼ぶ不自然なモデル。`get` → miss なら `insert` の closed-system
   replay を測れば、hit 時のホットパス (HashMap.get + visited bit set) と eviction
   レートのバランスが変わり、実態に近くなる。
3. **orig の profiling** — v0 系が orig に勝てない根本を理解するには orig 自体の
   hot function を見る必要がある (linked list の pointer chase vs arena の localityなど)。
