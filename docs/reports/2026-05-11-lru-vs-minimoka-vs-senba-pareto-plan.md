# lru / mini_moka::unsync / senba パレート再計測 — 計画

**Date**: 2026-05-11 / **Status**: planning

## Hypothesis / 動機

- senba は b28105a (cache-line co-location) ほか直近の改良が積まれており、過去の j8-twitter-pareto / mokabench-arc-traces (2026-05-05〜08) 時点とは別物。
- 比較相手として `mini_moka::unsync` (W-TinyLFU) は揃えていたが、最も基本となる **`lru` クレート (jeromefroe/lru-rs)** をまだ pareto に載せていない。LRU を基準線として置くと、SIEVE と W-TinyLFU の優位がより読みやすくなる。
- 全 ARC 14 トレース + Zipf 合成で、HR / throughput の2軸 pareto を取り直す。

## Scope

- **対象**: 単スレッド比較のみ。`senba::Cache` (`Slot32`, `Xxh3Build`)、`lru::LruCache` (`Xxh3Build` 差し替え)、`mini_moka::unsync::Cache` (`Xxh3Build` 既設定済)。
- **範囲外**: 並行ベンチ (`bench_concurrent.rs`)、`moka 0.12::sync` 比較、VTune / asm 解析、新規 senba variant 追加。

## Workload

| Source | Detail |
|---|---|
| ARC | mokabench 同梱 **全 14 トレース** (`external/mokabench/cache-trace/arc/*.lis.zst`)。capacity は trace ごとの `default_capacities` (既存 `--arc-preset` 機構)。 |
| Zipf | α ∈ **{0.8, 1.0, 1.2}**, keys=1,000,000, len=10,000,000, seed=42。capacity sweep = **{1k, 4k, 16k, 64k, 256k}**。 |

各 (variant, source, capacity) を `--repeat 3` 平均、median を採用。全 variant を同一プロセス内で連続実行し、cross-environment 比較はしない (WSL2 environment confound 回避; memory 参照)。

## ハーネス変更

1. `research/Cargo.toml` に `lru = "0.12"` を追加 (BuildHasher 差し替え対応版)。
2. `research/src/bin/bench.rs` に `LruAdapter<K,V>` を追加し `senba_research::CacheImpl` を実装 — `mini_moka::unsync` wrapper と同じ要領で thin に。`lru::LruCache::with_hasher(NonZeroUsize::new(cap), Xxh3Build::default())`。
3. `--variant lru` を dispatch に登録。
4. ARC preset / Zipf 経路は既存実装をそのまま流用。

## 計測 → 出力フロー

```
cargo run --release -p senba-research --bin bench --features external-traces -- \
  --source arc --arc-preset all --variant senba,lru,mini-moka-unsync --repeat 3
cargo run --release -p senba-research --bin bench -- \
  --source zipf --skew 0.8 --keys 1000000 --len 10000000 --seed 42 \
  --capacity 1024,4096,16384,65536,262144 --variant senba,lru,mini-moka-unsync --repeat 3
# (α=1.0, 1.2 も同様)
```

stdout (CSV) を `docs/reports/data/2026-05-11-pareto/` に保存。`scripts/plot_pareto_lru_minimoka_senba.py` (新規) で trace ごとに HR/throughput vs capacity の2枚を出力。

## 成果物

- `docs/reports/2026-05-11-lru-vs-minimoka-vs-senba-pareto.md` (Hypothesis → Action → Result)
- `docs/reports/data/2026-05-11-pareto/*.csv` (生データ保管)
- `scripts/plot_pareto_lru_minimoka_senba.py`
- `docs/reports/index.md` 更新 (1段落, 3–5行)

## 想定される結果と read 方法

- **小 capacity 帯**: SIEVE > W-TinyLFU > LRU (scan 耐性で SIEVE が頭一つ抜ける) を期待。崩れたら senba 改良後の regression 疑い。
- **cap-fits 帯 (working set ≤ capacity)**: アルゴリズム差は消え、throughput 勝負。Xxh3Build 揃えなので hash cost は同条件、senba の SIMD scan が効くか確認。
- **scan-heavy ARC trace (P3 など)**: mini_moka 0.10 (固定 window) が崩れる帯。senba がここで HR を保てるかが最大の見せ場。
