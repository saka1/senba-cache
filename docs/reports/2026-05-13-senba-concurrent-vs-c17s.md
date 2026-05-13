# senba::concurrent::Cache vs sieve_c17s — perf regression measurement

## Motivation

`senba::concurrent::Cache` (`src/concurrent/`) was promoted as a "c17s port" but it accumulated three c17s-absent deviations to fit a publishable surface:

1. **`Entry::value: Arc<V>`** + crossbeam-epoch deferred drop (for `V: Clone`, vs c17s's `V: Copy`)
2. **`free_ids: Vec<u16>` + `next_fresh_id`** in `WriterState` (for `pub fn remove()`, which c17s lacks)
3. **`Mutex<Box<WriterState>>`** (boxing to fit a 64 B `ShardHot` budget with the `Vec`)

All three sit on the reader / writer hot paths. Hypothesis: the additions move the reader from "1 ptr::read + 1 visited fetch_or" to "atomic Arc refcount RMW × 2 + epoch::pin + V::clone", which should bite hardest under Zipf contention. We had **no measurement** of how much they cost — `research/benches/sieve_cache_perf.rs` is single-thread `Cache<_, u64>` and never catches Arc-refcount cache-line ping-pong.

## Method

`docs/benchmark/senba-concurrent-vs-c17s/` (run.sh + plot.py).

- variants : `c17s` (research), `senba_concurrent` (lib) — same `--shards 512` (= cap/8, the c8x sweet spot from `2026-05-13-c17s-shard-heuristic.md`)
- workload : Zipf, `--keys 100000`, cap 4096, ops 2 M (3 trials)
- skews    : 0.8 / 1.0 / 1.4
- threads  : 1 / 4 / 8 / 16
- op_mix   : `gim` (get-if-miss-insert) / `read-heavy` (95 % get / 5 % insert with separate Zipf draw)
- value    : `u64` (so Arc / epoch overhead isolates from heavy `V::clone`)
- machine  : 12th-gen i5-12600K, 16 threads (Linux / WSL2)

48 cells × 3 trials = 144 runs.

## Result

**Median delta = −34 %, worst cell = −63 %.** The whole grid regresses; not a single cell is flat or better. Full breakdown: `docs/benchmark/senba-concurrent-vs-c17s/figures/regression_summary.md`.

### Scaling axes

| axis | low end | high end |
|---|---|---|
| threads | T=1 median −16 % | T=16 median −48 % |
| skew | 0.8 median −22 % | 1.4 median −44 % |
| op_mix | read-heavy median −22 % | gim median −44 % |

The contention dependency confirms hypothesis #1: **`Arc::increment_strong_count` / `decrement` on a shared `ArcInner.strong` is the dominant cost**, and it scales with how many cores simultaneously hit the same hot key. At T=1 skew=0.8 (least contended), senba_concurrent still loses 26 % on gim — the per-op atomic RMW + `epoch::pin` + `Arc::from_raw` + `drop(owned)` round-trip is visible even single-threaded.

### gim worse than read-heavy

Counter to a naive "more reads = more Arc traffic" reading. Two effects:

- gim's insert path adds **`Arc::new()` per miss** (heap alloc on every Path B/C), plus `epoch::pin` + `defer_unchecked` for the old value. At T=16 skew=1.4 the allocator becomes the bottleneck (gim −63 % vs read-heavy −50 % on the same cell).
- read-heavy is 95 % pure reads; the writer path runs 20× less, so writer-side overhead amortizes.

### Headline numbers (cap=4096)

```
                  c17s     senba    delta
T=1  z=1.0 gim   14.55    11.64   -20.0%
T=4  z=1.0 gim   41.06    25.09   -38.9%
T=8  z=1.0 gim   63.40    36.41   -42.6%
T=16 z=1.4 gim  184.25    67.35   -63.4%    ← worst
T=16 z=0.8 RH   100.95    82.86   -17.9%    ← best
```

`research/benches/sieve_cache_perf.rs` did not catch any of this — at T=1 the regression is "only" ~15–25 % which sits inside `cargo bench` noise, and the perf-gate's scenario set is single-threaded so contention scaling is invisible to it.

## Implications

The current `senba::concurrent::Cache` is **not a c17s port at the perf level**. It's a c17s skeleton + library API affordances that cost a third of throughput on average and over half under contention. Three concrete follow-ups worth weighing:

1. **`Cache<K, V: Copy>` specialization** — drop `Arc<V>` + epoch entirely on the Copy path. Recovers most of the −34 % for the common `u64`/`u32` value workload. The `Clone` path keeps the current Arc design as a fallback.
2. **`RwLock<ShardInner>` baseline variant** — c-series never measured the obvious "take read-lock, copy V, fetch_or visited, release" design. Even if RwLock acquire is ~10 ns, that's still cheaper than two Arc RMWs + epoch pin under contention, and it lets `tags` / `entries` revert to plain non-atomic data (which the SIMD path should auto-vectorize cleaner). Worth a `sieve_rw0` research variant before deciding the lib's surface.
3. **Add concurrent perf-gate** — the existing perf-gate doesn't cover concurrent throughput. Without one, any future Arc / epoch / Mutex change to `src/concurrent/` is unmonitored. Minimal version: 1 cell per (T, skew, mix) tuple selected from this sweep's regression hotspots, saved as a baseline.

`docs/benchmark/senba-concurrent-vs-c17s/figures/mops_vs_threads.png` is the visual summary.
