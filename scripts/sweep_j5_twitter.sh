#!/usr/bin/env bash
# Twitter cache trace (OSDI'20) で j5 per_shard sweep を回し orig と比較する。
# cluster × total_cap × per_shard を直積で回し 1 cell TRIALS 回 (デフォルト 5)。
# 出力: profiles/j5_twitter_pareto_<date>.csv
#
# 設計判断:
# - cluster006/018/019 を全て使う (read-heavy 度・ユニーク key 数が異なる: 136K/156K/633K)。
# - cap ∈ {1024, 4096, 16384, 65536} で synthetic Pareto と直比較可能な帯域 + 大容量 1 点。
#   65536 は cluster006/018 では 1/2 working set 程度、019 では 1/10 程度。
# - per_shard ∈ {32, 64, 128, 256} (synthetic Pareto と同じ列で揃え、shard 細分化が
#   real trace でも faithfulness を壊さないかを直接撫でる)。
# - LEN は 1M 全行 (デフォルト)。
set -euo pipefail

DATE="${DATE:-2026-05-05}"
OUT="${OUT:-profiles/j5_twitter_pareto_${DATE}.csv}"
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
        run_one "$trial" "j5_n${n}" "$cluster" "$cap" "$ps" "$n"
      done
    done
  done
  echo "trial $trial done" >&2
done

echo "wrote $OUT" >&2
