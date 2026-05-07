# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project goal

`senba-cache` is a sandbox for exploring **good Rust implementations of the SIEVE eviction algorithm** (NSDI'24, Zhang et al.). The aim is to develop and compare multiple variants — starting from a faithful port of the original C reference, then iterating toward Rust-idiomatic / high-performance designs — and study the trade-offs between them (correctness, allocation behavior, cache locality, ergonomics).

A second, downstream aim is to **harvest the results of that experimentation into a publishable library**: the variant that wins out on the sandbox benches gets promoted to `src/sieve_cache/` and treated as a stable, perf-gated public surface (`benches/sieve_cache_perf.rs`). The experimental modules under `src/experimental/` stay as research artifacts behind the `experimental` feature flag; the library surface is what we intend to ship. Decisions that affect the library surface (API additions, semantics, perf contract) should be made with that downstream use in mind, not just sandbox convenience.

The NSDI'24 paper is at https://yazhuozhang.com/assets/publication/nsdi24-sieve.pdf, and the authors' reference C implementation is included as a git submodule at `external/NSDI24-SIEVE/` (https://github.com/cacheMon/NSDI24-SIEVE).

## Feature flags

The crate has one feature, `experimental`, which gates the entire
research / dev surface — `src/experimental/` (historical variants +
`sieve_orig` oracle port + the `CacheImpl` trait), `src/workload/`
(Zipf / trace replay), and the `impl CacheImpl for Cache` adapter on
the library `Cache`. The publishable surface (`Cache`, `Drain`,
`Stats`, `SlotSize`, and `sieve_cache::hash::Xxh3Build`) compiles on
default features and is the *only* thing under `src/sieve_cache/`.

Targets that depend on the research surface have `required-features =
["experimental"]` in `Cargo.toml` and **silently skip** without it:

| Target                          | Default features | `--features experimental` |
| ------------------------------- | :--------------: | :-----------------------: |
| lib unit tests (public surface) | runs (~106)      | runs (~294, includes oracle cross-checks in `src/sieve_cache/tests/cache.rs`) |
| `tests/oracle.rs`               | skipped          | runs                      |
| `benches/sieve_cache_perf.rs`   | skipped (uses `workload`) | runs              |
| `benches/micro.rs`              | skipped          | runs                      |
| `src/bin/bench`, `bench_concurrent` | skipped      | builds                    |

Rule of thumb: **always iterate with `--features experimental`** —
the perf bench, oracle cross-checks, and every research driver need
it. Default-feature commands are only for verifying that the library
surface (`src/sieve_cache/` + `src/lib.rs`) compiles standalone, which
is the publish path.

## Commands

```bash
# library / publish surface (= what crates.io consumers see)
cargo check
cargo test --lib
cargo clippy

# full sandbox (oracle, experimental variants, research drivers)
cargo check  --features experimental
cargo test   --features experimental
cargo clippy --all-targets --all-features

# single test
cargo test --features experimental <name>
```

## Quality Gates

After any code change, ensure all pass:

```bash
cargo fmt                                            # auto-format
cargo clippy --all-targets --all-features            # zero warnings (must
                                                     # include tests — CI
                                                     # runs --all-targets;
                                                     # --all-features picks
                                                     # up experimental)
cargo test  --features experimental                  # full test surface
```

For pure public-API edits (changes confined to `src/sieve_cache/`),
it's fine to run the default-feature gates first as a fast inner loop,
then the `--features experimental` gates before commit. Anything
touching `src/experimental/` (including `sieve_orig`) or `src/workload/`
requires the feature flag to even compile.

### Performance regression check (`benches/sieve_cache_perf.rs`)

Whenever a change touches `src/sieve_cache/` (including its internal
`hash` submodule) in ways that could plausibly affect performance —
hot-path edits, layout changes, dispatch changes, new branches in `find` /
`insert` / `evict_one_returning_id`, etc. — run the perf-gate bench with
criterion's baseline mechanism:

```bash
# before your change (or on the parent commit)
cargo bench --bench sieve_cache_perf -- --save-baseline before
# after your change
cargo bench --bench sieve_cache_perf -- --baseline before
```

The bench has three scenarios (insert_u64 / mixed_u64 / insert_string).
Criterion prints `Performance has regressed.` / `... has improved.` per
scenario; treat **>5% regression on any scenario** as a signal to
investigate before commit. Sampling and noise-threshold tuning live in the
bench file itself.

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

Public (publishable) surface — compiles on default features, shipped to crates.io. The crates.io payload is allowlisted in `Cargo.toml`'s `package.include` to **exactly** these files:

- `src/sieve_cache/` — **library-grade SIEVE implementation** (`Cache`, `Drain`, `Stats`, `SlotSize` and the `Slot16/32/64` brackets). Split across `mod.rs` (Cache + Inner), `iter.rs` (`Iter`/`IterMut`/`Keys`/`Values`/`Drain`), `slot.rs` (`SlotSize` sealed trait), `stats.rs` (`Stats`), `hash.rs` (`Xxh3Build` — the default `H` on `Cache`), with tests under `tests/`. `benches/sieve_cache_perf.rs` guards its perf.
- `src/lib.rs` — module declarations and re-exports. `experimental` and `workload` are both `#[cfg(feature = "experimental")]`-gated.

Research surface — gated behind the `experimental` feature flag, **not** in the crates.io payload (filtered out by `package.include`):

- `src/experimental/` — historical / exploratory SIEVE variants (`sieve_v0..v3`, `sieve_j3..j8`, `sieve_c8`) plus `sieve_orig` (the oracle), each a self-contained module exposing the v0-style API (`new`, `len`, `capacity`, `contains_key`, `get(&mut)`, `insert -> Option<(K,V)>`). Used by `benches/micro.rs` and the `bin/bench*` harnesses for comparison.
- `src/experimental/mod.rs` also defines `pub trait CacheImpl<K, V>` — the cross-variant interface implemented by every variant (including `Cache`, behind the same feature gate). Re-exported at the crate root as `senba_cache::CacheImpl` when the feature is on.
- `src/experimental/sieve_orig.rs` — **faithful port of the NSDI'24 author reference** (`external/NSDI24-SIEVE/.../Sieve.c`). Doubly-linked list + single hand + per-entry visited bit, in safe Rust via an arena. **Treat this as the spec / oracle** — every variant's hit/miss behavior on any trace must match `sieve_orig` exactly. Oracle cross-checks inside `src/sieve_cache/tests/cache.rs` are tagged `#[cfg(feature = "experimental")]`.
- `src/workload/` — Zipf generator + trace replay utilities. Used by the perf-gate, microbench, and oracle test.
- `src/bin/bench.rs`, `src/bin/bench_concurrent.rs` — research drivers comparing senba's `Cache`, the experimental variants, `mini-moka`, and `moka`. The `mini-moka` / `moka` / `parking_lot` / `rand` / `rand_distr` deps are `optional = true` and only pulled in by the `experimental` feature.
- `tests/oracle.rs`, `benches/micro.rs`, `benches/sieve_cache_perf.rs` — same gating.
- `docs/reports/` — write-ups of what each experiment showed (see "Documenting results" below). The code is the artifact; the reports are the conclusions.

## Documenting results

The sandbox half of this project is research-driven, and reports are how we keep that research from evaporating between sessions. Every time a meaningful experiment concludes — a benchmark run, a profiling session, an overhead analysis, a design comparison — write a report under `docs/reports/YYYY-MM-DD-<topic>.md` before moving on. Reports also serve as the rationale trail for what eventually lands in the library surface.

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
