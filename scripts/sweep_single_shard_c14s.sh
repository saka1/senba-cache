#!/usr/bin/env bash
# c14s (c13s + 3 点 tuning: find_lockfree AVX2 / MAX_RETRY=1 / reader bounded retry)
# 評価 sweep。3 variants (c11s/c13s/c14s) × 5 threads × 5 workloads × 3 op-mixes × 1 trial = 225 trials。
#
# 比較対象選択:
#   - c11s: writer Mutex baseline (c13s sweep の比較対象、uniform で c14s が ≥ −5% を狙う)
#   - c13s: c14s の親 (skewed zipf のゲインを継承できているか確認)
#   - c14s: 本変種
# 採否判定 (docs/reports/2026-05-08-c14s-design.md §4):
#   - uniform read-heavy 16T: c11s ± 5%
#   - zipf-1.0 / zipf-1.2 read-heavy 16T: ≥ c13s × 0.9
#   - adversarial-hot HR: ≥ 0.85
set -euo pipefail

cd "$(dirname "$0")/.."
OUT=docs/reports/data/2026-05-08-c14s-sweep.csv
BIN=./target/release/bench_single_shard

cargo build -p senba-research --release --bin bench_single_shard >/dev/null 2>&1

PRINTED_HEADER=0
: > "$OUT"

CAP=64
KEYS=100000
OPS_PER_THREAD=1000000
WARMUP=80000

for variant in c11s c13s c14s; do
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
