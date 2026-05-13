#!/usr/bin/env bash
# r2h control sweep — `2026-05-13-r2-design.md` §6.4 の H1/H2 判定用 minimal sweep。
#
# 仮説:
#   H1: r2h@8 cluster019 Mops は r1@8 から大幅後退 (≤+30%) — affinity 寄与の切り分け
#   H2: r2h@8 ≈ c17s@(shards=scaled×8) ways=1 (Mops 差 ±5%) — "ways は shard 細分化と
#       等価か" の対照
#
# 軸: variants{c17s, c17s_8x, r1, r2h} × T{1,4,8,16} × workload{核 cell のみ}
#   - c17s_8x は variant 名 "c17s" + 8× shards で `H2 control` を作る
#   - r1 / r2h は ways=8 固定
#   - r2s/r2p は本 sweep 対象外 (H1/H2 判定後に着手判断)
#
# 核 cell:
#   - Twitter cluster019 (r1 sweet spot、H1 主役) × cap {1024,4096,16384,65536}
#   - Twitter cluster006 (r1 不採用、HR drop 27pp) × cap {1024,4096,16384,65536}
#   - ARC OLTP (cap-fits、r1 不採用 21pp) × cap {256,512,1000,2000}
#   - Zipf 1.4 read-heavy (adv-hot reference) × cap {1024,4096}
#
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
HERE="docs/benchmark/r2h-control-sweep"
DATA="$HERE/data"
mkdir -p "$DATA"

cargo build --release -p senba-research --bin bench_concurrent \
    --features "senba/concurrent senba-research/external-traces" >&2

T_LIST="${T_LIST:-1 4 8 16}"
TRIALS="${TRIALS:-3}"

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

# senba::Cache の auto-shard と同じ heuristic。
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

clamp_ways() {
  local ways="$1" shards="$2"
  [ "$ways" -gt "$shards" ] && ways="$shards"
  echo "$ways"
}

# args: tag variant ways shards_mult source wparam threads op_mix skew cap arc_preset
# tag は CSV では variant 列に書き出される (c17s_8x を H2 control として区別するため)。
run_one() {
  local tag="$1" variant="$2" ways="$3" shards_mult="$4"
  local source="$5" wparam="$6" threads="$7"
  local op_mix="$8" skew="$9" cap="${10}" arc_preset="${11}"
  local ops warmup base_shards shards
  ops=$(scale_ops "$cap")
  warmup=$(scale_warmup "$cap")
  base_shards=$(scale_shards "$cap")
  shards=$(( base_shards * shards_mult ))
  # power-of-2 / ≤131072 制約
  [ "$shards" -gt 131072 ] && shards=131072
  if [ "$variant" = "r1" ] || [ "$variant" = "r2h" ]; then
    ways=$(clamp_ways "$ways" "$shards")
  fi
  warmup=$(( warmup / threads * threads ))
  [ "$warmup" -lt "$threads" ] && warmup="$threads"
  ops=$(( ops / threads * threads ))
  local label="$tag (variant=$variant ways=$ways shards=$shards) src=$source wp=$wparam T=$threads cap=$cap op=$op_mix s=$skew ops=$ops"
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
    # CSV 行の variant 列を tag に書き換え (c17s_8x の H2 control を識別するため)。
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

# 4 ARM (tag, variant, ways, shards_mult):
#   c17s_1x : c17s @ scaled shards, ways=1  (baseline = current senba auto-shard)
#   c17s_8x : c17s @ scaled shards × 8, ways=1  (H2 control)
#   r1_w8   : r1 @ scaled shards, ways=8  (r1 既往)
#   r2h_w8  : r2h @ scaled shards, ways=8  (hash control)
ARMS=(
  "c17s_1x c17s 1 1"
  "c17s_8x c17s 1 8"
  "r1_w8   r1   8 1"
  "r2h_w8  r2h  8 1"
)

echo "=== stage_twitter (cluster019 + cluster006) ===" >&2
TWITTER_CAPS="1024 4096 16384 65536"
for arm in "${ARMS[@]}"; do
  read -r tag variant ways shards_mult <<< "$arm"
  for threads in $T_LIST; do
    for cap in $TWITTER_CAPS; do
      for cluster in cluster019 cluster006; do
        trace="external/twitter-cache-trace/$cluster"
        if [ -f "$trace" ]; then
          run_one "$tag" "$variant" "$ways" "$shards_mult" \
            twitter-yang "$cluster" "$threads" gim 1.0 "$cap" ""
        fi
      done
    done
  done
done

echo "=== stage_arc (OLTP) ===" >&2
OLTP_CAPS="256 512 1000 2000"
for arm in "${ARMS[@]}"; do
  read -r tag variant ways shards_mult <<< "$arm"
  for threads in $T_LIST; do
    for cap in $OLTP_CAPS; do
      run_one "$tag" "$variant" "$ways" "$shards_mult" \
        arc OLTP "$threads" gim 1.0 "$cap" OLTP
    done
  done
done

echo "=== stage_zipf (1.4 read-heavy) ===" >&2
ZIPF_CAPS="1024 4096"
for arm in "${ARMS[@]}"; do
  read -r tag variant ways shards_mult <<< "$arm"
  for threads in $T_LIST; do
    for cap in $ZIPF_CAPS; do
      run_one "$tag" "$variant" "$ways" "$shards_mult" \
        zipf "" "$threads" read-heavy 1.4 "$cap" ""
    done
  done
done

echo "[$(date +%H:%M:%S)] sweep complete: $OUT ($(wc -l < "$OUT") rows incl header)" >&2
if [ -s "$LOG" ]; then
  echo "[$(date +%H:%M:%S)] crashes recorded: $LOG" >&2
fi
