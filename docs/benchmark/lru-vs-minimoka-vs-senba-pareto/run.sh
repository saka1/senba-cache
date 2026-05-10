#!/usr/bin/env bash
# 単スレ pareto: lru-rs (LRU) vs mini_moka::unsync (W-TinyLFU) vs senba::Cache (SIEVE)。
# このスクリプトを repo root から実行すると `data/arc.csv` と `data/zipf.csv` を上書きする。
# 履歴は git に任せる方針 (再現コマンドの単一 source-of-truth はここ)。
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
HERE="docs/benchmark/lru-vs-minimoka-vs-senba-pareto"
DATA="$HERE/data"
mkdir -p "$DATA"

# release build (external-traces feature で ARC zst trace を読めるようにする)。
cargo build --release -p senba-research --bin bench --features external-traces

VARIANTS="senba,lru,mini_moka_unsync"
REPEAT=3

# --- ARC paper trace P1..P14 (mokabench 同梱、cap は default_capacities 既定) ---
ARC_OUT="$DATA/arc.csv"
echo "preset,variant,source,skew,keys,len,capacity,elapsed_ns,hits,misses,evictions" > "$ARC_OUT"
for p in p1 p2 p3 p4 p5 p6 p7 p8 p9 p10 p11 p12 p13 p14; do
  echo "[$(date +%H:%M:%S)] ARC preset=$p" >&2
  ./target/release/bench --source arc --arc-preset "$p" \
    --variant "$VARIANTS" --repeat "$REPEAT" \
    | tail -n +2 | sed "s/^/$p,/" >> "$ARC_OUT"
done

# --- Zipf 合成 (α∈{0.8,1.0,1.2}, keys=1e6, len=1e7, seed=42) ---
ZIPF_OUT="$DATA/zipf.csv"
echo "skew,variant,source,bench_skew,keys,len,capacity,elapsed_ns,hits,misses,evictions" > "$ZIPF_OUT"
for skew in 0.8 1.0 1.2; do
  echo "[$(date +%H:%M:%S)] Zipf skew=$skew" >&2
  ./target/release/bench --source zipf --skew "$skew" \
    --keys 1000000 --len 10000000 --seed 42 \
    --capacity 1024,4096,16384,65536,262144 \
    --variant "$VARIANTS" --repeat "$REPEAT" \
    | tail -n +2 | sed "s/^/$skew,/" >> "$ZIPF_OUT"
done

echo "[$(date +%H:%M:%S)] data refreshed: $ARC_OUT, $ZIPF_OUT" >&2

# --- 図の再生成 ---
uv run --project scripts python "$HERE/plot.py"
