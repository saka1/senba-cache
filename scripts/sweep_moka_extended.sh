#!/usr/bin/env bash
# Extended sweep: orig vs j8 (per_shard=32) vs mini-moka vs moka 0.12
# 3 Twitter cluster + 4 Zipf skew = 7 workload × 4 cap × 4 variant × 5 trial。
# 目的: (1) trace 多様性 → Zipf skew で popularity 分布を変える、
#       (2) moka 0.12 (adaptive window) を mini-moka 0.10 と直接比較。
set -euo pipefail

DATE="${DATE:-$(date +%Y-%m-%d)}"
OUT="${OUT:-profiles/moka_extended_${DATE}.csv}"
TRIALS="${TRIALS:-5}"
LEN="${LEN:-1000000}"
SEED="${SEED:-42}"
KEYS="${KEYS:-100000}"
BENCH=./target/release/bench

CLUSTERS=(cluster006 cluster018 cluster019)
ZIPF_SKEWS=(0.6 0.8 1.0 1.2)
CAPS=(1024 4096 16384 65536)
PER_SHARD=32

mkdir -p profiles
echo "trial,variant,workload,skew,len,capacity,per_shard,shards,elapsed_ns,hits,misses,evictions" > "$OUT"

run_twitter() {
  local trial=$1 variant=$2 cluster=$3 cap=$4 per_shard=$5 shards=$6
  "$BENCH" --source twitter \
           --path "external/twitter-cache-trace/${cluster}" \
           --len "$LEN" --seed "$SEED" \
           --capacity "$cap" --variant "$variant" \
    | tail -n +2 \
    | awk -v t="$trial" -v wl="$cluster" -v sk="" -v ps="$per_shard" -v sh="$shards" -F, \
        '{printf "%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n", t,$1,wl,sk,$5,$6,ps,sh,$7,$8,$9,$10}' \
    >> "$OUT"
}

run_zipf() {
  local trial=$1 variant=$2 skew=$3 cap=$4 per_shard=$5 shards=$6
  "$BENCH" --source zipf --skew "$skew" --keys "$KEYS" \
           --len "$LEN" --seed "$SEED" \
           --capacity "$cap" --variant "$variant" \
    | tail -n +2 \
    | awk -v t="$trial" -v wl="zipf" -v sk="$skew" -v ps="$per_shard" -v sh="$shards" -F, \
        '{printf "%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n", t,$1,wl,sk,$5,$6,ps,sh,$7,$8,$9,$10}' \
    >> "$OUT"
}

for trial in $(seq 1 "$TRIALS"); do
  for cluster in "${CLUSTERS[@]}"; do
    for cap in "${CAPS[@]}"; do
      n=$(( cap / PER_SHARD ))
      run_twitter "$trial" "orig"      "$cluster" "$cap" "$cap" 1
      run_twitter "$trial" "j8_n${n}"  "$cluster" "$cap" "$PER_SHARD" "$n"
      run_twitter "$trial" "mini_moka" "$cluster" "$cap" 0 0
      run_twitter "$trial" "moka"      "$cluster" "$cap" 0 0
    done
  done
  for skew in "${ZIPF_SKEWS[@]}"; do
    for cap in "${CAPS[@]}"; do
      n=$(( cap / PER_SHARD ))
      run_zipf "$trial" "orig"      "$skew" "$cap" "$cap" 1
      run_zipf "$trial" "j8_n${n}"  "$skew" "$cap" "$PER_SHARD" "$n"
      run_zipf "$trial" "mini_moka" "$skew" "$cap" 0 0
      run_zipf "$trial" "moka"      "$skew" "$cap" 0 0
    done
  done
  echo "trial $trial done" >&2
done

echo "wrote $OUT" >&2
