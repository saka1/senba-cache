#!/usr/bin/env bash
# `senba::concurrent::PartitionedCache` の (T × N) sweep — **cap=1024 版**。
# `2026-05-13-partitioned-vtune.md` で L3 latency が scaling 律速、cap=1024 で
# per-partition working set が L1d fit に入ると VTune で確認 → 同 cap での
# bench_concurrent sweep で partitioned 採否を確定させるための data 収集。
#
# 旧 cap=4096 sweep (`docs/benchmark/partitioned-sweep/`) と直接比較できるよう、
# stages/T/N/workload 軸は全部同一、CAP のみ差し替え。data/figures は分離。
#
# 主成果物は `data/results.csv` (plot.py が summary 図に変換、cell heatmap は削除)。
#
# # Stages
#   stage_baseline  : c17s @ ways=1 (= reference) を全 workload で先行収録
#   stage_partitioned : PartitionedCache を N={1,2,4,8,16} で sweep
#   stage_r1        : r1 を WAYS={1,4,16} で sweep (横比較用)
#
# 環境変数:
#   T_LIST       (default "1 2 4 8 16")
#   N_LIST       (default "1 2 4 8 16")   # PartitionedCache の --partitions
#   WAYS_LIST    (default "1 4 16")        # r1 の --ways
#   TRIALS       (default 3)
#   OPS          (default 2000000)
#   WARMUP       (default 200000)
#   CAP          (default 1024; ARC stage は ARC_CAP)
#   ARC_CAP      (default 1000)
#   VALUE_LIST   (default "u64")
#   STAGES       (default "baseline partitioned r1")
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
HERE="docs/benchmark/partitioned-cap1024-sweep"
DATA="$HERE/data"
mkdir -p "$DATA"

cargo build --release -p senba-research --bin bench_concurrent \
    --features "senba/concurrent senba-research/external-traces" >&2

T_LIST="${T_LIST:-1 2 4 8 16}"
N_LIST="${N_LIST:-1 2 4 8 16}"
WAYS_LIST="${WAYS_LIST:-1 4 16}"
TRIALS="${TRIALS:-3}"
OPS="${OPS:-2000000}"
WARMUP="${WARMUP:-200000}"
CAP="${CAP:-1024}"
ARC_CAP="${ARC_CAP:-1000}"
VALUE_LIST="${VALUE_LIST:-u64}"
STAGES="${STAGES:-baseline partitioned r1}"

OUT="$DATA/results.csv"
LOG="$DATA/crashes.log"
HEADER="variant,trial,ways,partitions,source,workload_param,op_mix,value,skew,keys,threads,cap,shards,ops,total_elapsed_ns,aggregate_mops,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv"

if [ ! -f "$OUT" ]; then
  echo "$HEADER" > "$OUT"
fi
: > "$LOG"

# args: variant ways partitions source wparam threads op_mix skew value cap trace_file
run_one() {
  local variant="$1" ways="$2" parts="$3" source="$4" wparam="$5" threads="$6"
  local op_mix="$7" skew="$8" value="$9" cap="${10}" trace="${11}"
  local label="$variant ways=$ways N=$parts src=$source wp=$wparam T=$threads cap=$cap op=$op_mix s=$skew v=$value"
  echo "[$(date +%H:%M:%S)] $label" >&2
  local tmp
  tmp=$(mktemp)
  local trace_args=()
  if [ -n "$trace" ]; then
    trace_args=(--source "$source" --trace-file "$trace" --workload-param "$wparam")
  else
    trace_args=(--source zipf)
  fi
  if ./target/release/bench_concurrent --variant "$variant" \
      --shards 64 --cap "$cap" --ops "$OPS" --warmup "$WARMUP" --trials "$TRIALS" --seed 42 \
      --threads "$threads" --skew "$skew" --keys 100000 \
      --op-mix "$op_mix" --value "$value" --ways "$ways" --partitions "$parts" \
      "${trace_args[@]}" > "$tmp" 2>&1; then
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

want_stage() {
  case " $STAGES " in
    *" $1 "*) return 0 ;;
    *) return 1 ;;
  esac
}

ZIPF_CONFIGS=(
  "0.8 gim"
  "1.0 gim"
  "1.4 gim"
  "1.4 read-heavy"
)

TWITTER_DIR="external/twitter-cache-trace"
TWITTER_CLUSTERS=("cluster006" "cluster019" "cluster034")

ARC_DIR="external/mokabench/cache-trace/arc"
ARC_TRACES=("OLTP" "DS1")

# ---------------------------------------------------------------------------
# Stage 1: baseline c17s @ ways=1
# ---------------------------------------------------------------------------
if want_stage baseline; then
  echo "=== stage_baseline: c17s @ ways=1 ===" >&2
  for value in $VALUE_LIST; do
    for threads in $T_LIST; do
      for cfg in "${ZIPF_CONFIGS[@]}"; do
        read -r skew op_mix <<< "$cfg"
        run_one c17s 1 1 zipf "" "$threads" "$op_mix" "$skew" "$value" "$CAP" ""
      done
      for cluster in "${TWITTER_CLUSTERS[@]}"; do
        trace="$TWITTER_DIR/$cluster"
        if [ -f "$trace" ]; then
          run_one c17s 1 1 twitter-yang "$cluster" "$threads" gim 1.0 "$value" "$CAP" "$trace"
        fi
      done
    done
    if [ "$value" = "u64" ]; then
      for threads in $T_LIST; do
        for arc_name in "${ARC_TRACES[@]}"; do
          trace="$ARC_DIR/${arc_name}.lis.zst"
          if [ -f "$trace" ]; then
            run_one c17s 1 1 arc "$arc_name" "$threads" gim 1.0 u64 "$ARC_CAP" "$trace"
          fi
        done
      done
    fi
  done
fi

# ---------------------------------------------------------------------------
# Stage 2: PartitionedCache × (T × N) sweep
# ---------------------------------------------------------------------------
if want_stage partitioned; then
  echo "=== stage_partitioned: PartitionedCache × (T × N) ===" >&2
  for value in $VALUE_LIST; do
    for threads in $T_LIST; do
      for parts in $N_LIST; do
        for cfg in "${ZIPF_CONFIGS[@]}"; do
          read -r skew op_mix <<< "$cfg"
          run_one partitioned 1 "$parts" zipf "" "$threads" "$op_mix" "$skew" "$value" "$CAP" ""
        done
        for cluster in "${TWITTER_CLUSTERS[@]}"; do
          trace="$TWITTER_DIR/$cluster"
          if [ -f "$trace" ]; then
            run_one partitioned 1 "$parts" twitter-yang "$cluster" "$threads" gim 1.0 "$value" "$CAP" "$trace"
          fi
        done
      done
    done
    if [ "$value" = "u64" ]; then
      for threads in $T_LIST; do
        for parts in $N_LIST; do
          for arc_name in "${ARC_TRACES[@]}"; do
            trace="$ARC_DIR/${arc_name}.lis.zst"
            if [ -f "$trace" ]; then
              run_one partitioned 1 "$parts" arc "$arc_name" "$threads" gim 1.0 u64 "$ARC_CAP" "$trace"
            fi
          done
        done
      done
    fi
  done
fi

# ---------------------------------------------------------------------------
# Stage 3: r1 × WAYS sweep (横比較)
# ---------------------------------------------------------------------------
if want_stage r1; then
  echo "=== stage_r1: r1 × WAYS ===" >&2
  for value in $VALUE_LIST; do
    for threads in $T_LIST; do
      for ways in $WAYS_LIST; do
        for cfg in "${ZIPF_CONFIGS[@]}"; do
          read -r skew op_mix <<< "$cfg"
          run_one r1 "$ways" 1 zipf "" "$threads" "$op_mix" "$skew" "$value" "$CAP" ""
        done
        for cluster in "${TWITTER_CLUSTERS[@]}"; do
          trace="$TWITTER_DIR/$cluster"
          if [ -f "$trace" ]; then
            run_one r1 "$ways" 1 twitter-yang "$cluster" "$threads" gim 1.0 "$value" "$CAP" "$trace"
          fi
        done
      done
    done
    if [ "$value" = "u64" ]; then
      for threads in $T_LIST; do
        for ways in $WAYS_LIST; do
          for arc_name in "${ARC_TRACES[@]}"; do
            trace="$ARC_DIR/${arc_name}.lis.zst"
            if [ -f "$trace" ]; then
              run_one r1 "$ways" 1 arc "$arc_name" "$threads" gim 1.0 u64 "$ARC_CAP" "$trace"
            fi
          done
        done
      done
    fi
  done
fi

echo "[$(date +%H:%M:%S)] sweep complete: $OUT ($(wc -l < "$OUT") rows incl header)" >&2
if [ -s "$LOG" ]; then
  echo "[$(date +%H:%M:%S)] crashes recorded: $LOG" >&2
fi

if command -v uv >/dev/null 2>&1; then
  uv run --project scripts python "$HERE/plot.py" "$OUT" "$HERE/figures" || echo "plot.py failed" >&2
fi
