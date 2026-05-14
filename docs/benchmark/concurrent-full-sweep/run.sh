#!/usr/bin/env bash
# senba::concurrent (v0.4.0, r4-based) vs sieve_c17s (research) vs moka vs
# mini_moka — full-sweep concurrent SIEVE comparison across every trace
# source the project supports.
#
# Phases (run independently via --phase):
#   zipf         synthetic Zipf, cap × skew × threads × mix × value
#   libcachesim  NSDI24-libcachesim (twitter_cluster52 + trace.csv)
#   twitter      Twitter-Yang clusters {006, 016, 018, 019, 034}
#   arc          ARC paper presets (mokabench, 22 traces)
#
# Default runs every phase in order. Pass --phase NAME to run just one.

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

HERE="docs/benchmark/concurrent-full-sweep"
DATA="$HERE/data"
mkdir -p "$DATA"

cargo build --release -p senba-research --bin bench_concurrent \
    --features "senba/concurrent,external-traces" >&2

# --- knobs ------------------------------------------------------------------
PHASE_ARG="all"
while [ $# -gt 0 ]; do
  case "$1" in
    --phase) PHASE_ARG="$2"; shift 2 ;;
    -h|--help)
      echo "usage: $0 [--phase {all|zipf|libcachesim|twitter|arc}]"
      exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

VARIANTS="${VARIANTS:-senba_concurrent c17s moka mini_moka}"
TRIALS="${TRIALS:-3}"
T_LIST="${T_LIST:-1 4 8 16}"
MIX_LIST="${MIX_LIST:-gim read-heavy}"
VALUE_LIST="${VALUE_LIST:-u64 string}"
SHARDS="${SHARDS:-512}"  # cap/8 sweet spot

# Zipf knobs
ZIPF_SKEW_LIST="${ZIPF_SKEW_LIST:-0.8 1.0 1.4}"
ZIPF_CAP_LIST="${ZIPF_CAP_LIST:-4096}"
ZIPF_KEYS="${ZIPF_KEYS:-100000}"

OUT="$DATA/results.csv"
LOG="$DATA/crashes.log"
HEADER="variant,trial,ways,partitions,source,workload_param,op_mix,value,skew,keys,threads,cap,shards,ops,total_elapsed_ns,aggregate_mops,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv"

# Preserve prior output across phases; only initialise on first phase / fresh run.
if [ ! -f "$OUT" ]; then
  echo "$HEADER" > "$OUT"
fi
if [ ! -f "$LOG" ]; then
  : > "$LOG"
fi

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

# Run a single bench_concurrent invocation. Captures stdout into the global
# results CSV; on non-zero exit captures the tail to the crashes log.
run_cell() {
  local label="$1"; shift
  echo "[$(date +%H:%M:%S)] $label" >&2
  local tmp
  tmp=$(mktemp)
  if ./target/release/bench_concurrent "$@" > "$tmp" 2>&1; then
    tail -n +2 "$tmp" | grep -E "^[a-z0-9_]+," >> "$OUT" || true
  else
    local rc=$?
    echo "[$(date +%H:%M:%S)] FAILED (rc=$rc): $label" >> "$LOG"
    tail -20 "$tmp" >> "$LOG"
    echo "---" >> "$LOG"
    tail -n +2 "$tmp" | grep -E "^[a-z0-9_]+," >> "$OUT" || true
  fi
  rm -f "$tmp"
}

# Phase A: synthetic Zipf -------------------------------------------------
phase_zipf() {
  echo "[$(date +%H:%M:%S)] phase: zipf" >&2
  for cap in $ZIPF_CAP_LIST; do
    local ops warmup
    ops=$(scale_ops "$cap")
    warmup=$(scale_warmup "$cap")
    for value in $VALUE_LIST; do
      for variant in $VARIANTS; do
        for threads in $T_LIST; do
          local ops_t=$(( ops / threads * threads ))
          local warmup_t=$(( warmup / threads * threads ))
          [ "$warmup_t" -lt "$threads" ] && warmup_t="$threads"
          for skew in $ZIPF_SKEW_LIST; do
            for op_mix in $MIX_LIST; do
              run_cell "zipf $variant T=$threads skew=$skew mix=$op_mix V=$value cap=$cap" \
                --variant "$variant" \
                --shards "$SHARDS" --cap "$cap" --ops "$ops_t" --warmup "$warmup_t" \
                --trials "$TRIALS" --seed 42 \
                --threads "$threads" --skew "$skew" --keys "$ZIPF_KEYS" \
                --op-mix "$op_mix" --value "$value" --ways 1 --partitions 1 \
                --source zipf
            done
          done
        done
      done
    done
  done
}

# Phase B: NSDI24-libcachesim CSV traces -----------------------------------
phase_libcachesim() {
  echo "[$(date +%H:%M:%S)] phase: libcachesim" >&2
  local sources=(
    "external/NSDI24-SIEVE/libCacheSim/data/twitter_cluster52.csv:twitter_cluster52"
    "external/NSDI24-SIEVE/libCacheSim/data/trace.csv:trace"
  )
  for entry in "${sources[@]}"; do
    local trace_file="${entry%%:*}"
    local label_name="${entry##*:}"
    [ -f "$trace_file" ] || { echo "skip $label_name (not found)" >&2; continue; }
    for cap in 4096; do
      local ops warmup
      ops=$(scale_ops "$cap")
      warmup=$(scale_warmup "$cap")
      for value in $VALUE_LIST; do
        for variant in $VARIANTS; do
          for threads in $T_LIST; do
            local ops_t=$(( ops / threads * threads ))
            local warmup_t=$(( warmup / threads * threads ))
            [ "$warmup_t" -lt "$threads" ] && warmup_t="$threads"
            for op_mix in $MIX_LIST; do
              run_cell "libcachesim/$label_name $variant T=$threads mix=$op_mix V=$value" \
                --variant "$variant" \
                --shards "$SHARDS" --cap "$cap" --ops "$ops_t" --warmup "$warmup_t" \
                --trials "$TRIALS" --seed 42 \
                --threads "$threads" --skew 1.0 --keys "$ZIPF_KEYS" \
                --op-mix "$op_mix" --value "$value" --ways 1 --partitions 1 \
                --source twitter-yang --trace-file "$trace_file" --workload-param "$label_name"
            done
          done
        done
      done
    done
  done
}

# Phase C: Twitter-Yang OSDI'20 clusters -----------------------------------
phase_twitter() {
  echo "[$(date +%H:%M:%S)] phase: twitter" >&2
  for c in 006 016 018 019 034; do
    local trace_file="external/twitter-cache-trace/cluster$c"
    [ -f "$trace_file" ] || { echo "skip cluster$c (not found)" >&2; continue; }
    for cap in 4096; do
      local ops warmup
      ops=$(scale_ops "$cap")
      warmup=$(scale_warmup "$cap")
      for value in $VALUE_LIST; do
        for variant in $VARIANTS; do
          for threads in $T_LIST; do
            local ops_t=$(( ops / threads * threads ))
            local warmup_t=$(( warmup / threads * threads ))
            [ "$warmup_t" -lt "$threads" ] && warmup_t="$threads"
            for op_mix in $MIX_LIST; do
              run_cell "twitter/cluster$c $variant T=$threads mix=$op_mix V=$value" \
                --variant "$variant" \
                --shards "$SHARDS" --cap "$cap" --ops "$ops_t" --warmup "$warmup_t" \
                --trials "$TRIALS" --seed 42 \
                --threads "$threads" --skew 1.0 --keys "$ZIPF_KEYS" \
                --op-mix "$op_mix" --value "$value" --ways 1 --partitions 1 \
                --source twitter-yang --trace-file "$trace_file" --workload-param "cluster$c"
            done
          done
        done
      done
    done
  done
}

# Phase D: ARC paper presets (mokabench) -----------------------------------
# `next_pow2 N` echoes the smallest power-of-two ≥ N (clamped to ≥ 4).
next_pow2() {
  local n="$1"
  local p=4
  while [ "$p" -lt "$n" ]; do p=$(( p * 2 )); done
  echo "$p"
}
# Per-preset shards: must satisfy MAX_PER_SHARD = 64, target next_pow2(cap/8)
# (auto-shard sweet spot from `2026-05-13-c17s-shard-heuristic.md`),
# floored by next_pow2(cap/64) (the 6-bit ID limit). dispatch macros in
# bench_concurrent cover {4, 8, ..., 131072} so cap up to 8M works.
arc_shards_for_cap() {
  local cap="$1"
  local target=$(( cap / 8 ))
  [ "$target" -lt 1 ] && target=1
  local floor=$(( (cap + 63) / 64 ))
  local picked
  picked=$(next_pow2 "$target")
  local floor_p
  floor_p=$(next_pow2 "$floor")
  [ "$picked" -lt "$floor_p" ] && picked="$floor_p"
  [ "$picked" -gt 131072 ] && picked=131072
  echo "$picked"
}

phase_arc() {
  echo "[$(date +%H:%M:%S)] phase: arc" >&2
  local presets=(
    "ConCat:200000"  "DS1:1000000"  "MergeP:400000"  "MergeS:400000"  "OLTP:1000"
    "P1:20000"  "P2:20000"  "P3:20000"  "P4:20000"  "P5:20000"
    "P6:20000"  "P7:20000"  "P8:20000"  "P9:20000"  "P10:20000"
    "P11:20000"  "P12:20000"  "P13:20000"  "P14:20000"
    "S1:160000"  "S2:160000"  "S3:160000"
  )
  for entry in "${presets[@]}"; do
    local name="${entry%%:*}"
    local cap="${entry##*:}"
    local ops warmup shards_arc
    ops=$(scale_ops "$cap")
    warmup=$(scale_warmup "$cap")
    shards_arc=$(arc_shards_for_cap "$cap")
    for value in $VALUE_LIST; do
      for variant in $VARIANTS; do
        for threads in $T_LIST; do
          local ops_t=$(( ops / threads * threads ))
          local warmup_t=$(( warmup / threads * threads ))
          [ "$warmup_t" -lt "$threads" ] && warmup_t="$threads"
          for op_mix in $MIX_LIST; do
            run_cell "arc/$name $variant T=$threads mix=$op_mix V=$value cap=$cap shards=$shards_arc" \
              --variant "$variant" \
              --shards "$shards_arc" --cap "$cap" --ops "$ops_t" --warmup "$warmup_t" \
              --trials "$TRIALS" --seed 42 \
              --threads "$threads" --skew 1.0 --keys "$ZIPF_KEYS" \
              --op-mix "$op_mix" --value "$value" --ways 1 --partitions 1 \
              --arc-preset "$name"
          done
        done
      done
    done
  done
}

case "$PHASE_ARG" in
  zipf)         phase_zipf ;;
  libcachesim)  phase_libcachesim ;;
  twitter)      phase_twitter ;;
  arc)          phase_arc ;;
  all)          phase_zipf; phase_libcachesim; phase_twitter; phase_arc ;;
  *) echo "unknown --phase: $PHASE_ARG (expected all|zipf|libcachesim|twitter|arc)" >&2; exit 2 ;;
esac

echo "[$(date +%H:%M:%S)] phase '$PHASE_ARG' complete: $OUT ($(wc -l < "$OUT") rows incl header)" >&2
if [ -s "$LOG" ]; then
  echo "[$(date +%H:%M:%S)] crashes recorded: $LOG" >&2
fi
