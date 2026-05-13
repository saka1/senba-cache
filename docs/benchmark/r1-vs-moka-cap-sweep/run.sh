#!/usr/bin/env bash
# r1 vs moka — capacity-axis sweep (`external-lib-sweep.md` 単スレ HR map に T 軸を貼り合わせる)。
#
# 動機: `2026-05-13-r1-vs-moka-sweep.md` の sweep は cap=4096 / 4000 固定で、policy 層の
# HR drop (ARC P3/P6/S3 large cap, DS1 大 cap で W-TinyLFU が SIEVE を 7–13pp 上回る)
# が観測領域外だった。本 sweep は **mokabench canonical 容量** (`research/src/workload/
# arc_preset.rs` 経由) で ARC trace を sweep し、Zipf / Twitter は cap={1024,4096,16384,65536}
# の 4 段で cap-axis を作る。多スレ regime (T=16) で external-lib-sweep の HR 構造が
# moka 側 multi-thread regress と相殺されるかを直接観測する。
#
# # 軸
#   variants : c17s @ ways=1, r1 @ ways={1,8}, moka 0.12 sync, mini_moka 0.10 sync
#   T_LIST   : 1 4 8 16
#   value    : u64 (ARC は u64 trace、Zipf/Twitter も u64 で揃える。string 軸は
#              `r1-vs-moka-sweep.md` で確認済なので本書では skip)
#
# # Stages
#   stage_zipf     : Zipf {0.8/gim, 1.0/gim, 1.4/gim, 1.4/RH} × cap ∈ ZIPF_CAPS
#   stage_twitter  : Twitter Yang {006,019,034} × cap ∈ TWITTER_CAPS
#   stage_arc      : ARC preset {OLTP, P1, P3, P6, P8, S1, S3, DS1, ConCat, MergeP, MergeS}
#                    × そのプリセット既定 cap (mokabench 由来)
#
# # ops / warmup の cap-scaled 設定
#   bench_concurrent は固定 ops で trace を slice & cycle する。cap が大きいときは
#   working set 全体に行き渡る ops が要るため、OPS = max(cap*4, 2_000_000) を 16M で打切る。
#   WARMUP = max(cap, 200_000) を 4M で打切る。trials=3 内部反復は固定。
#
# 環境変数:
#   T_LIST          (default "1 4 8 16")
#   WAYS_LIST       (default "1 8")
#   TRIALS          (default 3)
#   ZIPF_CAPS       (default "1024 4096 16384 65536")
#   TWITTER_CAPS    (default "1024 4096 16384 65536")
#   ARC_PRESETS     (default "OLTP P1 P3 P6 P8 S1 S3 DS1 ConCat MergeP MergeS")
#   STAGES          (default "zipf twitter arc")
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
HERE="docs/benchmark/r1-vs-moka-cap-sweep"
DATA="$HERE/data"
mkdir -p "$DATA"

cargo build --release -p senba-research --bin bench_concurrent \
    --features "senba/concurrent senba-research/external-traces" >&2

T_LIST="${T_LIST:-1 4 8 16}"
WAYS_LIST="${WAYS_LIST:-1 8}"
TRIALS="${TRIALS:-3}"
ZIPF_CAPS="${ZIPF_CAPS:-1024 4096 16384 65536}"
TWITTER_CAPS="${TWITTER_CAPS:-1024 4096 16384 65536}"
ARC_PRESETS="${ARC_PRESETS:-OLTP P1 P3 P6 P8 S1 S3 DS1 ConCat MergeP MergeS}"
STAGES="${STAGES:-zipf twitter arc}"

OUT="$DATA/results.csv"
LOG="$DATA/crashes.log"
HEADER="variant,trial,ways,partitions,source,workload_param,op_mix,value,skew,keys,threads,cap,shards,ops,total_elapsed_ns,aggregate_mops,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv"

if [ -f "$OUT" ]; then
  mv "$OUT" "${OUT}.$(date +%Y%m%d-%H%M%S).bak"
fi
echo "$HEADER" > "$OUT"
: > "$LOG"

# mokabench canonical caps (`research/src/workload/arc_preset.rs` の bash 同期コピー)。
# preset 名は arc_preset_caps 関数で解決。
declare -A ARC_CAPS_MAP=(
  [OLTP]="256 512 1000 2000"
  [P1]="20000 160000"
  [P2]="20000 160000"
  [P3]="20000 160000"
  [P4]="20000 160000"
  [P5]="20000 160000"
  [P6]="20000 160000"
  [P7]="20000 160000"
  [P8]="20000 160000"
  [P9]="20000 160000"
  [P10]="20000 160000"
  [P11]="20000 160000"
  [P12]="20000 160000"
  [P13]="20000 160000"
  [P14]="80000 640000"
  [S1]="100000 800000"
  [S2]="100000 800000"
  [S3]="100000 400000 800000"
  [DS1]="1000000 4000000 8000000"
  [ConCat]="200000 400000 3200000"
  [MergeP]="400000 1000000 3200000"
  [MergeS]="400000 1000000 3200000"
)

scale_ops() {
  # cap*4 を 2M 床 / 16M 天井で打ち切り。
  local cap="$1"
  local ops=$((cap * 4))
  [ "$ops" -lt 2000000 ] && ops=2000000
  [ "$ops" -gt 16000000 ] && ops=16000000
  echo "$ops"
}

scale_warmup() {
  # cap を 200k 床 / 4M 天井で打ち切り。
  local cap="$1"
  local w="$cap"
  [ "$w" -lt 200000 ] && w=200000
  [ "$w" -gt 4000000 ] && w=4000000
  echo "$w"
}

# c17s/r1 が要求する per-shard ≤ 64 (6-bit ID) を満たす最小 SHARDS。
# senba::Cache::new(cap) の auto-shard `next_pow2(ceil(cap/64))` と同じ heuristic。
# bench_concurrent は power-of-two `--shards` を要求するので next_pow2 で丸める。
# SHARDS の最小値は 4 (dispatch arm の下限) なので、cap が小さい場合も 4 で底打ち。
scale_shards() {
  local cap="$1"
  # ceil(cap / 64)
  local need=$(( (cap + 63) / 64 ))
  [ "$need" -lt 4 ] && need=4
  # next pow2
  local s=4
  while [ "$s" -lt "$need" ]; do
    s=$(( s * 2 ))
  done
  echo "$s"
}

# r1 の WAYS は `1 <= ways <= shards` 制約。shards が小さい cell (cap=256 等) では
# WAYS_LIST=8 を 4 等にクランプしてリクエスト spec を満たす。
clamp_ways() {
  local ways="$1" shards="$2"
  [ "$ways" -gt "$shards" ] && ways="$shards"
  echo "$ways"
}

# args: variant ways source wparam threads op_mix skew cap arc_preset
run_one() {
  local variant="$1" ways="$2" source="$3" wparam="$4" threads="$5"
  local op_mix="$6" skew="$7" cap="$8" arc_preset="$9"
  local ops
  local warmup
  local shards
  ops=$(scale_ops "$cap")
  warmup=$(scale_warmup "$cap")
  # shards は variant 別: c17s/r1 は cap-scaled、moka/mini_moka は内部 shard 管理なので
  # bench_concurrent への --shards は無視されるが、harness の power-of-2 制約を満たすため
  # 同じ値を渡しておく (CSV の shards 列は emit_csv で variant 別に分岐する)。
  shards=$(scale_shards "$cap")
  if [ "$variant" = "r1" ]; then
    ways=$(clamp_ways "$ways" "$shards")
  fi
  # warmup を threads で割り切れる値に丸める (bench_concurrent assertion)
  warmup=$(( warmup / threads * threads ))
  [ "$warmup" -lt "$threads" ] && warmup="$threads"
  ops=$(( ops / threads * threads ))
  local label="$variant ways=$ways shards=$shards src=$source wp=$wparam T=$threads cap=$cap op=$op_mix s=$skew ops=$ops"
  echo "[$(date +%H:%M:%S)] $label" >&2
  local tmp
  tmp=$(mktemp)
  local extra_args=()
  case "$source" in
    zipf) extra_args=(--source zipf) ;;
    twitter-yang)
      extra_args=(--source twitter-yang --trace-file "external/twitter-cache-trace/$wparam" --workload-param "$wparam")
      ;;
    arc)
      extra_args=(--arc-preset "$arc_preset")
      ;;
  esac
  if ./target/release/bench_concurrent --variant "$variant" \
      --shards "$shards" --cap "$cap" --ops "$ops" --warmup "$warmup" --trials "$TRIALS" --seed 42 \
      --threads "$threads" --skew "$skew" --keys 100000 \
      --op-mix "$op_mix" --value u64 --ways "$ways" --partitions 1 \
      "${extra_args[@]}" > "$tmp" 2>&1; then
    tail -n +2 "$tmp" | grep -E "^$variant," >> "$OUT" || true
  else
    local rc=$?
    echo "[$(date +%H:%M:%S)] FAILED (rc=$rc): $label" >> "$LOG"
    tail -20 "$tmp" >> "$LOG"
    echo "---" >> "$LOG"
    tail -n +2 "$tmp" | grep -E "^$variant," >> "$OUT" || true
  fi
  rm -f "$tmp"
}

want_stage() {
  case " $STAGES " in
    *" $1 "*) return 0 ;;
    *) return 1 ;;
  esac
}

ZIPF_CONFIGS=(
  "0.8 gim"
  "1.0 gim"
  "1.4 gim"
  "1.4 read-heavy"
)

TWITTER_CLUSTERS=("cluster006" "cluster019" "cluster034")

# variant ごとの ways sweep 軸 (c17s / moka / mini_moka は ways=1 固定、r1 は WAYS_LIST 全展開)
ways_for_variant() {
  case "$1" in
    r1) echo "$WAYS_LIST" ;;
    *) echo "1" ;;
  esac
}

VARIANTS=(c17s r1 moka mini_moka)

if want_stage zipf; then
  echo "=== stage_zipf ===" >&2
  for variant in "${VARIANTS[@]}"; do
    ways_axis=$(ways_for_variant "$variant")
    for threads in $T_LIST; do
      for ways in $ways_axis; do
        for cap in $ZIPF_CAPS; do
          for cfg in "${ZIPF_CONFIGS[@]}"; do
            read -r skew op_mix <<< "$cfg"
            run_one "$variant" "$ways" zipf "" "$threads" "$op_mix" "$skew" "$cap" ""
          done
        done
      done
    done
  done
fi

if want_stage twitter; then
  echo "=== stage_twitter ===" >&2
  for variant in "${VARIANTS[@]}"; do
    ways_axis=$(ways_for_variant "$variant")
    for threads in $T_LIST; do
      for ways in $ways_axis; do
        for cap in $TWITTER_CAPS; do
          for cluster in "${TWITTER_CLUSTERS[@]}"; do
            trace="external/twitter-cache-trace/$cluster"
            if [ -f "$trace" ]; then
              run_one "$variant" "$ways" twitter-yang "$cluster" "$threads" gim 1.0 "$cap" ""
            fi
          done
        done
      done
    done
  done
fi

if want_stage arc; then
  echo "=== stage_arc ===" >&2
  for variant in "${VARIANTS[@]}"; do
    ways_axis=$(ways_for_variant "$variant")
    for threads in $T_LIST; do
      for ways in $ways_axis; do
        for preset in $ARC_PRESETS; do
          caps="${ARC_CAPS_MAP[$preset]:-}"
          if [ -z "$caps" ]; then
            echo "[warn] unknown preset $preset, skipping" >&2
            continue
          fi
          for cap in $caps; do
            run_one "$variant" "$ways" arc "$preset" "$threads" gim 1.0 "$cap" "$preset"
          done
        done
      done
    done
  done
fi

echo "[$(date +%H:%M:%S)] sweep complete: $OUT ($(wc -l < "$OUT") rows incl header)" >&2
if [ -s "$LOG" ]; then
  echo "[$(date +%H:%M:%S)] crashes recorded: $LOG" >&2
fi

if command -v uv >/dev/null 2>&1; then
  uv run --project scripts python "$HERE/plot.py" "$OUT" "$HERE/figures" || echo "plot.py failed" >&2
fi
