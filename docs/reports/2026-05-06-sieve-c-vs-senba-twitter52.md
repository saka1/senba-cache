# SIEVE C reference vs senba on twitter_cluster52 (system-level wall-clock)

Date: 2026-05-06

## TL;DR

On `twitter_cluster52.csv` (1M req, 143,542 unique objects), running each
tool's full stack at three capacities (0.1% / 1% / 10% of footprint),
**single-thread, `taskset -c 0`, median of 5 runs**:

| capacity | tool | variant | miss ratio | MQPS | ns/op |
|---:|---|---|---:|---:|---:|
| 144 | cachesim | Sieve (C) | 0.4881 | 2.62 | 382 |
| 144 | senba | sieve_orig (Rust) | 0.4881 | 27.80 | 36.0 |
| 144 | senba | Cache n16 | 0.5473 | 35.28 | 28.3 |
| 1435 | cachesim | Sieve (C) | 0.3216 | 2.61 | 383 |
| 1435 | senba | sieve_orig (Rust) | 0.3216 | 34.51 | 29.0 |
| 1435 | senba | Cache n32 | 0.3658 | 33.63 | 29.7 |
| 14354 | cachesim | Sieve (C) | 0.1973 | 2.52 | 397 |
| 14354 | senba | sieve_orig (Rust) | 0.1973 | 42.90 | 23.3 |
| 14354 | senba | Cache n256 | 0.2317 | 34.70 | 28.8 |

**Headline numbers**
- senba's faithful Rust port (`sieve_orig`) is **~11–17× faster** than the
  C reference (`libCacheSim/cachesim` running `Sieve.c`).
- senba's publishable `Cache<u64,u64>` (set-associative) is **~13–14× faster**
  than cachesim, with somewhat worse hit ratio (set-associative trade-off).
- Hit ratio of `sieve_orig` matches cachesim Sieve **exactly** at all three
  capacities (sanity check: both tools ate the same trace).

**Caveat — this is system-level, not algorithm-level.** Most of the gap is
NOT the SIEVE algorithm. See [Caveats](#caveats).

## Setup

- Trace: `external/NSDI24-SIEVE/libCacheSim/data/twitter_cluster52.csv`
  (1,000,000 req, 143,542 unique objects via `awk` on column 2 ignoring
  the `# ...` header).
- Capacities: 0.1% / 1% / 10% of footprint = **144 / 1435 / 14354** objects.
- cachesim:
  ```
  taskset -c 0 cachesim <trace> csv Sieve <cap> --ignore-obj-size 1 \
    -t "obj-id-col=2,delimiter=,,has-header=true,obj-id-is-num=true"
  ```
  Throughput parsed from the formatter line at `sim.c:73`.
- senba bench (release):
  ```
  taskset -c 0 ./target/release/bench --source libcachesim-csv \
    --path <trace> --capacity <cap> --variant <v>
  ```
  Wall time measured by `Instant` around the get-then-insert loop only;
  trace is parsed into `Vec<u64>` first (not included in measurement).
- Variants: cachesim `Sieve`; senba `sieve_orig` (faithful port) and
  `senba::Cache<u64,u64,Slot32,N>` with N chosen to keep per-shard ≤ 64
  (n16 for cap=144, n32 for cap=1435, n256 for cap=14354).
- 5 runs each, median reported.
- Raw run-by-run numbers in `docs/reports/data/2026-05-06-sieve-c-vs-senba-twitter52.csv`.

A small reader was added to senba (`workload::file::libcachesim_csv_from_path`)
so it consumes the libCacheSim CSV format directly: skip `#` lines, parse
column 1 as `u64`. Both tools therefore see identical keys; the matching HR
confirms it.

## Observations

1. **HR matches `sieve_orig`↔cachesim exactly** at all caps (0.4881, 0.3216,
   0.1973). The trace pipeline is good.
2. **`senba::Cache`'s HR is consistently worse** than canonical SIEVE
   (e.g. 0.2317 vs 0.1973 at cap=14354). Expected: it's a set-associative
   layout, not the global-queue SIEVE. This trade-off is documented in the
   `senba::Cache` design and is the reason `sieve_orig` exists alongside.
3. **cachesim's MQPS is essentially flat** across capacities (2.52–2.65).
   This strongly suggests its bottleneck is **per-request fixed cost**
   (CSV scanf + `request_t` build + vtable dispatch), not the SIEVE work,
   which scales differently with cap.
4. **senba's MQPS rises with cap** (27.80 → 34.51 → 42.90 for `sieve_orig`).
   This is the opposite signal: bigger caches mean fewer evictions and
   hand-walks per request, so the algorithm cost falls.
5. **`senba::Cache` is roughly cap-independent** (~33–35 MQPS), suggesting
   its bottleneck is the bucket lookup itself rather than eviction work —
   consistent with set-associative scans.

## Caveats

The 11–17× ratio is **not** a SIEVE-algorithm comparison. The harnesses
differ in ways that load most of the runtime:

- **Trace I/O is in the timing on cachesim, not on senba.** cachesim
  streams CSV from disk and parses each line per request inside the
  measured loop. senba reads the CSV upfront into `Vec<u64>` and only
  the replay loop is measured. CSV parsing alone is plausibly several
  hundred ns/op, which would account for most of cachesim's ~382 ns/op.
- **Generic `cache_t` vtable + per-request `request_t`.** Every cachesim
  request goes through indirect calls and a struct that carries size, ttl,
  and other fields we ignore.
- **Hashtable.** cachesim uses its own (glib `GHashTable`-derived) table;
  senba uses hashbrown. Hashbrown is widely faster.
- **Rust release codegen vs default cmake build flags.** cmake build is
  Release per its summary, but exact flag parity (LTO, codegen units) is
  not checked.

Net: this experiment establishes that **senba's stack delivers
order-of-magnitude higher throughput than libCacheSim's stack at the same
HR**, which is a practically useful number, but it does **not** show that
the Rust SIEVE *algorithm* is faster than the C SIEVE *algorithm*. To
isolate the algorithm, run the C `Sieve.c` via FFI from senba's bench
harness so both see the same trace replay loop and hashmap. That is option
(b) from the design discussion and is left as follow-up.

A useful intermediate would be converting the trace to `oracleGeneralBin`
format (libCacheSim's binary trace) and re-running cachesim on it; if the
flat ~2.5 MQPS jumps significantly, the bottleneck is confirmed as CSV
parsing rather than the cache stack itself.

## Follow-up: cachesim on oracleGeneral binary trace

To isolate how much of cachesim's flat ~2.5 MQPS was CSV parse cost,
the trace was converted to libCacheSim's binary `oracleGeneral` format
(24 B/req, no per-request scanf):

```
traceConv twitter_cluster52.csv csv -o twitter_cluster52.oracleGeneral \
  -t "obj-id-col=2,delimiter=,,has-header=true,obj-id-is-num=true"
```

23 MB output, 1M req, working set 143,542 — matches earlier footprint.
Re-running cachesim with the same Sieve algorithm and caps:

| capacity | trace | miss ratio | MQPS (median of 5) | speedup vs CSV |
|---:|---|---:|---:|---:|
| 144 | csv | 0.4881 | 2.62 | — |
| 144 | oracleGeneral | 0.4881 | **7.56** | **2.9×** |
| 1435 | csv | 0.3216 | 2.61 | — |
| 1435 | oracleGeneral | 0.3216 | **7.72** | **3.0×** |
| 14354 | csv | 0.1973 | 2.52 | — |
| 14354 | oracleGeneral | 0.1973 | **7.47** | **3.0×** |

Findings:
- **CSV parse was ~⅔ of cachesim's wall-clock**. Roughly cap-independent
  3× speedup confirms the CSV path was a per-request fixed cost.
- HR identical between CSV and oracleGeneral runs across all caps (sanity
  on the conversion).
- **cachesim-bin MQPS still flat** (7.47–7.72 across caps). The flatness
  signature has not gone away — it just moved from CSV-parse to whatever
  is left (vtable dispatch, `request_t` build, glib hashtable).
- **senba vs cachesim-bin gap: ~4–6×**, down from ~11–17× on the CSV
  comparison. The remaining gap is the genuine cache-stack overhead
  (libCacheSim's generic `cache_t` plumbing + hashtable). The SIEVE
  algorithm proper appears to be small relative to that infrastructure.

### Updated headline table

| capacity | tool | variant | miss ratio | MQPS | gap |
|---:|---|---|---:|---:|---:|
| 144 | cachesim-bin | Sieve (C) | 0.4881 | 7.56 | 1.0× |
| 144 | senba | sieve_orig (Rust) | 0.4881 | 27.80 | 3.7× |
| 144 | senba | Cache n16 | 0.5473 | 35.28 | 4.7× |
| 1435 | cachesim-bin | Sieve (C) | 0.3216 | 7.72 | 1.0× |
| 1435 | senba | sieve_orig (Rust) | 0.3216 | 34.51 | 4.5× |
| 1435 | senba | Cache n32 | 0.3658 | 33.63 | 4.4× |
| 14354 | cachesim-bin | Sieve (C) | 0.1973 | 7.47 | 1.0× |
| 14354 | senba | sieve_orig (Rust) | 0.1973 | 42.90 | 5.7× |
| 14354 | senba | Cache n256 | 0.2317 | 34.70 | 4.6× |

The 4–6× gap is still **not an algorithm comparison** — it bundles the
two stacks' hashmap, dispatch model, and per-request bookkeeping. To
pin it down further, the FFI option (b) is required: link `Sieve.c`
into senba's bench harness so the only difference between two timed
runs is the cache implementation.

## Files

- Spec: [`2026-05-06-sieve-c-vs-senba-twitter52-design.md`](2026-05-06-sieve-c-vs-senba-twitter52-design.md)
- Raw data (csv): [`data/2026-05-06-sieve-c-vs-senba-twitter52.csv`](data/2026-05-06-sieve-c-vs-senba-twitter52.csv)
- Raw data (oracleGeneral): [`data/2026-05-06-sieve-c-vs-senba-twitter52-oraclegen.csv`](data/2026-05-06-sieve-c-vs-senba-twitter52-oraclegen.csv)
- Reader: `src/workload/file.rs::libcachesim_csv_from_path`
- Bench arm: `src/bin/bench.rs` `--source libcachesim-csv`
