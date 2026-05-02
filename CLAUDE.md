# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo check          # type-check without producing artifacts
cargo build          # build the library
cargo test           # run all tests
cargo test <name>    # run a single test by name
cargo clippy         # lint
```

## Architecture

`senba-cache` is a Rust library crate providing a generic cache abstraction.

- `src/cache.rs` — `Cache<K, V>` trait: the core interface all implementations must satisfy
- `src/error.rs` — `Error` enum and `Result<T>` alias used throughout the crate
- `src/lib.rs` — re-exports `Cache`, `Error`, and `Result` at the crate root

New cache implementations should live as separate modules under `src/` and implement `Cache<K, V>` from `cache.rs`.
