#!/usr/bin/env bash
# Single-shard concurrent testbed baseline sweep.
# 3 variants (c8/c9/c10s) × 5 threads × 5 workloads × 3 op-mixes × 1 trial = 225 trials.
#
# 各 trial: ops_per_thread = 1M (= ops = 1M * threads)。multi-trial repeat は
# c10 検証時に増やす。今回は scaling shape を素早く取るのが目的。
#
# workload 軸 (5 点):
#   - zipf skew=0.7  (cold)
#   - zipf skew=1.0  (mid)
#   - zipf skew=1.2  (hot)
#   - adversarial-hot  (key=0 only, visited ping-pong 上限)
#   - uniform          (thread 別 disjoint range, 競合 floor)
#
# op-mix 軸 (3 点):
#   - read-only   (100% get; visited contention の純粋効果)
#   - read-heavy  (95/5; insert を少量混ぜる)
#   - gim         (50/50 想定; insert を多めに混ぜる)
set -euo pipefail

cd "$(dirname "$0")/.."
OUT=docs/reports/data/2026-05-08-single-shard-baseline.csv
BIN=./target/release/bench_single_shard

cargo build -p senba-research --release --bin bench_single_shard >/dev/null 2>&1

PRINTED_HEADER=0
: > "$OUT"

# 単一 shard なので cap は c8 の 6-bit ID 上限 (= 64) 固定。
CAP=64
KEYS=100000  # zipf / uniform 用 key universe
OPS_PER_THREAD=1000000
WARMUP=80000  # threads で割れるよう 80000 を選択 (= 16T で 5000/thread)

for variant in c8 c9 c10s; do
  for op_mix in read-only read-heavy gim; do
    for workload_spec in "zipf:0.7" "zipf:1.0" "zipf:1.2" "adversarial-hot:_" "uniform:_"; do
      workload="${workload_spec%%:*}"
      skew="${workload_spec##*:}"
      for threads in 1 2 4 8 16; do
        ops=$((OPS_PER_THREAD * threads))
        # adversarial-hot は keys=1、uniform は thread 別 disjoint なので keys 大きめ
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
