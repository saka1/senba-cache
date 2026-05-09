#!/usr/bin/env bash
# c15s (sloppy visited) Phase 1 sweep. Two stages:
#
#   (1) Concurrent throughput: c14s baseline vs c15s_{16,8,4} on uniform read-heavy
#       16T Zipf via bench_concurrent. Output: profiles/c15s_phase1_<date>.csv
#       (CSV columns are emitted by bench_concurrent as-is.)
#
#   (2) Twitter HR: 5 cluster (cluster006/016/018/019/034) × {c14s,c15s_{16,8,4}}
#       × 5 RNG seed × 3 capacity (1024/2048/4096; SHARDS=64 caps total at 4096
#       due to 6-bit per-shard ID limit). Output: profiles/c15s_hr_<date>.csv
#       (CSV columns: trial,variant,cluster,len,capacity,elapsed_ns,hits,misses,evictions)
#
# Reference: docs/reports/2026-05-10-write-contention-design-space.md §7.
set -euo pipefail

cd "$(dirname "$0")/.."

DATE="${DATE:-$(date +%Y-%m-%d)}"
THR_OUT="${THR_OUT:-profiles/c15s_phase1_${DATE}.csv}"
HR_OUT="${HR_OUT:-profiles/c15s_hr_${DATE}.csv}"
mkdir -p profiles

echo "[1/2] building bench / bench_concurrent (release)..." >&2
cargo build --release -p senba-research --bin bench --bin bench_concurrent >&2

BENCH=./target/release/bench
BENCH_CONC=./target/release/bench_concurrent

# ---- Stage 1: concurrent throughput (uniform read-heavy 16T) -----------------
TRIALS_THR="${TRIALS_THR:-5}"
THREADS="${THREADS:-16}"
CAP_THR="${CAP_THR:-4096}"   # SHARDS=64 で per-shard 64 (max)
KEYS="${KEYS:-200000}"
OPS="${OPS:-16000000}"
WARMUP="${WARMUP:-1000000}"
SEED="${SEED:-42}"

# 注: skew=1.0 が hot-key contention を引き出す唯一の設定。skew=0.0 では reader path が
# ほぼ踏まれず gate の効きが出ない (詳細は 2026-05-10-c15s-sloppy-visited.md §3.1)。
SKEW_THR="${SKEW_THR:-1.0}"
echo "[2/2] stage 1: concurrent throughput (read-heavy ${THREADS}T, skew=$SKEW_THR) -> $THR_OUT" >&2
echo "variant,trial,op_mix,skew,keys,threads,cap,shards,ops,total_elapsed_ns,aggregate_mops,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv" > "$THR_OUT"
"$BENCH_CONC" \
    --variant c14s,c15s_16,c15s_8,c15s_4 \
    --shards 64 --threads "$THREADS" --cap "$CAP_THR" --keys "$KEYS" \
    --skew "$SKEW_THR" --op-mix read-heavy \
    --ops "$OPS" --warmup "$WARMUP" --trials "$TRIALS_THR" --seed "$SEED" \
    | tail -n +2 \
    >> "$THR_OUT"

# ---- Stage 2: Twitter HR (5 cluster, 5 seed, 3 cap) --------------------------
CLUSTERS=(cluster006 cluster016 cluster018 cluster019 cluster034)
CAPS_HR=(1024 2048 4096)
SEEDS_HR=(1 2 3 4 5)
VARIANTS_HR=(c14s_n64 c15s_16_n64 c15s_8_n64 c15s_4_n64)

echo "stage 2: Twitter HR (5 cluster × ${#CAPS_HR[@]} cap × ${#SEEDS_HR[@]} seed × ${#VARIANTS_HR[@]} variant) -> $HR_OUT" >&2
echo "trial,variant,cluster,len,capacity,elapsed_ns,hits,misses,evictions" > "$HR_OUT"

for cluster in "${CLUSTERS[@]}"; do
    for seed in "${SEEDS_HR[@]}"; do
        # bench.rs は variant カンマ列を一発で受け付けるので 1 process / cluster*seed*caps
        for variant in "${VARIANTS_HR[@]}"; do
            CAPS_CSV=$(IFS=,; echo "${CAPS_HR[*]}")
            "$BENCH" --source twitter \
                     --path "external/twitter-cache-trace/${cluster}" \
                     --variant "$variant" --capacity "$CAPS_CSV" \
                     --rng-seed "$seed" \
                | tail -n +2 \
                | awk -v t="$seed" -v cl="$cluster" -F, \
                    '{printf "%s,%s,%s,%s,%s,%s,%s,%s,%s\n", t,$1,cl,$5,$6,$7,$8,$9,$10}' \
                >> "$HR_OUT"
        done
        echo "  $cluster seed=$seed done" >&2
    done
done

echo "done: $THR_OUT  $HR_OUT" >&2
