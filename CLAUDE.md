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

## Architecture

Layout:

- `src/cache.rs` — `Cache<K, V>` trait (placeholder). Note: SIEVE's `get` semantically needs `&mut self` (it sets the visited bit), so the current trait signature is **not** yet a good fit and the SIEVE modules below do not implement it. Aligning the trait with SIEVE is future work.
- `src/error.rs` — `Error` enum and `Result<T>` alias.
- `src/lib.rs` — re-exports `Cache`, `Error`, `Result` and declares the SIEVE modules.

SIEVE implementations (each is a self-contained module under `src/`):

- `src/sieve_orig.rs` — **faithful port of the NSDI'24 author reference** (`external/NSDI24-SIEVE/libCacheSim/libCacheSim/cache/eviction/Sieve.c`). Doubly-linked list (head=newest, tail=oldest) + single hand pointer + per-entry `freq` visited bit. Implemented in safe Rust via an arena (`Vec<MaybeUninit<Node>>` + `free_list`) with `NodeId = u32` prev/next (`NIL = u32::MAX` sentinel). **Treat this as the spec / oracle** — when adding a new variant, its hit/miss behavior on any trace must match `sieve_orig` exactly.
- `src/sieve_v0.rs` — first experimental variant. Single contiguous `Vec` "logical queue" with tombstone marking + periodic compaction, instead of a linked list. Same external API as `sieve_orig` for direct comparison.

Both SIEVE modules expose the same v0-style API (`new`, `len`, `capacity`, `contains_key`, `get(&mut)`, `insert -> Option<(K,V)>`, plus `remove` on `sieve_orig`) so they can be benchmarked / property-tested against each other with the same harness.

## Adding a new SIEVE variant

1. Add `src/sieve_<name>.rs` with the same public API as `sieve_orig`.
2. Register it in `src/lib.rs` next to the existing modules.
3. Mirror the test names in `sieve_orig` so equivalent behavior is checked side-by-side.
4. The bar for correctness is "produces the same evicted-key sequence as `sieve_orig` on the same input trace" — not just "passes the unit tests".
