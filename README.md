# senba-cache

A Rust sandbox for exploring good implementations of the **SIEVE** cache eviction algorithm (NSDI'24, Zhang et al.).

The goal is not to ship a single "best" cache, but to develop several SIEVE variants in parallel — starting from a faithful port of the authors' reference C code, then iterating toward Rust-idiomatic and performance-tuned designs — and to study the trade-offs between them.

- Paper: <https://yazhuozhang.com/assets/publication/nsdi24-sieve.pdf>
- Authors' reference repo: <https://github.com/cacheMon/NSDI24-SIEVE> (included as a git submodule at `external/NSDI24-SIEVE/`)

## Variants

| Module | Role | Internal data structure |
|---|---|---|
| `src/sieve_orig.rs` | **Reference port.** A line-by-line Rust port of `Sieve.c` from libCacheSim. Treated as the spec / oracle — every other variant must reproduce its eviction behavior. | Arena (`Vec<Option<Node>>`) + `Option<NodeId>` doubly-linked list + single `hand` pointer + per-entry `freq` (visited bit). |
| `src/sieve_v0.rs` | First experimental variant. Replaces the linked list with a contiguous logical queue + tombstones + periodic compaction. | `Vec<Option<EntryId>>` queue + `BitSet` for `visited`/`tombstone` + periodic `compact()`. |

All SIEVE modules expose the same minimal API so they can be swapped in benchmarks:

```rust
let mut cache = SieveCache::new(capacity);
cache.insert(key, value);            // -> Option<(K, V)>  (returns evicted on overflow)
cache.get(&key);                     // -> Option<&V>      (sets visited bit on hit)
cache.contains_key(&key);
cache.len(); cache.capacity();
// sieve_orig also: cache.remove(&key) -> Option<V>
```

## Build / test

```bash
cargo test                 # all unit tests
cargo test sieve_orig      # just the reference port
cargo test sieve_v0        # just the v0 variant
cargo clippy
```

## Project layout

```
src/
  cache.rs        # Cache<K,V> trait (placeholder; not yet a fit for SIEVE — see CLAUDE.md)
  error.rs        # Error / Result
  lib.rs          # module declarations and re-exports
  sieve_orig.rs   # faithful NSDI'24 reference port  ← oracle
  sieve_v0.rs     # tombstone + compaction variant
external/
  NSDI24-SIEVE/   # git submodule: authors' libCacheSim repo (read-only reference)
```

## Reference

```bibtex
@inproceedings{zhang2024-sieve,
  title={SIEVE is Simpler than LRU: an Efficient Turn-Key Eviction Algorithm for Web Caches},
  author={Zhang, Yazhuo and Yang, Juncheng and Yue, Yao and Vigfusson, Ymir and Rashmi, K.V.},
  booktitle={USENIX Symposium on Networked Systems Design and Implementation (NSDI'24)},
  year={2024}
}
```
