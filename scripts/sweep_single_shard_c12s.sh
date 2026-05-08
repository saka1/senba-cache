#!/usr/bin/env bash
# c12s (CAS-based slot claim) 評価 sweep。
# 3 variants (c8/c11s/c12s) × 5 threads × 5 workloads × 3 op-mixes × 1 trial = 225 trials。
#
# 比較対象選択:
#   - c8: pre-c10 baseline (writer Mutex)、採否判定の絶対基準
#   - c11s: 直前の親 (writer Mutex + visited 分離 + conditional set)
#   - c12s: 本変種 (writer Mutex 完全排除 + install-at-evicted-pos)
# 採否判定: read-heavy zipf 16T で c12s が c8 を 5%+ 上回るか。
#
# c10s は c11s に置換済みで sweep からは除外 (c11s の親なので c11s 数値で代替可)。
# c9 (Mutex<Shard>) は親 sweep で性能下限が確定しているため除外。
set -euo pipefail

cd "$(dirname "$0")/.."
OUT=docs/reports/data/2026-05-08-c12s-sweep.csv
BIN=./target/release/bench_single_shard

cargo build -p senba-research --release --bin bench_single_shard >/dev/null 2>&1

PRINTED_HEADER=0
: > "$OUT"

CAP=64
KEYS=100000
OPS_PER_THREAD=1000000
WARMUP=80000

for variant in c8 c11s c12s; do
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
