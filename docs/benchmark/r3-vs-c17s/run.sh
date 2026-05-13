#!/usr/bin/env bash
# sieve_r3 (RwLock-based research) vs sieve_c17s (current research perf champion)
# vs senba::concurrent::Cache (lib 0.3.0, Arc+epoch).
#
# 目的: r3 が c17s からの atomic 削減で reader hot path を speed up できるか、
#       かつ Arc+epoch を背負った senba::concurrent からどれだけ取り戻せるかを
#       同一 48-cell grid で測る (senba-concurrent-vs-c17s と axis 同型)。
#
# 軸:
#   variant       : r3 / c17s / senba_concurrent (全部 shards=512 = cap/8 c8x sweet spot)
#   cap           : 4096
#   skew          : 0.8 / 1.0 / 1.4
#   threads       : 1 / 4 / 8 / 16
#   op_mix        : gim / read-heavy
#   value         : u64
#   trials        : 3
#
# Cell 数: 3 skew × 4 threads × 2 mix × 3 variant = 72 invocations × 3 trials = 216 rows.

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

HERE="docs/benchmark/r3-vs-c17s"
DATA="$HERE/data"
mkdir -p "$DATA"

cargo build --release -p senba-research --bin bench_concurrent \
    --features "senba/concurrent" >&2

TRIALS="${TRIALS:-3}"
T_LIST="${T_LIST:-1 4 8 16}"
SKEW_LIST="${SKEW_LIST:-0.8 1.0 1.4}"
MIX_LIST="${MIX_LIST:-gim read-heavy}"
CAP="${CAP:-4096}"
SHARDS="${SHARDS:-512}"

OUT="$DATA/results.csv"
LOG="$DATA/crashes.log"
HEADER="variant,trial,ways,partitions,source,workload_param,op_mix,value,skew,keys,threads,cap,shards,ops,total_elapsed_ns,aggregate_mops,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv"

if [ -f "$OUT" ]; then
  mv "$OUT" "${OUT}.$(date +%Y%m%d-%H%M%S).bak"
fi
echo "$HEADER" > "$OUT"
: > "$LOG"

scale_ops() {
  local cap="$1"
  local ops=$((cap * 4))
  [ "$ops" -lt 2000000 ] && ops=2000000
  [ "$ops" -gt 16000000 ] && ops=16000000
  echo "$ops"
}

scale_warmup() {
  local cap="$1"
  local w="$cap"
  [ "$w" -lt 200000 ] && w=200000
  [ "$w" -gt 4000000 ] && w=4000000
  echo "$w"
}

run_one() {
  local variant="$1" threads="$2" skew="$3" op_mix="$4"
  local ops warmup
  ops=$(scale_ops "$CAP")
  warmup=$(scale_warmup "$CAP")
  warmup=$(( warmup / threads * threads ))
  [ "$warmup" -lt "$threads" ] && warmup="$threads"
  ops=$(( ops / threads * threads ))
  local label="$variant T=$threads skew=$skew mix=$op_mix"
  echo "[$(date +%H:%M:%S)] $label" >&2
  local tmp
  tmp=$(mktemp)
  if ./target/release/bench_concurrent --variant "$variant" \
      --shards "$SHARDS" --cap "$CAP" --ops "$ops" --warmup "$warmup" --trials "$TRIALS" --seed 42 \
      --threads "$threads" --skew "$skew" --keys 100000 \
      --op-mix "$op_mix" --value u64 --ways 1 --partitions 1 \
      --source zipf > "$tmp" 2>&1; then
    tail -n +2 "$tmp" | grep -E "^$variant," >> "$OUT" || true
  else
    local rc=$?
    echo "[$(date +%H:%M:%S)] FAILED (rc=$rc): $label" >> "$LOG"
    tail -20 "$tmp" >> "$LOG"
    echo "---" >> "$LOG"
    tail -n +2 "$tmp" | grep -E "^$variant," >> "$OUT" || true
  fi
  rm -f "$tmp"
}

for variant in r3 c17s senba_concurrent; do
  for threads in $T_LIST; do
    for skew in $SKEW_LIST; do
      for op_mix in $MIX_LIST; do
        run_one "$variant" "$threads" "$skew" "$op_mix"
      done
    done
  done
done

echo "[$(date +%H:%M:%S)] sweep complete: $OUT ($(wc -l < "$OUT") rows incl header)" >&2
[ -s "$LOG" ] && echo "[$(date +%H:%M:%S)] crashes recorded: $LOG" >&2
