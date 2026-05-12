#!/usr/bin/env bash
# r1 vs moka 0.12 / mini_moka 0.10 — full (T × variant × workload × value) sweep。
#
# 動機: r1 が c17s baseline 比で +30〜+77% を出した cell が moka / mini_moka と
# 比べてどこに位置するかを直接測る。lib viability 主張の transitive 推論を
# 直接データで置き換える。詳細経緯は `docs/reports/2026-05-13-r1-vs-moka-sweep.md`。
#
# 主成果物は `data/results.csv`、`plot.py` が overlay 図 + summary に変換。
#
# # 軸
#   variants : c17s @ ways=1, r1 @ ways={1,8}, moka, mini_moka
#   T_LIST   : 1 2 4 8 16
#   value    : u64, string (ARC は u64 のみ; trace 側が u64 key)
#   cap      : 4096 (Zipf/Twitter), 4000 (ARC)
#
# # Stages
#   stage_c17s      : baseline (ways=1)
#   stage_r1        : r1 × ways={1,8}
#   stage_moka      : moka 0.12 sync
#   stage_minimoka  : mini-moka 0.10 sync
#
# 環境変数:
#   T_LIST       (default "1 2 4 8 16")
#   WAYS_LIST    (default "1 8")           # r1 の --ways
#   TRIALS       (default 3)
#   OPS          (default 2000000)
#   WARMUP       (default 200000)
#   CAP          (default 4096; ARC stage は ARC_CAP)
#   ARC_CAP      (default 4000)
#   VALUE_LIST   (default "u64 string")    # ARC は u64 だけ走る
#   STAGES       (default "c17s r1 moka minimoka")
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
HERE="docs/benchmark/r1-vs-moka"
DATA="$HERE/data"
mkdir -p "$DATA"

cargo build --release -p senba-research --bin bench_concurrent \
    --features "senba/concurrent senba-research/external-traces" >&2

T_LIST="${T_LIST:-1 2 4 8 16}"
WAYS_LIST="${WAYS_LIST:-1 8}"
TRIALS="${TRIALS:-3}"
OPS="${OPS:-2000000}"
WARMUP="${WARMUP:-200000}"
CAP="${CAP:-4096}"
ARC_CAP="${ARC_CAP:-4000}"
VALUE_LIST="${VALUE_LIST:-u64 string}"
STAGES="${STAGES:-c17s r1 moka minimoka}"

OUT="$DATA/results.csv"
LOG="$DATA/crashes.log"
HEADER="variant,trial,ways,partitions,source,workload_param,op_mix,value,skew,keys,threads,cap,shards,ops,total_elapsed_ns,aggregate_mops,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv"

# 既存 results は退避して新規。実行時の再現性を優先 (途中追記は分析を歪める)。
if [ -f "$OUT" ]; then
  mv "$OUT" "${OUT}.$(date +%Y%m%d-%H%M%S).bak"
fi
echo "$HEADER" > "$OUT"
: > "$LOG"

run_one() {
  local variant="$1" ways="$2" source="$3" wparam="$4" threads="$5"
  local op_mix="$6" skew="$7" value="$8" cap="$9" trace="${10}"
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
      --op-mix "$op_mix" --value "$value" --ways "$ways" --partitions 1 \
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

# variant ごとの (ways) sweep 軸。c17s/moka/mini_moka は ways=1 固定。
sweep_variant() {
  local variant="$1" ways_axis="$2"
  for value in $VALUE_LIST; do
    for threads in $T_LIST; do
      for ways in $ways_axis; do
        for cfg in "${ZIPF_CONFIGS[@]}"; do
          read -r skew op_mix <<< "$cfg"
          run_one "$variant" "$ways" zipf "" "$threads" "$op_mix" "$skew" "$value" "$CAP" ""
        done
        for cluster in "${TWITTER_CLUSTERS[@]}"; do
          trace="$TWITTER_DIR/$cluster"
          if [ -f "$trace" ]; then
            run_one "$variant" "$ways" twitter-yang "$cluster" "$threads" gim 1.0 "$value" "$CAP" "$trace"
          fi
        done
      done
    done
    if [ "$value" = "u64" ]; then
      for threads in $T_LIST; do
        for ways in $ways_axis; do
          for arc_name in "${ARC_TRACES[@]}"; do
            trace="$ARC_DIR/${arc_name}.lis.zst"
            if [ -f "$trace" ]; then
              run_one "$variant" "$ways" arc "$arc_name" "$threads" gim 1.0 u64 "$ARC_CAP" "$trace"
            fi
          done
        done
      done
    fi
  done
}

if want_stage c17s; then
  echo "=== stage_c17s: baseline ways=1 ===" >&2
  sweep_variant c17s "1"
fi

if want_stage r1; then
  echo "=== stage_r1: r1 × ways={$WAYS_LIST} ===" >&2
  sweep_variant r1 "$WAYS_LIST"
fi

if want_stage moka; then
  echo "=== stage_moka: moka 0.12 sync ===" >&2
  sweep_variant moka "1"
fi

if want_stage minimoka; then
  echo "=== stage_minimoka: mini_moka 0.10 sync ===" >&2
  sweep_variant mini_moka "1"
fi

echo "[$(date +%H:%M:%S)] sweep complete: $OUT ($(wc -l < "$OUT") rows incl header)" >&2
if [ -s "$LOG" ]; then
  echo "[$(date +%H:%M:%S)] crashes recorded: $LOG" >&2
fi

if command -v uv >/dev/null 2>&1; then
  uv run --project scripts python "$HERE/plot.py" "$OUT" "$HERE/figures" || echo "plot.py failed" >&2
fi
