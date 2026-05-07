#!/usr/bin/env bash
# P2 sweep: c8 vs c9 vs moka vs mini-moka
# 4 variants × 5 threads × 3 skews × 2 op-mixes × 3 trials = 360 trials.
# ops scales with thread count so per-thread work stays at 4M (per design doc).
set -euo pipefail

cd "$(dirname "$0")/.."
OUT=docs/reports/data/2026-05-08-c8-vs-c9-thread-sweep.csv
BIN=./target/release/bench_concurrent

cargo build -p senba-research --release --bin bench_concurrent >/dev/null 2>&1

PRINTED_HEADER=0
: > "$OUT"

for op_mix in gim read-heavy; do
  for skew in 0.7 1.0 1.2; do
    for threads in 1 2 4 8 16; do
      ops=$((4000000 * threads))
      echo "=== op_mix=$op_mix skew=$skew threads=$threads ops=$ops ===" >&2
      out=$("$BIN" \
        --variant c8,c9,moka,mini_moka \
        --cap 16384 --shards 256 \
        --threads "$threads" --skew "$skew" \
        --keys 1000000 --ops "$ops" \
        --warmup 200000 --trials 3 --seed 42 \
        --op-mix "$op_mix")
      if [ "$PRINTED_HEADER" -eq 0 ]; then
        echo "$out" | head -1 >> "$OUT"
        PRINTED_HEADER=1
      fi
      echo "$out" | tail -n +2 >> "$OUT"
    done
  done
done
echo "wrote $OUT" >&2
