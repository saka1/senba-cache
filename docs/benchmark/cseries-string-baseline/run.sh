#!/usr/bin/env bash
# c-series 並行 variant (c14s / c16s / c17s) を u64 + String の両 value 型で
# T ∈ {4, 8, 16} × {gim@skew=1.0, read-heavy@skew=1.4} の正典点で sweep。
#
# 目的: c17s 採否判断のための Slot32-natural workload を含む比較可能 baseline を
# 一度きちんと撮る。今後 variant が増えても、ここを fix 点として参照できる。
#
# 設定値の出処は `docs/reports/2026-05-11-c17s-results.md` の "Step 2-4" 計測:
#   cap 4096, keys 100000, ops 40_000_000, warmup 1_000_000, shards 64, trials 3.
# 既存 baseline と同条件にすることで、差分を value 軸に絞って読める。
#
# ## Crash 耐性
# c14s / c16s は V=String × op-mix=read-heavy で memory corruption (seqlock-
# via-tag の racing window で `ManuallyDrop<String>` の ptr::read が半上書き
# された header を読み、その drop で free が壊れる) が出る。1 variant の
# crash で sweep 全体を巻き戻さないよう、(variant, threads, value, op-mix)
# 単位で個別に起動し、失敗は `crashes.log` に記録して次に進む。
#
# repo root から実行。`data/sweep.csv` を上書きする。
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
HERE="docs/benchmark/cseries-string-baseline"
DATA="$HERE/data"
mkdir -p "$DATA"

cargo build --release -p senba-research --bin bench_concurrent

TRIALS=3
COMMON="--shards 64 --cap 4096 --keys 100000 --ops 40000000 --warmup 1000000 --trials $TRIALS"

OUT="$DATA/sweep.csv"
LOG="$DATA/crashes.log"
HEADER="variant,trial,op_mix,value,skew,keys,threads,cap,shards,ops,total_elapsed_ns,aggregate_mops,hit_ratio,p50_chunk_ns,p99_chunk_ns,thread_throughput_cv"
echo "$HEADER" > "$OUT"
: > "$LOG"

run_one() {
  local variant="$1" value="$2" threads="$3" op_mix="$4" skew="$5"
  local label="$variant T=$threads op_mix=$op_mix skew=$skew value=$value"
  echo "[$(date +%H:%M:%S)] $label" >&2
  local tmp
  tmp=$(mktemp)
  if ./target/release/bench_concurrent --variant "$variant" $COMMON \
      --threads "$threads" --skew "$skew" --op-mix "$op_mix" --value "$value" \
      > "$tmp" 2>&1; then
    # header 1 行 + データ rows → header を除いて append
    tail -n +2 "$tmp" | grep -E "^$variant," >> "$OUT" || true
  else
    local rc=$?
    echo "[$(date +%H:%M:%S)] FAILED (rc=$rc): $label" >> "$LOG"
    tail -20 "$tmp" >> "$LOG"
    echo "---" >> "$LOG"
    # 部分的に出ていた CSV 行があれば拾う
    tail -n +2 "$tmp" | grep -E "^$variant," >> "$OUT" || true
  fi
  rm -f "$tmp"
}

for value in u64 string; do
  for threads in 4 8 16; do
    for op_mix_skew in "gim 1.0" "read-heavy 1.4"; do
      read -r op_mix skew <<< "$op_mix_skew"
      for variant in c14s c16s c17s; do
        run_one "$variant" "$value" "$threads" "$op_mix" "$skew"
      done
    done
  done
done

echo "[$(date +%H:%M:%S)] sweep complete: $OUT" >&2
if [ -s "$LOG" ]; then
  echo "[$(date +%H:%M:%S)] crashes recorded: $LOG ($(grep -c '^.*FAILED' "$LOG") cases)" >&2
fi
