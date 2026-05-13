#!/usr/bin/env bash
# r4 sanitizer stress: TSan + ASan で V=String hot-key を 60s 走らせ data race / UAF を集める。
# 設計 §10.4 検証戦略 (C) に対応。
#
# 使い方:
#   ./docs/benchmark/r4-sanitizer/run.sh         # TSan + ASan 両方
#   SAN=tsan ./docs/benchmark/r4-sanitizer/run.sh # TSan のみ
#   SAN=asan ./docs/benchmark/r4-sanitizer/run.sh # ASan のみ
#
# Phase 2 完了 gate: TSan で WARNING ゼロ、ASan で ERROR ゼロ。

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

HERE="docs/benchmark/r4-sanitizer"
TARGET="x86_64-unknown-linux-gnu"
mkdir -p "$HERE"

SAN="${SAN:-both}"

DURATION_OPS="${DURATION_OPS:-30000000}"  # ~30s wall (sanitizer は 5-10x slow)
T="${T:-16}"

run_tsan() {
    local log="$HERE/tsan.log"
    echo "[r4-sanitizer] building with -Zsanitizer=thread..."
    RUSTFLAGS="-Zsanitizer=thread" \
        cargo +nightly build --release -p senba-research --bin bench_concurrent \
        --features senba/concurrent \
        -Zbuild-std --target "$TARGET" >&2

    echo "[r4-sanitizer] running TSan stress (V=String hot-key, T=$T)..."
    local bin="./target/$TARGET/release/bench_concurrent"
    "$bin" \
        --variant r4 --shards 512 --cap 4096 --ops "$DURATION_OPS" --warmup 2000000 --trials 1 \
        --threads "$T" --skew 1.8 --keys 1000 --op-mix read-heavy --value string \
        --ways 1 --partitions 1 --source zipf 2>&1 | tee "$log"

    if grep -q "WARNING: ThreadSanitizer:" "$log"; then
        echo "[FAIL] TSan reported races. See $log" >&2
        return 1
    fi
    echo "[OK] TSan clean."
}

run_asan() {
    local log="$HERE/asan.log"
    echo "[r4-sanitizer] building with -Zsanitizer=address..."
    RUSTFLAGS="-Zsanitizer=address" \
        cargo +nightly build --release -p senba-research --bin bench_concurrent \
        --features senba/concurrent \
        -Zbuild-std --target "$TARGET" >&2

    echo "[r4-sanitizer] running ASan stress (V=String hot-key, T=$T)..."
    local bin="./target/$TARGET/release/bench_concurrent"
    ASAN_OPTIONS="abort_on_error=1:halt_on_error=1" \
    "$bin" \
        --variant r4 --shards 512 --cap 4096 --ops "$DURATION_OPS" --warmup 2000000 --trials 1 \
        --threads "$T" --skew 1.8 --keys 1000 --op-mix read-heavy --value string \
        --ways 1 --partitions 1 --source zipf 2>&1 | tee "$log"

    if grep -qE "ERROR: AddressSanitizer:|heap-use-after-free|SEGV" "$log"; then
        echo "[FAIL] ASan reported errors. See $log" >&2
        return 1
    fi
    echo "[OK] ASan clean."
}

case "$SAN" in
    tsan) run_tsan ;;
    asan) run_asan ;;
    both) run_tsan && run_asan ;;
    *) echo "SAN must be tsan|asan|both" >&2; exit 2 ;;
esac
