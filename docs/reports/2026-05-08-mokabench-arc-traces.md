# mokabench 由来の ARC trace を bench に取り込む

## 目的

`research/src/bin/bench.rs` の workload は (1) 合成 Zipf、(2) NSDI'24 同梱 zipf テキスト、
(3) Twitter cache trace (OSDI'20)、(4) libCacheSim CSV の 4 系統だった。memory feedback
「perf-gate には多様な workload が必要」に沿って、これらと独立な軸として **ARC paper
trace** を追加する。狙いは三つ:

1. Zipf / Twitter とは独立な workload で HR/速度の cross-check を増やす
2. SIEVE 系と W-TinyLFU 系で得手不得手が出やすい scan-heavy / loopy アクセスパターン
   (S3, OLTP) を持ち込む
3. 将来 perf-gate (`sieve_cache_perf.rs`) に scenario を足したくなった時の供給源にする

## 取り入れ方の方針

mokabench (<https://github.com/moka-rs/mokabench>) の load generator 本体は
**統合せず、trace 形式とパーサ意味論だけ拝借する**。理由:

- mokabench 側は cache 実装を **compile-time feature flag で差し替える** 構造のため、
  senba を載せるには mokabench を fork して driver を生やす必要がある。我々の
  experimental 群 (j7/j8/c8/c11s/.../senba::Cache) を全部回したいので、mokabench
  本体の枠は窮屈。
- 既存 `bench.rs` driver には senba, moka, mini-moka, sieve_orig, j7/j8 系がすでに
  載っている。trace reader だけ追加すれば全 variant がそのまま ARC を食える。
- mokabench の `src/parser.rs::GenericTraceParser` は 50 行弱のシンプルな仕様
  (`start len` または `start` 単独 → `start..start+len` 展開)。再実装コストが低い。

trace dataset 自体は `cache-trace` submodule (<https://github.com/moka-rs/cache-trace>)
にあり、mokabench から transitively pull できる。我々は `external/mokabench/` を
submodule として足し、その下の `cache-trace/` を `--depth 1` で取得する形にした。

## 実装 (本コミット範囲)

- `external/mokabench/` を submodule に追加 (cache-trace を含む)
- `senba-research` に `zstd = "0.13"` 依存を追加 (`.lis.zst` を直接読むため)
- `research/src/workload/file.rs::arc_from_path` を新設
  - `start len` を `flat_map` で `start..start+len` に展開
  - 拡張子 `.zst` で zstd Decoder を被せる auto-detect
  - `spc1likeread` の split zst (`.zst.00`/`.zst.01`/...) は非対応
    (連結 reader が必要、現時点で需要なし)
- `bench.rs` に `--source arc` を追加。既存の u64 経路 (senba / moka / mini-moka /
  orig / j7 / j8) がすべてそのまま使える

外部 submodule 依存になるため、CI 通過のためには **default-off feature gate** が
本来は欲しい。現状はテストが arc trace を触らないので未 gate のまま。テストに組む
段階で `external-traces` 同様の取り回しに揃える。

## 初期スモーク

比較対象は **mini-moka** (W-TinyLFU の同期版実装、moka 0.12 のような background
thread / pending tasks の overhead が無く、`run_pending_tasks` 同等の `sync()` を
明示呼び出しする条件で並列化オーバーヘッドが乗らない)。senba 側は
`senba::Cache<u64,u64>` (Slot32 default、SHARDS は per-shard ≤ 64 制約に収まるよう
選択)。ns/op は `bench.rs` の get-then-insert ループ全体を `Instant::now` で計測した
もので、`sync()` / `run_pending_tasks()` を含む。

### Zipf (skew=1.0, keys=100k, len=500k, cap=4096)

| variant | HR | elapsed |
|---|---|---|
| mini_moka | 70.4% | 171 ms |
| senba_n128 | 70.0% | 9.7 ms |

HR は ±0.4pp で一致、ns/op は senba が ~17.6×。基準点の確認のみ。

### ARC OLTP (CODASYL DB 参照、914,145 access)

| cap | mini_moka HR | senba_n128 HR | senba speedup |
|---|---|---|---|
| 1000 | 34.2% | 37.4% | ~17× |
| 4000 | 45.7% | 51.7% | ~14× |

OLTP は **HR で senba (= SIEVE) が上回る**。W-TinyLFU の admission 拒否が
recency 局在の強い DB workload では裏目に出る、という既知パターンと整合。

### ARC S3 (search-engine disk read, 先頭 1M access)

| cap | mini_moka HR | senba_n512 HR | senba speedup |
|---|---|---|---|
| 4000  | 0.09% | 0.11% | ~26× |
| 16000 | 0.39% | 0.43% | ~22× |

S3 はキー空間が極めて広い scan-heavy で **どちらも HR が破滅** (1% にも届かず)。
これは「ARC 系を入れた成果」の一つで、Twitter cluster ベースだけ見ていると気付け
ない: 「working set が cap を遥かに超えると、admission policy 差は誤差の範囲で、
何を選んでも当たらない」という事実が一目で出る。**perf-gate に S3 を足す価値は
低い (signal が無い)**、OLTP は signal があるので候補。

## 含意

- **比較対象は moka でなく mini-moka** が適切。moka 0.12 は adaptive window
  sizing + tokio runtime + background thread の overhead が乗っており、
  single-thread bench で測ると senba との速度差が水増しされる
  (前リビジョンの 25-27× は run_pending_tasks 込み計測由来の過大見積もり、
  mini-moka に置き換えても 14-17× の差は残るが、より誠実な数字)。
- ARC trace 4 種 (S3 / DS1 / OLTP / spc1likeread) のうち、**OLTP が perf-gate
  scenario 候補**として最も価値がある。S3 は HR の signal が無く、DS1 と
  spc1likeread は未検証 (DS1 は ERP 系で OLTP と性格が近い見込み、
  spc1likeread は split zst 対応工事が要る)。
- mokabench を fork して senba driver を生やす方向は **当面見送り**。理由は
  上述の通り (本体 trace reader 部分以外は overlap が大きい)。並行 cache の
  比較を厚くしたくなった段階で再検討する。

## Follow-up

- DS1 / spc1likeread に拡張 (spc1likeread は split zst 連結処理が必要)
- ARC trace 利用テストを書く場合は `external-traces` 相当の feature gate に揃える
- perf-gate (`sieve_cache_perf.rs`) に OLTP scenario を載せるかは別途検討
  (criterion は trace I/O を毎回 reload しない仕掛けが要る)
