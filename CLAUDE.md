# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project goal

`senba-cache` is a sandbox for exploring **good Rust implementations of the SIEVE eviction algorithm** (NSDI'24, Zhang et al.). The aim is to develop and compare multiple variants — starting from a faithful port of the original C reference, then iterating toward Rust-idiomatic / high-performance designs — and study the trade-offs between them (correctness, allocation behavior, cache locality, ergonomics).

A second, downstream aim is to **harvest the results of that experimentation into a publishable library** (the `senba` crate on crates.io): the variant that wins out on the sandbox benches gets promoted to the workspace-root `senba` crate (`src/lib.rs` + flat sibling modules) and treated as a stable, perf-gated public surface (`research/benches/sieve_cache_perf.rs`). The experimental modules under `research/src/experimental/` stay as research artifacts in the non-publishable `senba-research` crate; the `senba` crate is what we intend to ship. Decisions that affect the library surface (API additions, semantics, perf contract) should be made with that downstream use in mind, not just sandbox convenience.

The NSDI'24 paper is at https://yazhuozhang.com/assets/publication/nsdi24-sieve.pdf, and the authors' reference C implementation is included as a git submodule at `external/NSDI24-SIEVE/` (https://github.com/cacheMon/NSDI24-SIEVE).

## Workspace layout

The repo is a **two-member Cargo workspace**:

| Member          | Path        | Publish?     | Purpose                                                                                  |
| --------------- | ----------- | ------------ | ---------------------------------------------------------------------------------------- |
| `senba`         | `./`        | yes (crates.io) | Library surface: `Cache`, `Drain`, `Stats`, `SlotSize` (`Slot16/32/64`), `hash::Xxh3Build`. |
| `senba-research`| `research/` | `publish = false` | Historical / exploratory SIEVE variants (`experimental/`), Zipf + trace replay (`workload/`), research drivers (`bin/bench*`), oracle tests, micro-bench. Depends on `senba` via path dep. |

The `senba` package's crates.io payload is allowlisted in `Cargo.toml`'s
`package.include` to exactly the publishable surface — required because the
package directory is the workspace root and would otherwise pull in everything
(`research/`, `external/`, `docs/`, etc.).

`senba::Cache` implements `senba_research::CacheImpl` in `research/src/lib.rs`
via the orphan rule (the trait is local to `senba-research`). This lets
cross-variant drivers and the oracle test treat senba's library `Cache` and
the experimental variants symmetrically.

## Commands

```bash
# publishable surface (= what crates.io consumers see)
cargo check  -p senba
cargo test   -p senba
cargo clippy -p senba

# research surface (experimental variants + oracle + drivers)
cargo check  -p senba-research
cargo test   -p senba-research
cargo clippy -p senba-research --all-targets

# whole workspace (both crates, all targets)
cargo check --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets

# single test
cargo test -p senba-research <name>
```

### Tests gated by the `external-traces` feature

A handful of oracle tests in `research/tests/oracle.rs`
(`*_matches_orig_on_bundled_zipf`) read trace files from the
`external/NSDI24-SIEVE` git submodule. They are gated behind the
**default-off** `external-traces` feature on `senba-research` so that
`cargo test --workspace` in CI works without initializing the submodule.

To run them locally (after `git submodule update --init`):

```bash
cargo test -p senba-research --features external-traces
# or just the oracle target:
cargo test -p senba-research --features external-traces --test oracle
```

When adding a new test that depends on a file under `external/`, gate it the
same way: `#[cfg(feature = "external-traces")]` on the `#[test]` fn (and on
any imports that become unused without the feature, e.g. `workload::file`).

## Quality Gates

After any code change, ensure all pass:

```bash
cargo fmt --all                                      # auto-format
cargo clippy --workspace --all-targets -- -D warnings  # zero warnings
cargo test --workspace                               # full test surface
```

For pure public-API edits confined to the senba crate (`src/lib.rs` and its
flat sibling modules), it's fine to run the senba-only gates first as a fast
inner loop, then the full workspace gates before commit:

```bash
cargo check  -p senba
cargo test   -p senba
cargo clippy -p senba -- -D warnings
```

### Performance regression check (`research/benches/sieve_cache_perf.rs`)

Whenever a change touches the senba library files (`src/lib.rs`, `src/shard.rs`,
`src/iter.rs`, `src/slot.rs`, `src/stats.rs`, `src/hash.rs`) in ways that
could plausibly affect performance — hot-path edits, layout changes, dispatch
changes, new branches in `find` / `insert` / `evict_one_returning_id`, etc. —
run the perf-gate bench with criterion's baseline mechanism:

```bash
# before your change (or on the parent commit)
cargo bench -p senba-research --bench sieve_cache_perf -- --save-baseline before
# after your change
cargo bench -p senba-research --bench sieve_cache_perf -- --baseline before
```

The bench has six scenarios (insert_u64 / mixed_u64 / insert_string /
insert_u32_slot16 / get_heavy_u64 / mixed_lowskew_u64) covering the three
slot strides (Slot16/32/64), two op-mix points (50/50, 90/10) and two
Zipf skews (1.0, 0.7). Criterion prints `Performance has regressed.` /
`... has improved.` per scenario; treat **>5% regression on any scenario**
as a signal to investigate before commit. If a regression appears on a
single scenario but Twitter trace cross-check (`research/src/bin/bench`,
`--source twitter`/`twitter-string`) goes the other direction, the
regression is likely scenario-specific layout noise — see
`docs/reports/2026-05-07-aligned-tags-load.md` for one such case.
Sampling and noise-threshold tuning live in the bench file itself.

The bench lives in `senba-research` (it depends on `senba_research::workload`
for Zipf generation), but the contract it gates is `senba::Cache`. Edits to
the senba library hot path must respect this gate.

Skipping the perf-gate is fine for pure documentation / test-only / clippy
fixes — anything that demonstrably cannot affect the compiled `Cache` hot
path. When in doubt, run it; it's cheap.

This bench is **separate from `research/benches/micro.rs`** by design.
`micro.rs` is the experimental playground (variants come and go, scenarios
get rewritten freely); `sieve_cache_perf.rs` is the stable contract for the
library `Cache` and should only be edited deliberately, with the
understanding that edits invalidate prior saved baselines.

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

### `senba` (publishable)

The crates.io payload is allowlisted in the root `Cargo.toml`'s
`package.include` to **exactly** these files:

- `src/lib.rs` — **library-grade SIEVE implementation** (`Cache`, `Drain`, `Stats`, `SlotSize` and the `Slot16/32/64` brackets). Holds the public `Cache` type plus the module declarations / re-exports for the flat sibling files.
- `src/shard.rs` — per-shard state (the SIEVE state machine, SIMD `find`, evict / insert / remove). `src/iter.rs` — iterator types (`Iter`/`IterMut`/`Keys`/`Values`/`Drain`). `src/slot.rs` — `SlotSize` sealed trait. `src/stats.rs` — `Stats`. `src/hash.rs` — `Xxh3Build` (the default `H` on `Cache`). `src/tests/` — unit tests, split by topic.
- `research/benches/sieve_cache_perf.rs` guards perf for this set (lives in `senba-research`).

### `senba-research` (non-publishable)

- `research/src/experimental/` — historical / exploratory SIEVE variants (`sieve_v0..v3`, `sieve_j3..j8`, `sieve_c8`) plus `sieve_orig` (the oracle), each a self-contained module exposing the v0-style API (`new`, `len`, `capacity`, `contains_key`, `get(&mut)`, `insert -> Option<(K,V)>`). Used by `research/benches/micro.rs` and the `research/src/bin/bench*` harnesses for comparison.
- `research/src/experimental/mod.rs` defines `pub trait CacheImpl<K, V>` — the cross-variant interface implemented by every variant. Re-exported at `senba_research::CacheImpl`. The `impl CacheImpl for senba::Cache` adapter lives in `research/src/lib.rs`.
- `research/src/experimental/sieve_orig.rs` — **faithful port of the NSDI'24 author reference** (`external/NSDI24-SIEVE/.../Sieve.c`). Doubly-linked list + single hand + per-entry visited bit, in safe Rust via an arena. **Treat this as the spec / oracle** — every variant's hit/miss behavior on any trace must match `sieve_orig` exactly. Oracle cross-checks for the senba library `Cache` live in `research/tests/oracle_cache_match.rs`; oracle cross-checks across the experimental variants live in `research/tests/oracle.rs`.
- `research/src/workload/` — Zipf generator + trace replay utilities. Used by the perf-gate, micro-bench, and oracle test.
- `research/src/bin/bench.rs`, `research/src/bin/bench_concurrent.rs` — research drivers comparing senba's `Cache`, the experimental variants, `mini-moka`, and `moka`. The `mini-moka` / `moka` / `parking_lot` / `rand` / `rand_distr` deps live on `senba-research` (not on `senba`).
- `research/src/bin/bench_vtune.rs` — self-contained Windows / VTune profiling driver (senba vs orig only, in-process Zipf, no third-party caches, no trace files). Drives VTune collection via the Intel ITT API (`ittapi::pause` / `resume`), so the measurement window is bracketed automatically — no manual Resume/Enter coordination. Cross-buildable to `x86_64-pc-windows-msvc` via `cargo xwin`; build steps and rationale are in the file's module docstring. The `zstd` dep is gated behind the `external-traces` feature so this build path stays free of Windows-specific C toolchain needs (only `ittapi`'s small C source is built via `clang-cl`).
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

1. Add `research/src/experimental/sieve_<name>.rs` with the same public API as `sieve_orig`.
2. Register it in `research/src/experimental/mod.rs`.
3. Mirror the test names in `sieve_orig` so equivalent behavior is checked side-by-side.
4. The bar for correctness is "produces the same evicted-key sequence as `sieve_orig` on the same input trace" — not just "passes the unit tests". Add a corresponding entry under `research/tests/oracle.rs`.
