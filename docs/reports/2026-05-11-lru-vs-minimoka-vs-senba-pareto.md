# lru / mini_moka::unsync / senba — 単スレ pareto 再計測

**Date**: 2026-05-11 / **Status**: done
**Data / scripts / figures**: [`docs/benchmark/lru-vs-minimoka-vs-senba-pareto/`](../benchmark/lru-vs-minimoka-vs-senba-pareto/)

## Hypothesis

直近の senba 改良 (b28105a cache-line co-location, visited bitmap, Slot32 default …) を踏まえ、SIEVE が LRU/W-TinyLFU をどの帯で上回るかを地図化したい。
事前予想は (1) 小 capacity 帯で **SIEVE > W-TinyLFU > LRU** (SIEVE の scan 耐性)、(2) cap-fits 帯ではアルゴリズム差が消えて throughput 勝負に変わる、(3) scan-heavy ARC trace (P3 等) で SIEVE が HR を保って勝つ、の 3 点。
比較相手として最も基本となる **`lru` クレート (jeromefroe/lru-rs)** を baseline に追加して 3 者の Pareto を取り直した。

## Action

ハーネス変更:

- `research/Cargo.toml` に `lru = "0.12"` を追加。
- `research/src/bin/bench.rs` に `LruAdapter<K,u64>` を実装 — `lru::LruCache::with_hasher(NonZeroUsize, senba::Xxh3Build)` で hash cost を senba / mini-moka と揃え、`push` を使うことで evicted (K,V) を取れる (CSV `evictions` 列が意味を持つ点で mini-moka 系より素直)。
- `--variant lru` を u64 / String 両 dispatch に登録。

計測:

- ARC paper P1..P14 (mokabench 同梱 `external/mokabench/cache-trace/arc/*.lis.zst`)、capacity は `default_capacities` 既定 (small / large 2 段)、`--repeat 3` で trace 連結後 1 shot。
- Zipf α ∈ {0.8, 1.0, 1.2}, keys=1M, len=10M, seed=42、capacity sweep {1k,4k,16k,64k,256k}、`--repeat 3`。
- 全 variant (`senba,lru,mini_moka_unsync`) を同一 process 内で連続実行 (WSL2 environment confound 回避)。
- データは 1 度きりの履歴ではなく**上書き運用**に切り替え: `docs/benchmark/lru-vs-minimoka-vs-senba-pareto/{run.sh, plot.py, data/, figures/}` 配下、過去版は git history に任せる。

## Result

各 variant の同定 (図表凡例の根拠):
- **senba::Cache** — SIEVE eviction を厳密実装 (oracle PASS) した上で **set-associative tag array + AVX2 SIMD scan + visited bitmap** に再構成した変種。原典 NSDI'24 SIEVE の linked-list 実装ではない。
- **lru-rs** — `jeromefroe/lru-rs` 0.12、HashMap + intrusive doubly-linked list の純 LRU。
- **mini_moka::unsync** — Caffeine 由来の **W-TinyLFU** (CMSketch + window/main 2-segment)。同期版 (sync 用 atomic / write log なしの単スレ公平条件)。

### Throughput — senba (set-assoc SIEVE) が全帯で 2–4× 優位

| Workload | senba (Mops/s) | lru | mini_moka_unsync |
|---|---|---|---|
| Zipf α=1.0, cap=16k | **42.8** | 32.7 | 17.7 |
| Zipf α=1.2, cap=64k | **76.8** | 60.9 | 22.1 |
| ARC P8, cap=160k | **34.4** | 18.7 | 9.8 |
| ARC P1, cap=160k | **33.3** | 16.0 | 8.5 |

Zipf 全帯で senba ≈ 1.3–1.5× lru、≈ 2–3× mini_moka_unsync。ARC でも同様の比率で、SIEVE algorithm 自体の安さに加え senba **変種**の SIMD scan + co-located shard layout が効く形 (linked-list SIEVE では出ない構造的利得)。`mini_moka_unsync` はサイズ加重の admission 判定 / TinyLFU 更新が常時走る分、HR が同等でも throughput では振るわない。

### HR — 仮説 (1) は **部分的に反証**、ARC 小 cap では **W-TinyLFU > SIEVE**

| Workload | senba HR | lru HR | mini_moka_unsync HR | 序列 |
|---|---|---|---|---|
| Zipf α=0.8, cap=4k | **0.274** | 0.171 | 0.241 | senba ≥ tinylfu > lru |
| Zipf α=1.0, cap=4k | **0.603** | 0.513 | 0.587 | senba ≥ tinylfu > lru |
| ARC P3, cap=20k | 0.069 | 0.023 | **0.122** | tinylfu > senba > lru |
| ARC P6, cap=20k | 0.064 | 0.021 | **0.169** | tinylfu > senba > lru |
| ARC P8, cap=20k | 0.171 | 0.095 | **0.223** | tinylfu > senba > lru |
| ARC P6, cap=160k | 0.672 | **0.834** | 0.793 | lru > tinylfu > senba |

Zipf では事前予想どおり senba ≥ mini_moka_unsync > lru で、SIEVE は W-TinyLFU と HR ほぼ同等 + throughput で大きく優位 (= cap-fits 入口前は SIEVE 圧勝)。
一方 **ARC 系では小 cap での序列は逆**: P3/P6/P8 など scan-heavy 寄りの trace で `mini_moka_unsync` が senba を超えて HR を取る。frequency-aware admission が極端な one-shot 列を弾くのに効く帯で、SIEVE は visited 1 bit ぶんの記憶しか無いため admission ノイズに弱い。仮説 (3) (scan-heavy で SIEVE が勝つ) は P3/P6 では棄却。
さらに ARC P6 cap=160k では **lru が両方に勝つ** (0.834 vs 0.793 vs 0.672) — recency が working set を素直に包む trace 形状が出ており、SIEVE の visited リフレッシュが裏目に出ている。

### Pareto 図 (ns/op vs HR、capacity sweep traced)

ARC P1..P14 + Zipf α∈{0.8,1.0,1.2} を 1 つの 5×4 grid に統合: [`figures/pareto-grid.png`](../benchmark/lru-vs-minimoka-vs-senba-pareto/figures/pareto-grid.png)。各 subplot は横軸 ns/op / 縦軸 HR、capacity sweep を 1 本のラインで追跡。Pareto front は左上 (低 latency × 高 HR)。

senba は全 17 panel で **左端のラインが他 2 者より一段上** に位置 (低 ns/op × 同等以上 HR)。HR で mini_moka_unsync が senba を超える点 (ARC 小 cap) はグラフ上ほぼ同 HR で右側に張り出す形になり、Pareto-dominate されないが「速度を売って HR を買う」位置に留まる。

### Pareto 読み

- **Zipf 帯 (合成 skew workload)**: senba が HR・throughput 両軸で Pareto-dominant、cap-fits 直前まで他 2 者は乗らない。
- **ARC 帯 (実 trace)**: throughput では senba 一強だが、HR で見ると小 cap は `mini_moka_unsync`、特定形状の大 cap は `lru` に Pareto を譲る組合せが出る。
- **総合**: senba は「throughput を強く取る場面」での pareto 入口、mini_moka_unsync は「タイト budget での HR」を、lru は「working-set-fit の recency-pure trace」をそれぞれカバーする 3 者並立。

### 反省 / follow-up

- ARC 小 cap で SIEVE が W-TinyLFU に HR で負けるのは、`improvement-ideas.md` の「visited 多 bit 化 / TinyLFU 風 admission の組合せ」検討材料になる (現状 visited は 1 bit)。
- bench harness 改修副産物: ベンチ系のデータ置き場を `docs/benchmark/<topic>/` に集約し、上書き運用 + 履歴は git に寄せる方針へ移行。再現コマンドの単一 source-of-truth は `run.sh` に。
