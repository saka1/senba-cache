#!/usr/bin/env bash
# sweep: skew × total_cap × per_shard を直積で回し、orig と j5_n{N} の
# elapsed_ns / hits / misses / evictions を CSV にまとめる。
# 1 cell あたり TRIALS 回 (デフォルト 5)。trial 列を付加して raw 行を出す。
#
# 出力: profiles/j5_pershard_pareto_2026-05-05.csv
set -euo pipefail

OUT="${OUT:-profiles/j5_pershard_pareto_2026-05-05.csv}"
TRIALS="${TRIALS:-5}"
KEYS="${KEYS:-100000}"
LEN="${LEN:-1000000}"
SEED="${SEED:-42}"
BENCH=./target/release/bench

SKEWS=(0.9 1.0 1.2)
CAPS=(1024 4096 16384)
PER_SHARDS=(32 64 128 256)

mkdir -p profiles
echo "trial,variant,source,skew,keys,len,capacity,per_shard,shards,elapsed_ns,hits,misses,evictions" > "$OUT"

run_one() {
  local trial=$1 variant=$2 skew=$3 cap=$4 per_shard=$5 shards=$6
  "$BENCH" --source zipf --skew "$skew" --keys "$KEYS" --len "$LEN" --seed "$SEED" \
           --capacity "$cap" --variant "$variant" \
    | tail -n +2 \
    | awk -v t="$trial" -v ps="$per_shard" -v sh="$shards" -F, \
        '{printf "%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n", t,$1,$2,$3,$4,$5,$6,ps,sh,$7,$8,$9,$10}' \
    >> "$OUT"
}

for trial in $(seq 1 "$TRIALS"); do
  for skew in "${SKEWS[@]}"; do
    for cap in "${CAPS[@]}"; do
      # orig は per_shard 不変 (cap だけ依存) なので per_shard=cap, shards=1 として記録
      run_one "$trial" "orig" "$skew" "$cap" "$cap" 1
      for ps in "${PER_SHARDS[@]}"; do
        if (( cap % ps != 0 )); then continue; fi
        n=$(( cap / ps ))
        # n は power of two かつ既知の variant のみ
        case "$n" in
          1|2|4|8|16|32|64|128|256|512) ;;
          *) continue ;;
        esac
        run_one "$trial" "j5_n${n}" "$skew" "$cap" "$ps" "$n"
      done
    done
  done
  echo "trial $trial done" >&2
done

echo "wrote $OUT" >&2
