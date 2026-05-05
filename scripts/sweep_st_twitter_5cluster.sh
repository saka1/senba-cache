#!/usr/bin/env bash
# Twitter cache trace (OSDI'20) で orig vs j8 (per_shard=32 champion) vs moka 0.12 vs mini_moka 0.10
# のシングルスレッド比較。
# cluster006/016/018/019/034 × cap{1024,4096,16384,65536} × {orig, j8_n*, moka, mini_moka} × 5 trials。
# 出力: profiles/st_twitter_5cluster_<date>.csv
set -euo pipefail

DATE="${DATE:-$(date +%Y-%m-%d)}"
OUT="${OUT:-profiles/st_twitter_5cluster_${DATE}.csv}"
TRIALS="${TRIALS:-5}"
LEN="${LEN:-1000000}"
SEED="${SEED:-42}"
BENCH=./target/release/bench

CLUSTERS=(cluster006 cluster016 cluster018 cluster019 cluster034)
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
      run_one "$trial" "moka" "$cluster" "$cap" "0" "0"
      run_one "$trial" "mini_moka" "$cluster" "$cap" "0" "0"
    done
  done
  echo "trial $trial done" >&2
done

echo "wrote $OUT" >&2
