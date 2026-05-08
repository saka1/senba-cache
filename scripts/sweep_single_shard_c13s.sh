#!/usr/bin/env bash
# c13s (senba::Cache lineage + lock-free Path A) 評価 sweep。
# 4 variants (c8/c11s/c12s/c13s) × 5 threads × 5 workloads × 3 op-mixes × 1 trial = 300 trials。
#
# 比較対象選択:
#   - c8: pre-c10 baseline (writer Mutex)、採否判定の絶対基準
#   - c11s: writer Mutex + visited 分離 + conditional set (前々親)
#   - c12s: writer Mutex 完全排除 (lock-free) だが install-at-evicted-pos で SIEVE 等価性破壊 (不採用)
#   - c13s: senba::Cache lineage (shift-on-evict) + Path A だけ lock-free、Path B/C は writer Mutex
# 採否判定: read-heavy zipf 16T で c13s が c11s を上回り、かつ HR が c11s と一致するか。
set -euo pipefail

cd "$(dirname "$0")/.."
OUT=docs/reports/data/2026-05-08-c13s-sweep.csv
BIN=./target/release/bench_single_shard

cargo build -p senba-research --release --bin bench_single_shard >/dev/null 2>&1

PRINTED_HEADER=0
: > "$OUT"

CAP=64
KEYS=100000
OPS_PER_THREAD=1000000
WARMUP=80000

for variant in c8 c11s c12s c13s; do
  for op_mix in read-only read-heavy gim; do
    for workload_spec in "zipf:0.7" "zipf:1.0" "zipf:1.2" "adversarial-hot:_" "uniform:_"; do
      workload="${workload_spec%%:*}"
      skew="${workload_spec##*:}"
      for threads in 1 2 4 8 16; do
        ops=$((OPS_PER_THREAD * threads))
        if [ "$workload" = "adversarial-hot" ]; then
          keys=1
        else
          keys=$KEYS
        fi
        skew_arg="1.0"
        if [ "$workload" = "zipf" ]; then
          skew_arg="$skew"
        fi
        echo "=== $variant $workload skew=$skew_arg op_mix=$op_mix threads=$threads ===" >&2
        out=$("$BIN" \
          --variant "$variant" \
          --workload "$workload" --skew "$skew_arg" \
          --cap "$CAP" --threads "$threads" \
          --keys "$keys" --ops "$ops" \
          --warmup "$WARMUP" --trials 1 --seed 42 \
          --op-mix "$op_mix")
        if [ "$PRINTED_HEADER" -eq 0 ]; then
          echo "$out" | head -1 >> "$OUT"
          PRINTED_HEADER=1
        fi
        echo "$out" | tail -n +2 >> "$OUT"
      done
    done
  done
done
echo "wrote $OUT" >&2
