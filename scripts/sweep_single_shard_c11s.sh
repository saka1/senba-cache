#!/usr/bin/env bash
# c11s (conditional visited set) 評価 sweep。
# 3 variants (c8/c10s/c11s) × 5 threads × 5 workloads × 3 op-mixes × 1 trial = 225 trials。
#
# 比較対象選択:
#   - c8: pre-c10 baseline (visited bit を tag 列に同居)
#   - c10s: visited 分離 (c11s の親)
#   - c11s: c10s + conditional set (load → 0 ならだけ fetch_or)
# c9 は親 sweep (2026-05-08-single-shard-baseline) で見たとおり Mutex<Shard>
# 戦略の絶対値が低く scaling shape も別軸で評価済みなので除外。
#
# workload / op-mix 軸は親 sweep と同一に揃える (c10s 数値の cross-check 可能)。
set -euo pipefail

cd "$(dirname "$0")/.."
OUT=docs/reports/data/2026-05-08-c11s-sweep.csv
BIN=./target/release/bench_single_shard

cargo build -p senba-research --release --bin bench_single_shard >/dev/null 2>&1

PRINTED_HEADER=0
: > "$OUT"

CAP=64
KEYS=100000
OPS_PER_THREAD=1000000
WARMUP=80000

for variant in c8 c10s c11s; do
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
