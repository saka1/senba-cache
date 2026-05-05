#!/usr/bin/env bash
# Twitter cache trace (OSDI'20) で orig vs j7 を j5 sweep と同じ枠組みで全数比較。
# cluster006/018/019 × cap{1024,4096,16384,65536} × per_shard{32,64,128,256}。
# LEN 1M、TRIALS 5 (median 報告用)。
# 出力: profiles/j7_twitter_full_<date>.csv
set -euo pipefail

DATE="${DATE:-2026-05-05}"
OUT="${OUT:-profiles/j7_twitter_full_${DATE}.csv}"
TRIALS="${TRIALS:-5}"
LEN="${LEN:-1000000}"
SEED="${SEED:-42}"
BENCH=./target/release/bench

CLUSTERS=(cluster006 cluster018 cluster019)
CAPS=(1024 4096 16384 65536)
PER_SHARDS=(32 64 128 256)

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
      for ps in "${PER_SHARDS[@]}"; do
        if (( cap % ps != 0 )); then continue; fi
        n=$(( cap / ps ))
        case "$n" in
          1|2|4|8|16|32|64|128|256|512|1024|2048) ;;
          *) continue ;;
        esac
        run_one "$trial" "j7_n${n}" "$cluster" "$cap" "$ps" "$n"
      done
    done
  done
  echo "trial $trial done" >&2
done

echo "wrote $OUT" >&2
