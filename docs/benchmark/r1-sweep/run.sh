#!/usr/bin/env bash
# r1 (routing affinity variant) を c17s baseline と (T × WAYS × workload × value)
# sweep で比較する。主成果物は `data/results.csv` (plot.py が pareto 図 + Mops vs T
# 曲線に変換)。
#
# 設計仕様: `docs/reports/2026-05-12-r1-design.md` §6.2。本 sweep は r-series 全体の
# baseline 収録を目的とし、初版 sweep (T=16 偏り、Twitter cluster52 のみ、WAYS≤16) で
# 不足だった以下を埋める:
#   - T={1,2,4,8,16} 全列 (Mops vs T 曲線の knee 観測)
#   - Twitter は OSDI'20 Yang 形式の cluster006/016/018/019/034 5 種
#   - ARC は OLTP/DS1/S1/S3/P1/P8/ConCat/MergeP の代表 8 種
#   - WAYS={1,2,4,8} (16 は前 sweep で HR drop 過大が確認済、除外)
#   - value u64 / string 両方
#
# # Stages (順次 append, 個別停止可)
#
#   stage_baseline  : c17s @ ways=1 を全 workload で先行収録 (= r1 比較の reference)
#   stage_zipf      : r1 を Zipf 系 workload で sweep
#   stage_twitter   : r1 を Twitter cluster trace (OSDI Yang 形式) で sweep
#   stage_arc       : r1 を ARC trace で sweep (cap=4000 固定)
#
# 環境変数で範囲を絞れる:
#   T_LIST       (default "1 2 4 8 16")
#   WAYS_LIST    (default "1 2 4 8")
#   TRIALS       (default 3)
#   OPS          (default 4000000)
#   WARMUP       (default 200000)
#   CAP          (default 4096; ARC stage は ARC_CAP を別途使用)
#   ARC_CAP      (default 4000; senba 既往優位帯)
#   VALUE_LIST   (default "u64 string"; ARC trace は u64 固定)
#   STAGES       (default "baseline zipf twitter arc")
#
# 部分実行例:
#   STAGES="baseline zipf" VALUE_LIST=u64 ./run.sh
#   STAGES=twitter T_LIST="1 4 16" WAYS_LIST="1 4" ./run.sh
#
# repo root から実行。`data/results.csv` に append。fresh sweep にしたい時は事前削除。
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
HERE="docs/benchmark/r1-sweep"
DATA="$HERE/data"
mkdir -p "$DATA"

# external-traces feature を有効化 (ARC OLTP の zstd 展開に必須)。Zipf / Twitter のみの
# stage では feature なしでも動くが、build を 1 本に統一して再 link 時間を節約。
cargo build --release -p senba-research --bin bench_concurrent --features senba-research/external-traces

T_LIST="${T_LIST:-1 2 4 8 16}"
WAYS_LIST="${WAYS_LIST:-1 2 4 8}"
TRIALS="${TRIALS:-3}"
OPS="${OPS:-4000000}"
WARMUP="${WARMUP:-200000}"
CAP="${CAP:-4096}"
ARC_CAP="${ARC_CAP:-4000}"
VALUE_LIST="${VALUE_LIST:-u64 string}"
STAGES="${STAGES:-baseline zipf twitter arc}"

OUT="$DATA/results.csv"
LOG="$DATA/crashes.log"
HEADER="variant,trial,ways,source,workload_param,op_mix,value,skew,keys,threads,cap,shards,ops,total_elapsed_ns,aggregate_mops,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv"

if [ ! -f "$OUT" ]; then
  echo "$HEADER" > "$OUT"
fi
: > "$LOG"

# args: variant ways source wparam threads op_mix skew value cap trace_file
run_one() {
  local variant="$1" ways="$2" source="$3" wparam="$4" threads="$5" op_mix="$6"
  local skew="$7" value="$8" cap="$9" trace="${10}"
  local label="$variant ways=$ways src=$source wp=$wparam T=$threads cap=$cap op=$op_mix s=$skew v=$value"
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
      --op-mix "$op_mix" --value "$value" --ways "$ways" \
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

# Zipf workload (skew, op-mix) は r1 設計 §6.2 から:
ZIPF_CONFIGS=(
  "0.8 gim"
  "1.0 gim"
  "1.4 gim"
  "1.4 read-heavy"
)

# Twitter OSDI'20 Yang 形式 trace (string key → DefaultHasher で u64 化)。
# external/twitter-cache-trace/ submodule。
TWITTER_DIR="external/twitter-cache-trace"
TWITTER_CLUSTERS=("cluster006" "cluster016" "cluster018" "cluster019" "cluster034")

# ARC 代表 trace 8 種。set / OLTP / process / merge / concat / DS を網羅:
ARC_DIR="external/mokabench/cache-trace/arc"
ARC_TRACES=("OLTP" "DS1" "S1" "S3" "P1" "P8" "ConCat" "MergeP")

# ---------------------------------------------------------------------------
# Stage 1: baseline c17s @ ways=1
# ---------------------------------------------------------------------------
if want_stage baseline; then
  echo "=== stage_baseline: c17s @ ways=1 ===" >&2
  for value in $VALUE_LIST; do
    for threads in $T_LIST; do
      for cfg in "${ZIPF_CONFIGS[@]}"; do
        read -r skew op_mix <<< "$cfg"
        run_one c17s 1 zipf "" "$threads" "$op_mix" "$skew" "$value" "$CAP" ""
      done
      for cluster in "${TWITTER_CLUSTERS[@]}"; do
        trace="$TWITTER_DIR/$cluster"
        if [ -f "$trace" ]; then
          run_one c17s 1 twitter-yang "$cluster" "$threads" gim 1.0 "$value" "$CAP" "$trace"
        else
          echo "[$(date +%H:%M:%S)] SKIP $trace (missing)" >&2
        fi
      done
    done
    if [ "$value" = "u64" ]; then
      for threads in $T_LIST; do
        for arc_name in "${ARC_TRACES[@]}"; do
          trace="$ARC_DIR/${arc_name}.lis.zst"
          if [ -f "$trace" ]; then
            run_one c17s 1 arc "$arc_name" "$threads" gim 1.0 u64 "$ARC_CAP" "$trace"
          else
            echo "[$(date +%H:%M:%S)] SKIP $trace (missing)" >&2
          fi
        done
      done
    fi
  done
fi

# ---------------------------------------------------------------------------
# Stage 2: r1 × Zipf, T × WAYS sweep
# ---------------------------------------------------------------------------
if want_stage zipf; then
  echo "=== stage_zipf: r1 × Zipf ===" >&2
  for value in $VALUE_LIST; do
    for threads in $T_LIST; do
      for ways in $WAYS_LIST; do
        for cfg in "${ZIPF_CONFIGS[@]}"; do
          read -r skew op_mix <<< "$cfg"
          run_one r1 "$ways" zipf "" "$threads" "$op_mix" "$skew" "$value" "$CAP" ""
        done
      done
    done
  done
fi

# ---------------------------------------------------------------------------
# Stage 3: r1 × Twitter (OSDI Yang 形式 5 cluster)
# ---------------------------------------------------------------------------
if want_stage twitter; then
  echo "=== stage_twitter: r1 × twitter-yang ===" >&2
  for value in $VALUE_LIST; do
    for threads in $T_LIST; do
      for ways in $WAYS_LIST; do
        for cluster in "${TWITTER_CLUSTERS[@]}"; do
          trace="$TWITTER_DIR/$cluster"
          if [ ! -f "$trace" ]; then
            echo "[$(date +%H:%M:%S)] SKIP $trace (missing)" >&2
            continue
          fi
          run_one r1 "$ways" twitter-yang "$cluster" "$threads" gim 1.0 "$value" "$CAP" "$trace"
        done
      done
    done
  done
fi

# ---------------------------------------------------------------------------
# Stage 4: r1 × ARC (u64, cap=ARC_CAP)
# ---------------------------------------------------------------------------
if want_stage arc; then
  echo "=== stage_arc: r1 × ARC @ cap=$ARC_CAP ===" >&2
  for threads in $T_LIST; do
    for ways in $WAYS_LIST; do
      for arc_name in "${ARC_TRACES[@]}"; do
        trace="$ARC_DIR/${arc_name}.lis.zst"
        if [ ! -f "$trace" ]; then
          echo "[$(date +%H:%M:%S)] SKIP $trace (missing)" >&2
          continue
        fi
        run_one r1 "$ways" arc "$arc_name" "$threads" gim 1.0 u64 "$ARC_CAP" "$trace"
      done
    done
  done
fi

echo "[$(date +%H:%M:%S)] sweep complete: $OUT ($(wc -l < "$OUT") rows incl header)" >&2
if [ -s "$LOG" ]; then
  echo "[$(date +%H:%M:%S)] crashes recorded: $LOG" >&2
fi

if command -v uv >/dev/null 2>&1; then
  uv run --project scripts python "$HERE/plot.py" "$OUT" "$HERE/figures" || echo "plot.py failed" >&2
fi
