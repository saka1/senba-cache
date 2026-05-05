#!/usr/bin/env bash
# D': j7 vs j8 を per_shard ∈ {16, 32, 64} × cap ∈ {1024, 4096, 16384} で sweep。
# 2026-05-05-sieve-j8-bench.md §4.5/§7-D' の仮説検証用:
#   per_shard を下げると false-match 率の (b) 退行が消えて (a) dep chain 単独になるはず。
#
# orig は per_shard 概念を持たないので 1 cell/cap だけ取る。
set -euo pipefail

DATE="${DATE:-2026-05-06}"
OUT="${OUT:-profiles/j8_pershard_sweep_${DATE}.csv}"
TRIALS="${TRIALS:-5}"
LEN="${LEN:-1000000}"
SEED="${SEED:-42}"
BENCH=./target/release/bench

CLUSTER="${CLUSTER:-cluster018}"
CAPS=(1024 4096 16384)
PER_SHARDS=(16 32 64)

mkdir -p profiles
echo "trial,variant,cluster,len,capacity,per_shard,shards,elapsed_ns,hits,misses,evictions" > "$OUT"

run_one() {
  local trial=$1 variant=$2 cap=$3 per_shard=$4 shards=$5
  "$BENCH" --source twitter \
           --path "external/twitter-cache-trace/${CLUSTER}" \
           --len "$LEN" --seed "$SEED" \
           --capacity "$cap" --variant "$variant" \
    | tail -n +2 \
    | awk -v t="$trial" -v cl="$CLUSTER" -v ps="$per_shard" -v sh="$shards" -F, \
        '{printf "%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n", t,$1,cl,$5,$6,ps,sh,$7,$8,$9,$10}' \
    >> "$OUT"
}

for trial in $(seq 1 "$TRIALS"); do
  for cap in "${CAPS[@]}"; do
    run_one "$trial" "orig" "$cap" "$cap" 1
    for per_shard in "${PER_SHARDS[@]}"; do
      n=$(( cap / per_shard ))
      run_one "$trial" "j7_n${n}" "$cap" "$per_shard" "$n"
      run_one "$trial" "j8_n${n}" "$cap" "$per_shard" "$n"
    done
  done
  echo "trial $trial done" >&2
done

echo "wrote $OUT" >&2
