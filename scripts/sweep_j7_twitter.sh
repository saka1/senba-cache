#!/usr/bin/env bash
# Twitter cache trace (OSDI'20) で j7 (M2.3: tag を u16 化、visited + 14-bit hash) を
# j5 / j6 / orig と並べてベンチする。`sweep_j6_twitter.sh` の構造を踏襲し、
# 同じ cell で 4 variant を縦に並べて CSV を出すので diff がそのまま取れる。
#
# - cluster018 を 1 本 (j6 と同じ cluster で直比較可能)。
# - cap ∈ {1024, 4096, 16384}、per_shard ∈ {32, 64, 128} (j5 sweet spot 帯 + 退化帯)。
# - LEN 1M、TRIALS 5。
set -euo pipefail

DATE="${DATE:-2026-05-05}"
OUT="${OUT:-profiles/j7_twitter_pareto_${DATE}.csv}"
TRIALS="${TRIALS:-5}"
LEN="${LEN:-1000000}"
SEED="${SEED:-42}"
BENCH=./target/release/bench

CLUSTERS=("${CLUSTERS:-cluster018}")
CAPS=(1024 4096 16384)
PER_SHARDS=(32 64 128)

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
        run_one "$trial" "j5_n${n}" "$cluster" "$cap" "$ps" "$n"
        run_one "$trial" "j6_n${n}" "$cluster" "$cap" "$ps" "$n"
        run_one "$trial" "j7_n${n}" "$cluster" "$cap" "$ps" "$n"
      done
    done
  done
  echo "trial $trial done" >&2
done

echo "wrote $OUT" >&2
