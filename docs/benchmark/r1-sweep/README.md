# r1 sweep — shard 間 routing × thread affinity

r1 variant (`research/src/experimental/sieve_r1.rs`) の (T × WAYS × workload × value) 比較計測。
**最終成果物**は `figures/fig_pareto_*.png` の HR vs Mops pareto。

## 設計と判定基準

- 設計仕様: [`docs/reports/2026-05-12-r1-design.md`](../../reports/2026-05-12-r1-design.md)
- 採用領域 (§6.3): **HR drop ≤ 5pp AND Mops gain ≥ +20%** (vs c17s baseline)
- 結果報告: [`docs/reports/2026-05-12-r1-results.md`](../../reports/2026-05-12-r1-results.md)

## 構成

```
r1-sweep/
├── run.sh        # 4 stage の sweep driver (baseline → zipf → twitter → arc)
├── plot.py       # uv + pandas + matplotlib で pareto 図出力
├── data/         # results.csv (append-only)
└── figures/      # plot.py の PNG 出力
```

## 実行

```bash
# 全 stage、デフォルト grid (T ∈ {1,2,4,8,16,32}, WAYS ∈ {1,2,4,8,16})
./docs/benchmark/r1-sweep/run.sh

# 部分実行例
STAGES="baseline zipf" VALUE_LIST=u64 \
  T_LIST="1 4 8 16" WAYS_LIST="1 2 4 8" \
  ./docs/benchmark/r1-sweep/run.sh
```

trace ファイル (`external/NSDI24-SIEVE/libCacheSim/data/twitter_cluster52.csv`、
`external/mokabench/cache-trace/arc/OLTP.lis.zst`) が見つからない stage は自動 skip。

## CSV schema (`data/results.csv`)

```
variant,trial,ways,source,workload_param,op_mix,value,skew,keys,threads,cap,shards,ops,
total_elapsed_ns,aggregate_mops,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv
```

c17s baseline は `variant=c17s, ways=1`、r1 は `variant=r1, ways∈{1,2,4,...}`。
`source = zipf | twitter | arc`、`workload_param` は trace 系のとき `cluster52`、`OLTP` 等。

このディレクトリは **上書き運用** (反復ベンチの最新を保持、履歴は git)。
