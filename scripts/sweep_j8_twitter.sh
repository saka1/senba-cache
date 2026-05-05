#!/usr/bin/env bash
# Twitter cache trace (OSDI'20) で j8 (M5.3 + tag id embed + free_list 廃止) を
# orig / j7 と並べてベンチする。
#
# - per_shard 固定: j8 の 6-bit ID 制限より MAX=64。本 sweep は **per_shard=64 固定**
#   で「特性が出るか」を最初に詰める (j8_plan.md §11.1)。
# - cap ∈ {1024, 4096, 16384}: 各 cap で SHARDS = cap/64 ⇒ {16, 64, 256}。
# - LEN=1M、TRIALS=5、cluster018 (j7 と同じ baseline)。
set -euo pipefail

DATE="${DATE:-2026-05-05}"
OUT="${OUT:-profiles/j8_twitter_pareto_${DATE}.csv}"
TRIALS="${TRIALS:-5}"
LEN="${LEN:-1000000}"
SEED="${SEED:-42}"
BENCH=./target/release/bench

CLUSTERS=("${CLUSTERS:-cluster018}")
CAPS=(1024 4096 16384)
PER_SHARD=64

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
      n=$(( cap / PER_SHARD ))
      # orig は単一構造、per_shard 概念なし (= cap そのもの)
      run_one "$trial" "orig" "$cluster" "$cap" "$cap" 1
      run_one "$trial" "j7_n${n}" "$cluster" "$cap" "$PER_SHARD" "$n"
      run_one "$trial" "j8_n${n}" "$cluster" "$cap" "$PER_SHARD" "$n"
    done
  done
  echo "trial $trial done" >&2
done

echo "wrote $OUT" >&2
