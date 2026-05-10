# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project goal

`senba-cache` is a sandbox for studying Rust implementations of the SIEVE eviction algorithm (NSDI'24, Zhang et al.) — building multiple variants, comparing trade-offs (correctness, allocations, cache locality, ergonomics), and **harvesting the winner into a publishable library**. The `senba` crate (crates.io target, workspace root) is the stable, perf-gated surface; experimental variants stay as research artifacts in `senba-research`. Decisions touching the library surface (API, semantics, perf contract) are made with that downstream use in mind, not just sandbox convenience.

NSDI'24 paper: https://yazhuozhang.com/assets/publication/nsdi24-sieve.pdf — the author reference C lives at `external/NSDI24-SIEVE/` (git submodule).

## Workspace layout

| Member | Path | Publish? | Purpose |
| --- | --- | --- | --- |
| `senba` | `./` | yes (crates.io) | Library surface: `Cache`, `Drain`, `Stats`, `SlotSize` (`Slot16/32/64`), `hash::Xxh3Build` |
| `senba-research` | `research/` | `publish = false` | Experimental variants, Zipf + trace replay, `bin/bench*` drivers, oracle tests, micro-bench |

The `senba` package's crates.io payload is allowlisted in `Cargo.toml`'s `package.include` — necessary because the package directory is the workspace root, so without an explicit list `research/` / `external/` / `docs/` would all be pulled in. `senba::Cache` implements `senba_research::CacheImpl` via the orphan rule (the trait is local to `senba-research`), letting cross-variant drivers and the oracle test treat the library `Cache` and the experimental variants symmetrically.

## Commands

```bash
cargo check  -p senba                  # publishable surface only — fast inner loop
cargo test   -p senba
cargo clippy -p senba -- -D warnings

cargo test   --workspace               # full surface; required before commit
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt    --all
```

Oracle tests in `research/tests/oracle.rs` (`*_matches_orig_on_bundled_zipf`) read traces from the `external/NSDI24-SIEVE` submodule and are gated behind the default-off `external-traces` feature on `senba-research`, so `cargo test --workspace` works without submodule init. To run them: `cargo test -p senba-research --features external-traces`. Gate any new test that reads files under `external/` the same way (`#[cfg(feature = "external-traces")]` on the test fn and on any imports that become unused without the feature).

## Quality Gates

After any code change: `cargo fmt --all` → `cargo clippy --workspace --all-targets -- -D warnings` → `cargo test --workspace` must all pass. For senba public-API edits the `-p senba` triple is a fine fast inner loop, but always re-run the workspace gate before commit.

### Performance regression check (`research/benches/sieve_cache_perf.rs`)

Run when senba library files (`src/lib.rs`, `src/shard/`, `src/iter.rs`, `src/slot.rs`, `src/stats.rs`, `src/hash.rs`) change in ways that could plausibly reach the compiled `Cache` hot path (layout, dispatch, new branches in `find` / `insert` / `evict_one_returning_id`):

```bash
cargo bench -p senba-research --bench sieve_cache_perf -- --save-baseline before
# apply change
cargo bench -p senba-research --bench sieve_cache_perf -- --baseline before
```

The bench has 8 scenarios covering `SlotSize` × op-mix × skew × value-type (full list in the bench file header). Treat **>5% regression on any scenario** as a commit-blocker. If the perf-gate and a Twitter cross-check (`research/src/bin/bench --source twitter`) disagree, the regression is likely scenario-specific layout noise — see `docs/reports/2026-05-07-aligned-tags-load.md` for an example.

Skipping is fine for changes that demonstrably cannot reach the compiled hot path (doc / test / clippy fixes). The perf-gate is the **stable contract for `senba::Cache`** and is intentionally separated from `research/benches/micro.rs` (the experimental playground, freely rewritten as variants come and go); editing the perf-gate invalidates prior saved baselines.

## Plot / analysis scripts (Python)

`scripts/` is a separate uv project (`pyproject.toml` + `uv.lock` + `.python-version`). Run from anywhere in the repo:

```bash
uv run --project scripts python scripts/<name>.py
```

## Conventions

- Source code (identifiers, comments, doc comments) in **English**. Reports and other docs have no language requirement.
- Assembly in code, comments, or reports uses **Intel syntax** (`mov rax, [rdi]`), not AT&T.

## Architecture

### `senba` (publishable)

`Cargo.toml`'s `package.include` allowlists the crates.io payload to:

- `src/lib.rs` — public types (`Cache`, `Drain`, `Stats`, `SlotSize`, `Slot16/32/64`) + sibling-module re-exports
- `src/shard/` — per-shard SIEVE state machine, SIMD `find`, evict / insert / remove (split into `mod` / `scan` / `state` / `lookup`)
- `src/iter.rs` — `Iter` / `IterMut` / `Keys` / `Values` / `Drain`
- `src/slot.rs` — `SlotSize` sealed trait
- `src/stats.rs` — `Stats`
- `src/hash.rs` — `Xxh3Build` (default `H` on `Cache`)
- `src/tests/` — unit tests, split by topic
- `research/benches/sieve_cache_perf.rs` — perf-gate (lives in `senba-research` but the contract it gates is `senba::Cache`)

### `senba-research` (non-publishable)

- `research/src/experimental/` — SIEVE variants and oracle. Naming series: `sieve_v0..v3` (linked-list → array trials), `sieve_j3..j8` (Map 廃止 → set-associative → tag u16 → final j8), `sieve_c8` / `c14s` / `c16s` (concurrent line), plus `sieve_orig`. Each module exposes the v0-style API (`new`, `len`, `capacity`, `contains_key`, `get(&mut)`, `insert -> Option<(K,V)>`).
- `research/src/experimental/mod.rs` defines `pub trait CacheImpl<K, V>`. The `impl CacheImpl for senba::Cache` adapter lives in `research/src/lib.rs` (orphan-rule bridge).
- `research/src/experimental/sieve_orig.rs` — **faithful port of the NSDI'24 author reference** (`external/NSDI24-SIEVE/.../Sieve.c`), in safe Rust via an arena. **Treat as spec / oracle**: every variant must match `sieve_orig`'s hit/miss + eviction sequence on every trace. Cross-checks: `tests/oracle_cache_match.rs` (for `senba::Cache`), `tests/oracle.rs` (across experimental variants).
- `research/src/workload/` — Zipf + trace replay (used by perf-gate, micro-bench, oracle tests).
- `research/src/bin/bench.rs`, `bench_concurrent.rs` — research drivers comparing `senba::Cache`, the experimental variants, `mini-moka`, and `moka`. `mini-moka` / `moka` / `parking_lot` / `rand` / `rand_distr` deps live on `senba-research`, not on `senba`.
- `research/src/bin/bench_vtune.rs`, `bench_vtune_concurrent.rs` — self-contained Windows / VTune profiling drivers (senba internals only, no third-party caches). Intel ITT API (`ittapi`) brackets the measurement window automatically; cross-buildable to MSVC ABI via `cargo xwin`. Build steps and rationale live in each file's module docstring.
- `docs/reports/` — experiment write-ups (see "Documenting results" below). The code is the artifact; the reports are the conclusions.

## Documenting results

Research evaporates between sessions, so whenever a meaningful experiment concludes — a benchmark run, a profiling session, an overhead analysis, a design comparison — write a report under `docs/reports/YYYY-MM-DD-<topic>.md` before moving on. A report is warranted when a benchmark produces numbers worth keeping (including null / negative results), when an investigation reaches a conclusion (root cause / quantified overhead / resolved trade-off), or when a design decision becomes a basis for future work. **Reports are this project's primary output** — undocumented benches are effectively lost.

**Structure (hypothesis → action → result).** Reports follow the plain engineering loop: state the hypothesis or motivation, describe what was done, present what was learned. Skip background sections, related-work surveys, and other padding — the reader knows the project. Surprising findings, refuted hypotheses, and follow-ups belong in the body; everything else is noise.

**Index (`docs/reports/index.md`).** One paragraph per report, 3–5 lines, in the form *"Hypothesis: X. Did Y. Found Z."* Keep at most 1–2 of the most striking numbers — secondary numbers, refutations, scope notes, and figures stay in the linked report. If an entry exceeds 5 lines, suspect over-summarising (it should be a digest, not a recap) or over-splitting (one experiment may not warrant its own report). Read the index first to orient yourself without opening individual files; whenever you add or substantially revise a report, update its index entry.

## Adding a new SIEVE variant

1. Add `research/src/experimental/sieve_<name>.rs` exposing the v0-style API.
2. Register it in `research/src/experimental/mod.rs`.
3. Mirror the test names from `sieve_orig` so equivalent behavior is checked side-by-side.
4. **Correctness bar: produces the same evicted-key sequence as `sieve_orig` on the same input trace** — passing unit tests is not enough. Add a corresponding entry under `research/tests/oracle.rs`.
