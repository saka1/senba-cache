#!/usr/bin/env bash
# Twitter cache trace (OSDI'20) を **生 String キーのまま** orig vs senba::Cache で sweep。
# cluster006/016/018/019/034 × cap{1024,4096,16384,65536} × {orig, senba per_shard=32, senba per_shard=64} × 3 trials。
#
# senba::Cache は per-shard ≤ 64 (6-bit ID) のため、SHARDS = cap / per_shard を都度選択。
# Slot32 default (Entry<String, u64> = 32B 完全一致)。
#
# 出力: profiles/senba_twitter_string_<date>.csv
set -euo pipefail

DATE="${DATE:-$(date +%Y-%m-%d)}"
OUT="${OUT:-profiles/senba_twitter_string_${DATE}.csv}"
TRIALS="${TRIALS:-3}"
LEN="${LEN:-1000000}"
BENCH=./target/release/bench

CLUSTERS=(cluster006 cluster016 cluster018 cluster019 cluster034)
CAPS=(1024 4096 16384 65536)
PER_SHARDS=(32 64)

mkdir -p profiles
echo "trial,variant,cluster,len,capacity,per_shard,shards,elapsed_ns,hits,misses,evictions" > "$OUT"

run_one() {
  local trial=$1 variant=$2 cluster=$3 cap=$4 per_shard=$5 shards=$6
  "$BENCH" --source twitter-string \
           --path "external/twitter-cache-trace/${cluster}" \
           --len "$LEN" \
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
      for ps in "${PER_SHARDS[@]}"; do
        n=$(( cap / ps ))
        run_one "$trial" "senba_n${n}" "$cluster" "$cap" "$ps" "$n"
      done
    done
    echo "trial $trial $cluster done" >&2
  done
done

echo "wrote $OUT" >&2
