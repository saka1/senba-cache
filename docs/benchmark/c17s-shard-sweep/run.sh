#!/usr/bin/env bash
# c17s shard heuristic sweep — `2026-05-13-r2h-control-results.md` の発見
# 「c17s_8x が r1/r2h を pareto dominate」を広範な実 trace で検証 + sweet spot 特定。
#
# 軸:
#   variant       : c17s × shards_mult ∈ {1,2,4,8,16} (= c1x..c16x) + r1@ways=8 対照列
#   T_LIST        : 1 4 8 16
#   workload      : Twitter cluster {006,016,018,019,034} × cap {1024,4096,16384,65536}
#                   ARC preset {OLTP,P1,P3,P6,P8,S1,S3,DS1,ConCat,MergeP,MergeS} × preset cap
#                   Zipf {0.8/gim,1.0/gim,1.4/gim,1.4/RH} × cap {1024,4096,16384,65536}
#   value         : u64
#   trials        : 3
#
# 制約:
#   - bench_concurrent の SHARDS は 4..131072 の power-of-2、超過は 131072 にクランプ
#   - cap_per_shard >= 2 が SIEVE 動作の min。shards_mult が大きいと per_shard=1 になる
#     cell があり、そういう cell は skip して `data/skipped.log` に記録
#
# 環境変数:
#   T_LIST          (default "1 4 8 16")
#   MULT_LIST       (default "1 2 4 8 16")
#   TRIALS          (default 3)
#   STAGES          (default "zipf twitter arc")
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
HERE="docs/benchmark/c17s-shard-sweep"
DATA="$HERE/data"
mkdir -p "$DATA"

cargo build --release -p senba-research --bin bench_concurrent \
    --features "senba/concurrent senba-research/external-traces" >&2

T_LIST="${T_LIST:-1 4 8 16}"
MULT_LIST="${MULT_LIST:-1 2 4 8 16}"
TRIALS="${TRIALS:-3}"
STAGES="${STAGES:-zipf twitter arc}"
ZIPF_CAPS="${ZIPF_CAPS:-1024 4096 16384 65536}"
TWITTER_CAPS="${TWITTER_CAPS:-1024 4096 16384 65536}"

OUT="$DATA/results.csv"
LOG="$DATA/crashes.log"
SKIP="$DATA/skipped.log"
HEADER="variant,trial,ways,partitions,source,workload_param,op_mix,value,skew,keys,threads,cap,shards,ops,total_elapsed_ns,aggregate_mops,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv"

if [ -f "$OUT" ]; then
  mv "$OUT" "${OUT}.$(date +%Y%m%d-%H%M%S).bak"
fi
echo "$HEADER" > "$OUT"
: > "$LOG"
: > "$SKIP"

# mokabench canonical caps (r1-vs-moka-cap-sweep と同期コピー)
declare -A ARC_CAPS_MAP=(
  [OLTP]="256 512 1000 2000"
  [P1]="20000 160000"
  [P3]="20000 160000"
  [P6]="20000 160000"
  [P8]="20000 160000"
  [S1]="100000 800000"
  [S3]="100000 400000 800000"
  [DS1]="1000000 4000000 8000000"
  [ConCat]="200000 400000 3200000"
  [MergeP]="400000 1000000 3200000"
  [MergeS]="400000 1000000 3200000"
)

scale_ops() {
  local cap="$1" ops=$((cap * 4))
  [ "$ops" -lt 2000000 ] && ops=2000000
  [ "$ops" -gt 16000000 ] && ops=16000000
  echo "$ops"
}

scale_warmup() {
  local cap="$1" w="$cap"
  [ "$w" -lt 200000 ] && w=200000
  [ "$w" -gt 4000000 ] && w=4000000
  echo "$w"
}

# senba::Cache auto-shard と同じ heuristic = next_pow2(ceil(cap/64))、min 4
scale_shards() {
  local cap="$1"
  local need=$(( (cap + 63) / 64 ))
  [ "$need" -lt 4 ] && need=4
  local s=4
  while [ "$s" -lt "$need" ]; do
    s=$(( s * 2 ))
  done
  echo "$s"
}

next_pow2_clamp() {
  # 引数 n を power-of-2 に切り下げ (実は scale_shards × mult は既に pow2)、131072 にクランプ
  local n="$1"
  [ "$n" -gt 131072 ] && n=131072
  echo "$n"
}

clamp_ways() {
  local ways="$1" shards="$2"
  [ "$ways" -gt "$shards" ] && ways="$shards"
  echo "$ways"
}

# args: tag variant ways shards source wparam threads op_mix skew cap arc_preset
run_one() {
  local tag="$1" variant="$2" ways="$3" shards="$4"
  local source="$5" wparam="$6" threads="$7"
  local op_mix="$8" skew="$9" cap="${10}" arc_preset="${11}"
  local ops warmup
  ops=$(scale_ops "$cap")
  warmup=$(scale_warmup "$cap")
  if [ "$variant" = "r1" ] || [ "$variant" = "r2h" ]; then
    ways=$(clamp_ways "$ways" "$shards")
  fi
  warmup=$(( warmup / threads * threads ))
  [ "$warmup" -lt "$threads" ] && warmup="$threads"
  ops=$(( ops / threads * threads ))
  local label="$tag (variant=$variant ways=$ways shards=$shards) src=$source wp=$wparam T=$threads cap=$cap op=$op_mix s=$skew"
  echo "[$(date +%H:%M:%S)] $label" >&2
  local tmp
  tmp=$(mktemp)
  local extra_args=()
  case "$source" in
    zipf) extra_args=(--source zipf) ;;
    twitter-yang)
      extra_args=(--source twitter-yang --trace-file "external/twitter-cache-trace/$wparam" --workload-param "$wparam")
      ;;
    arc)
      extra_args=(--arc-preset "$arc_preset")
      ;;
  esac
  if ./target/release/bench_concurrent --variant "$variant" \
      --shards "$shards" --cap "$cap" --ops "$ops" --warmup "$warmup" --trials "$TRIALS" --seed 42 \
      --threads "$threads" --skew "$skew" --keys 100000 \
      --op-mix "$op_mix" --value u64 --ways "$ways" --partitions 1 \
      "${extra_args[@]}" > "$tmp" 2>&1; then
    tail -n +2 "$tmp" | grep -E "^$variant," | sed "s/^$variant,/$tag,/" >> "$OUT" || true
  else
    local rc=$?
    echo "[$(date +%H:%M:%S)] FAILED (rc=$rc): $label" >> "$LOG"
    tail -20 "$tmp" >> "$LOG"
    echo "---" >> "$LOG"
    tail -n +2 "$tmp" | grep -E "^$variant," | sed "s/^$variant,/$tag,/" >> "$OUT" || true
  fi
  rm -f "$tmp"
}

# c17s shards_mult arm dispatcher: 1 cell × MULT_LIST 全部
run_c17s_mults() {
  local source="$1" wparam="$2" threads="$3" op_mix="$4" skew="$5" cap="$6" arc_preset="$7"
  local base_shards
  base_shards=$(scale_shards "$cap")
  for mult in $MULT_LIST; do
    local shards=$(( base_shards * mult ))
    shards=$(next_pow2_clamp "$shards")
    local per_shard=$(( cap / shards ))
    if [ "$per_shard" -lt 2 ]; then
      echo "[skip per_shard=$per_shard<2] cap=$cap shards=$shards mult=$mult src=$source wp=$wparam T=$threads" >> "$SKIP"
      continue
    fi
    run_one "c${mult}x" c17s 1 "$shards" "$source" "$wparam" "$threads" \
      "$op_mix" "$skew" "$cap" "$arc_preset"
  done
}

# r1@ways=8 対照列 (base shards 固定)
run_r1_w8() {
  local source="$1" wparam="$2" threads="$3" op_mix="$4" skew="$5" cap="$6" arc_preset="$7"
  local base_shards
  base_shards=$(scale_shards "$cap")
  run_one "r1_w8" r1 8 "$base_shards" "$source" "$wparam" "$threads" \
    "$op_mix" "$skew" "$cap" "$arc_preset"
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
TWITTER_CLUSTERS=("cluster006" "cluster016" "cluster018" "cluster019" "cluster034")

if want_stage zipf; then
  echo "=== stage_zipf ===" >&2
  for threads in $T_LIST; do
    for cap in $ZIPF_CAPS; do
      for cfg in "${ZIPF_CONFIGS[@]}"; do
        read -r skew op_mix <<< "$cfg"
        run_c17s_mults zipf "" "$threads" "$op_mix" "$skew" "$cap" ""
        run_r1_w8     zipf "" "$threads" "$op_mix" "$skew" "$cap" ""
      done
    done
  done
fi

if want_stage twitter; then
  echo "=== stage_twitter ===" >&2
  for threads in $T_LIST; do
    for cap in $TWITTER_CAPS; do
      for cluster in "${TWITTER_CLUSTERS[@]}"; do
        trace="external/twitter-cache-trace/$cluster"
        [ -f "$trace" ] || continue
        run_c17s_mults twitter-yang "$cluster" "$threads" gim 1.0 "$cap" ""
        run_r1_w8     twitter-yang "$cluster" "$threads" gim 1.0 "$cap" ""
      done
    done
  done
fi

if want_stage arc; then
  echo "=== stage_arc ===" >&2
  for threads in $T_LIST; do
    for preset in OLTP P1 P3 P6 P8 S1 S3 DS1 ConCat MergeP MergeS; do
      caps="${ARC_CAPS_MAP[$preset]:-}"
      [ -z "$caps" ] && continue
      for cap in $caps; do
        run_c17s_mults arc "$preset" "$threads" gim 1.0 "$cap" "$preset"
        run_r1_w8     arc "$preset" "$threads" gim 1.0 "$cap" "$preset"
      done
    done
  done
fi

echo "[$(date +%H:%M:%S)] sweep complete: $OUT ($(wc -l < "$OUT") rows incl header)" >&2
[ -s "$LOG" ] && echo "[$(date +%H:%M:%S)] crashes recorded: $LOG" >&2
[ -s "$SKIP" ] && echo "[$(date +%H:%M:%S)] skipped cells: $SKIP" >&2
