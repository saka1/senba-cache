# partitioned sweep — `senba::concurrent::PartitionedCache` の (T × N) 比較

設計: [`docs/reports/2026-05-12-partitioned-design.md`](../../reports/2026-05-12-partitioned-design.md)
結果: [`docs/reports/2026-05-12-partitioned-results.md`](../../reports/2026-05-12-partitioned-results.md)

採用領域: **HR drop ≤ 5pp AND Mops gain ≥ +20%** (vs c17s baseline) — `plot.py` の accept zone と同じ。

## 構成

```
partitioned-sweep/
├── run.sh        # 3 stage (baseline → partitioned → r1)
├── plot.py       # heatmap / pareto / scalability 図を生成
├── data/         # results.csv (append-only)
└── figures/      # PNG + summary.md
```

## 実行

```bash
# 全 stage、デフォルト grid (T ∈ {1,2,4,8,16}, N ∈ {1,2,4,8,16}, WAYS ∈ {1,4,16})
./docs/benchmark/partitioned-sweep/run.sh

# 部分実行
STAGES=baseline ./docs/benchmark/partitioned-sweep/run.sh
STAGES=partitioned T_LIST="8 16" N_LIST="4 8 16" ./docs/benchmark/partitioned-sweep/run.sh
```

`senba/concurrent` feature を付けて build される (= `parking_lot::Mutex` 採用)。

## CSV schema (`data/results.csv`)

```
variant,trial,ways,partitions,source,workload_param,op_mix,value,skew,keys,threads,cap,shards,ops,
total_elapsed_ns,aggregate_mops,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv
```

このディレクトリは **上書き運用** (反復ベンチの最新を保持、履歴は git)。
