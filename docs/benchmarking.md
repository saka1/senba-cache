# Benchmarking & profiling

This crate ships two distinct bench harnesses with very different
purposes. They live in different files because mixing them would muddy
both: the perf-regression gate needs a stable, narrow scenario set so
two runs months apart are directly comparable, while the research
microbench is a playground that gets rewritten freely as new variants
land.

| Harness                      | Default features? | Audience              | Stability                |
| ---------------------------- | :---------------: | --------------------- | ------------------------ |
| `benches/sieve_cache_perf.rs` |       yes         | Library contributors  | Stable contract          |
| `benches/micro.rs`            |       no¹         | SIEVE researchers     | Rewritten as needed      |

¹ Requires `--features experimental`. So do `src/bin/bench.rs`,
`src/bin/bench_concurrent.rs`, and `tests/oracle.rs`.

## Perf-regression gate (`benches/sieve_cache_perf.rs`)

This is the quality-gate companion to `cargo test` / `cargo clippy` for
the public `senba::Cache`. It uses three fixed scenarios
(`insert_u64` / `mixed_u64` / `insert_string`) covering the warm-up +
eviction loop on the smallest entry size, the SIMD `find` + visited
bit promotion path, and the heavier-entry drop-on-evict path. Whole run
finishes in ~10s.

Use criterion's baseline mechanism to compare before / after a change:

```bash
# before your change (or on the parent commit)
cargo bench --bench sieve_cache_perf -- --save-baseline before

# after your change
cargo bench --bench sieve_cache_perf -- --baseline before
```

Criterion prints `Performance has regressed.` / `... has improved.` per
scenario. Treat **>5% regression on any scenario** as a signal to
investigate before merging — wall-clock noise on a quiet machine is
typically ±2–3%, so 5% is a deliberate margin.

The gate uses only the public API (`Cache`, `Slot32`, `Slot64`,
`workload::zipf::ZipfGen`) — if a refactor breaks the public path, this
bench notices.

## Research microbench (`benches/micro.rs`)

The microbench is the experimental playground used to compare SIEVE
variants under synthetic workloads. It runs the `experimental` /
`sieve_orig` modules side-by-side, so it requires the feature flag.

```bash
cargo bench --bench micro --features experimental
cargo bench --bench micro --features experimental insert_only        # filter
cargo bench --bench micro --features experimental -- --profile-time 5 \
    'insert_only/v0/skew1/10000'
```

`--profile-time SECS` is criterion's flag for "skip warm-up / analysis,
just run the loop" — useful when you want to attach a profiler.

Configuration is set by `(skew, capacity)` over a Zipf trace generated
by `senba::workload::zipf`. The defaults follow NSDI'24 §5.3 /
§6.1 synthetic-Zipf shape (see `docs/sieve-paper-workload.md` if you
have access to the source tree, excluded from the published crate):

- skew α ∈ {0.6, 0.8, 1.0, 1.2}
- footprint N = 100,000 unique objects
- trace length = 1,000,000 requests (= 10× footprint)
- cache capacity = footprint × {0.1%, 1%, 10%} = {100, 1000, 10000}

Criterion writes results to `target/criterion/<group>/<case>/new/estimates.json`.
A small Python helper produces a 4-implementation comparison table:

```bash
uv run --project scripts python scripts/criterion_compare.py
```

If you change `SKEWS`, `CAP_RATIOS`, `N_KEYS`, or `TRACE_LEN` in
`benches/micro.rs`, also update the same constants in
`scripts/criterion_compare.py`.

## Profiling with samply

> SIEVE variants typically differ by single-digit to low-double-digit
> percentages, so function-level views aren't enough — you have to drop
> to source-line / instruction-level. samply (a Firefox Profiler-compatible
> sampling profiler) is the recommended tool.

### One-time setup

```bash
cargo install samply

# samply / perf use perf_event_open. To run unprivileged, lower
# perf_event_paranoid to 1 or below (effective until reboot).
echo 1 | sudo tee /proc/sys/kernel/perf_event_paranoid

# To make it permanent: write `kernel.perf_event_paranoid=1` into
# /etc/sysctl.d/99-perf.conf
```

`Cargo.toml` already sets `[profile.release].debug = "line-tables-only"`,
which is enough for `addr2line` to resolve source lines (full DWARF is
not needed).

### Capturing a profile

```bash
# Build the bench binary
cargo bench --bench micro --features experimental --no-run

# Note the path of the produced micro-XXXX binary
BIN=$(ls -t target/release/deps/micro-* | grep -v '\.d$' | head -1)

# Run a single case for 8 seconds and save the profile
mkdir -p profiles
samply record --save-only -o profiles/v0_worst.json --rate 4000 -- \
    "$BIN" --bench --profile-time 8 'insert_only/v0/skew1/10000'

samply record --save-only -o profiles/orig_worst.json --rate 4000 -- \
    "$BIN" --bench --profile-time 8 'insert_only/orig/skew1/10000'
```

`--profile-time SECS` (criterion flag) skips warm-up and analysis so
sampling sees only the steady-state loop.

### Viewing in the browser

```bash
samply load --no-open --port 3000 profiles/v0_worst.json
samply load --no-open --port 3001 profiles/orig_worst.json
```

`samply load` prints `Local server listening at http://127.0.0.1:PORT`
and a complete URL (`https://profiler.firefox.com/from-url/...?symbolServer=...`).
On WSL2, localhost is forwarded to the Windows host, so the URL works
in a Windows browser as-is. The samply symbol server resolves symbols
on demand.

Useful views:

- **Flame Graph / Stack Chart** — call hierarchy and per-function self time
- **Call Tree → Inverted** — leaf view, ranks where self-time burns at
  the std/core boundary
- **Time-range drag** — exclude `iter_batched` setup cost by selecting
  only the steady region

### Quick text view

The `scripts/` directory contains two helpers for headless analysis:

```bash
# Top self-time symbols by leaf address
uv run --project scripts python scripts/samply_top.py \
    target/release/deps/micro-XXXX \
    profiles/v0_worst.json profiles/orig_worst.json

# Source-line aggregation across inline frames; emits hot lines per
# source file plus a category breakdown.
uv run --project scripts python scripts/samply_lines.py
```

`samply_lines.py` invokes `addr2line -f -C -i` once per unique address
so it can take tens of seconds on a large profile. Edit the `BIN` path
constant at the top of the script.

### Down to instructions

In the samply UI, pick a hot function from the Call Tree and choose
"Show in source view" to see per-line sample counts. "Show in disassembly
view" then shows per-instruction sample distribution (DWARF-driven).
That is the shortest path to **"which source line / which machine
instruction is hot"** on Linux today.

## Reports archive

Long-form write-ups of past experiments live under `docs/reports/`,
indexed in `docs/reports/index.md`. They are excluded from the
published crate (so they don't end up on docs.rs / crates.io) but are
visible in the GitHub repository.
