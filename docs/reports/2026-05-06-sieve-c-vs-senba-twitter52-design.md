# Design: SIEVE C reference vs senba on twitter_cluster52 (option a)

Date: 2026-05-06
Status: design (pre-experiment)

## Goal

Run libCacheSim's `cachesim` (C reference SIEVE) and senba's bench (Rust:
`sieve_orig`, `senba::Cache`) against the same Twitter trace at the same
capacities, and compare wall-clock throughput side by side.

This is **system-level / implementation-stack comparison**, not isolated
algorithm comparison. The numbers include each tool's own trace reader,
hashmap, and dispatch overhead. That's accepted up front; the value is a
quick first read on whether senba's Rust implementations are in the right
ballpark relative to the C reference.

If the result is interesting, an isolated FFI-based comparison (option b)
follows in a separate experiment.

## Trace

`external/NSDI24-SIEVE/libCacheSim/data/twitter_cluster52.csv` (1,000,000
requests). Format:

```
# time, object, size, next_access_vtime
0, 13053225291711363978, 737, 13
```

Both tools must consume this file directly; no preprocessing.

## Capacity sweep

Three points: 0.1%, 1%, 10% of unique-object footprint.

Footprint is computed once via a one-shot `awk` over the CSV (skip header,
unique col 2). Resulting absolute capacities are reused verbatim by both
tools so their cache budgets match.

If 0.1% lands in the low-hundreds and ends up dominated by noise, the
sweep is rebalanced to 1% / 10% / 50%. Decision made after the first run.

## Variants

- C: libCacheSim's `Sieve` (`external/NSDI24-SIEVE/libCacheSim/libCacheSim/cache/eviction/Sieve.c`)
- Rust: `senba::sieve_orig::SieveCache` (faithful port), `senba::Cache` (publishable best variant)

## Tooling changes (senba side)

### `src/workload/file.rs`

Add `libcachesim_csv_from_path(path) -> io::Result<impl Iterator<Item = u64>>`:
- skip lines starting with `#`
- `split(',').nth(1).trim().parse::<u64>()`
- streaming, mirrors existing `from_path` / `twitter_csv_from_path` shape

### `src/bin/bench.rs`

Add `--source libcachesim-csv` arm that calls the new reader. Routes into
the existing u64-key `drive` path (no String-key variant needed; libCacheSim
CSV gives integer object ids).

No other bench changes.

## Run protocol

Both tools:
- release build
- single thread
- `taskset -c 0`
- 5 runs per (variant, capacity), median reported
- no turbo control (noise tolerated as long as variant ordering is stable)

### cachesim

```
cachesim <trace> csv Sieve <size> --ignore-obj-size 1 \
  --trace-type-params "obj-id-col=2,delimiter=,,has-header=true"
```

`sim.c:73` formatter emits `req_cnt` and `runtime`. Throughput derived from
those.

### senba bench

```
cargo run --release --bin bench -- \
  --source libcachesim-csv --path <trace> \
  --capacity <c1>,<c2>,<c3> --variant orig,senba
```

Existing CSV (`elapsed_ns`, `hits`, `misses`) is used directly.

## Output

Report at `docs/reports/2026-05-06-sieve-c-vs-senba-twitter52.md`. Table
columns: variant × capacity × {req/s, ns/op, hit ratio}. Hit ratio is the
sanity check that all three tools saw the same trace; if HR diverges
between cachesim and senba, the trace pipeline is broken and the run is
discarded.

Index entry added to `docs/reports/index.md`.

## Steps

1. Build libCacheSim via CMake. Sanity-run `cachesim` once on the trace.
2. Add reader + bench source arm. Sanity-run senba bench once.
3. Compute footprint, derive 3 caps.
4. Run both tools at 3 caps × 5 runs.
5. Tabulate, write report, update index.

## Out of scope

- Isolated SIEVE-only comparison (FFI). Separate experiment.
- libCacheSim trace formats other than CSV (oracleGeneral binary etc.).
- Multiple traces. cluster52 only this round.
- Concurrency. Both runs are single-thread.
