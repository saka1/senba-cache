#!/usr/bin/env bash
# Twitter cache trace (OSDI'20) で orig vs j8 (per_shard=32 champion) vs mini-moka (W-TinyLFU)。
# cluster006/018/019 × cap{1024,4096,16384,65536} × {orig, j8_n*, mini_moka} × 5 trials。
# j8 は per_shard=32 固定 → SHARDS=cap/32 → j8_n{32,128,512,2048}。
# 出力: profiles/minimoka_twitter_<date>.csv
set -euo pipefail

DATE="${DATE:-$(date +%Y-%m-%d)}"
OUT="${OUT:-profiles/minimoka_twitter_${DATE}.csv}"
TRIALS="${TRIALS:-5}"
LEN="${LEN:-1000000}"
SEED="${SEED:-42}"
BENCH=./target/release/bench

CLUSTERS=(cluster006 cluster018 cluster019)
CAPS=(1024 4096 16384 65536)
PER_SHARD=32  # j8 champion

mkdir -p profiles
echo "trial,variant,cluster,len,capacity,per_shard,shards,elapsed_ns,hits,misses,evictions" > "$OUT"

run_one() {
  local trial=$1 variant=$2 cluster=$3 cap=$4 per_shard=$5 shards=$6
  "$BENCH" --source twitter \
           --path "external/twitter-cache-trace/${cluster}" \
           --len "$LEN" --seed "$SEED" \
           --capacity "$cap" --variant "$variant" \
    | tail -n +2 \
    | awk -v t="$trial" -v cl="$cluster" -v ps="$per_shard" -v sh="$shards" -F, \
        '{printf "%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n", t,$1,cl,$5,$6,ps,sh,$7,$8,$9,$10}' \
    >> "$OUT"
}

for trial in $(seq 1 "$TRIALS"); do
  for cluster in "${CLUSTERS[@]}"; do
    for cap in "${CAPS[@]}"; do
      run_one "$trial" "orig" "$cluster" "$cap" "$cap" 1
      n=$(( cap / PER_SHARD ))
      run_one "$trial" "j8_n${n}" "$cluster" "$cap" "$PER_SHARD" "$n"
      # mini_moka は per_shard 概念が無いので NA 扱い (列は cap を入れる)
      run_one "$trial" "mini_moka" "$cluster" "$cap" "0" "0"
    done
  done
  echo "trial $trial done" >&2
done

echo "wrote $OUT" >&2
