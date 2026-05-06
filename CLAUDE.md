# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project goal

`senba-cache` is a sandbox for exploring **good Rust implementations of the SIEVE eviction algorithm** (NSDI'24, Zhang et al.). The aim is to develop and compare multiple variants — starting from a faithful port of the original C reference, then iterating toward Rust-idiomatic / high-performance designs — and study the trade-offs between them (correctness, allocation behavior, cache locality, ergonomics).

The NSDI'24 paper is at https://yazhuozhang.com/assets/publication/nsdi24-sieve.pdf, and the authors' reference C implementation is included as a git submodule at `external/NSDI24-SIEVE/` (https://github.com/cacheMon/NSDI24-SIEVE).

## Commands

```bash
cargo check          # type-check without producing artifacts
cargo build          # build the library
cargo test           # run all tests
cargo test <name>    # run a single test by name
cargo clippy         # lint
```

## Quality Gates

After any code change, ensure all pass:

```bash
cargo fmt                    # auto-format
cargo clippy --all-targets   # zero warnings (must include tests — CI runs --all-targets)
cargo test                   # all tests pass
```

### Performance regression check (`benches/sieve_cache_perf.rs`)

Whenever a change touches `src/sieve_cache.rs` or the modules it depends on
(`hash`, `workload`, …) in ways that could plausibly affect performance —
hot-path edits, layout changes, dispatch changes, new branches in `find` /
`insert` / `evict_one_returning_id`, etc. — run the perf-gate bench with
criterion's baseline mechanism:

```bash
# before your change (or on the parent commit)
cargo bench --bench sieve_cache_perf -- --save-baseline before
# after your change
cargo bench --bench sieve_cache_perf -- --baseline before
```

The bench has three scenarios (insert_u64 / mixed_u64 / insert_string) and
finishes in ~10s. Criterion prints `Performance has regressed.` /
`... has improved.` per scenario; treat **>5% regression on any scenario**
as a signal to investigate before commit. Wall-clock noise on a quiet
machine is typically ±2–3%, so 5% is a deliberate margin.

Skipping the perf-gate is fine for pure documentation / test-only / clippy
fixes — anything that demonstrably cannot affect the compiled `Cache` hot
path. When in doubt, run it; it's cheap.

This bench is **separate from `benches/micro.rs`** by design. `micro.rs` is
the experimental playground (variants come and go, scenarios get rewritten
freely); `sieve_cache_perf.rs` is the stable contract for the library
`Cache` and should only be edited deliberately, with the understanding that
edits invalidate prior saved baselines.

## Plot / analysis scripts (Python)

Auxiliary plotting and analysis scripts live in `scripts/` as a **separate
uv project** (`scripts/pyproject.toml`, `scripts/uv.lock`,
`scripts/.python-version`). Run from anywhere in the repo with:

```bash
uv run --project scripts python scripts/<name>.py
```

Scripts resolve data paths via `Path(__file__).resolve().parent.parent`,
so the current working directory does not matter.

## Conventions

- Write all source code (identifiers, comments, doc comments) in **English**. Reports and other documentation do not need to be in English.
- When including assembly in code, comments, or reports, use **Intel syntax** (e.g. `mov rax, [rdi]`), not AT&T.

## Architecture

Top-level (library surface):

- `src/cache.rs` — `Cache<K, V>` trait (placeholder; SIEVE's `get` needs `&mut self`, so trait alignment is future work).
- `src/sieve_cache.rs` — **library-grade SIEVE implementation** (`Cache`, `SlotSize`). This is the stable, benchmarked surface; `benches/sieve_cache_perf.rs` guards its perf.
- `src/sieve_orig.rs` — **faithful port of the NSDI'24 author reference** (`external/NSDI24-SIEVE/.../Sieve.c`). Doubly-linked list + single hand + per-entry visited bit, in safe Rust via an arena. **Treat this as the spec / oracle** — every variant's hit/miss behavior on any trace must match `sieve_orig` exactly.
- `src/hash.rs`, `src/workload/` — shared utilities (xxh3, Zipf, trace replay).
- `src/lib.rs` — module declarations and re-exports.

Experimental variants:

- `src/experimental/` — historical / exploratory SIEVE variants (`sieve_v0..v3`, `sieve_j3..j8`, `sieve_c8`), each a self-contained module exposing the v0-style API (`new`, `len`, `capacity`, `contains_key`, `get(&mut)`, `insert -> Option<(K,V)>`). Used by `benches/micro.rs` and the `bin/bench*` harnesses for comparison; **not part of the library surface**.
- `docs/reports/` — write-ups of what each experiment showed (see "Documenting results" below). The code in `src/experimental/` is the artifact; the reports are the conclusions.

## Documenting results

This is a research/investigation project, not a product. Every time a meaningful experiment concludes — a benchmark run, a profiling session, an overhead analysis, a design comparison — write a report under `docs/reports/YYYY-MM-DD-<topic>.md` before moving on.

A report is warranted when:
- A benchmark produces numbers worth keeping (even if the result is "it didn't improve")
- An investigation reaches a conclusion (root cause found, overhead quantified, design trade-off resolved)
- A design decision is made that future experiments depend on

Reports are the primary output of this project. Code and bench numbers that aren't documented are effectively lost between sessions.

**Index:** `docs/reports/index.md` contains a one-paragraph summary of every report. Read the index first to orient yourself without opening individual files. Whenever you add or substantially revise a report, update the corresponding entry in the index (or add a new one).

## Adding a new SIEVE variant

1. Add `src/experimental/sieve_<name>.rs` with the same public API as `sieve_orig`.
2. Register it in `src/experimental/mod.rs`.
3. Mirror the test names in `sieve_orig` so equivalent behavior is checked side-by-side.
4. The bar for correctness is "produces the same evicted-key sequence as `sieve_orig` on the same input trace" — not just "passes the unit tests".
