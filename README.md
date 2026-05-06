# senba-cache

A Rust cache library implementing the **SIEVE** eviction algorithm
(NSDI'24, Zhang et al.). SIEVE is a single-hand FIFO sweep with a
per-entry visited bit; on web-style workloads it matches or exceeds
LRU and W-TinyLFU on hit ratio while keeping the per-op work small.

`senba-cache` ships a sharded, SIMD-accelerated implementation with a
`HashMap`-style API. Each shard caps at 64 entries so a 6-bit id packs
inline with the hash bits in a single tag word, which is what the
AVX2 (BMI1) `find` path scans 16 lanes at a time. Storage is a fixed
stride arena (16 / 32 / 64 byte slot brackets) selected at the type
level; choice of stride is part of the public API.

The crate is single-threaded: every mutating operation takes
`&mut self`. Wrap in `Mutex<Cache>` / `RwLock<Cache>` if you need
concurrent access.

## Quick start

```toml
[dependencies]
senba-cache = "0.1"
```

```rust
use senba_cache::Cache;

let mut cache: Cache<u64, String> = Cache::new(1024);
cache.insert(1, "hello".into());
cache.insert(2, "world".into());

assert_eq!(cache.get(&1), Some(&"hello".to_string()));

// `insert` returns the entry it evicted on overflow, if any.
for k in 3..=2048 {
    let _evicted = cache.insert(k, format!("v{k}"));
}
```

## Choosing a `SlotSize`

The entries arena uses a fixed stride per slot so the SIMD `find` path
can compute byte offsets via a single shift. Pick the smallest bracket
that fits `Entry<K, V>`; if the entry is too large the crate refuses to
compile.

| `SlotSize`         | Stride | Typical fit                                         |
| ------------------ | -----: | --------------------------------------------------- |
| `Slot16`           |   16 B | `(u32, u32)`, `(u64, u64)`                          |
| `Slot32` (default) |   32 B | `(String, V_small)`, `(Arc<str>, Arc<str>)`         |
| `Slot64`           |   64 B | `(String, String)`, `(K, V)` up to ~56 B payload    |

```rust
use senba_cache::{Cache, Slot64};

let cache: Cache<String, String, Slot64> = Cache::new(1024);
```

`sizeof(Entry<K, V>) > S::SIZE` is rejected at compile time with a
const-eval message pointing at the next bracket up.

## Custom hasher

The default hasher is xxh3 (`senba_cache::hash::Xxh3Build`). To plug in
your own `BuildHasher`:

```rust
use senba_cache::Cache;
use std::collections::hash_map::RandomState;

let cache: Cache<u64, u64, _, RandomState> =
    Cache::with_hasher(1024, RandomState::new());
```

## Observability

`Cache::stats()` returns lifetime counters aggregated across shards.
Promoting lookups (`get`, `get_mut`, `get_key_value`, the lookup half of
`get_or_insert_with`) update `hits` / `misses`; `peek*` and
`contains_key` are non-promoting and not counted.

```rust
let s = cache.stats();
println!(
    "hits={} misses={} insertions={} evictions={}",
    s.hits, s.misses, s.insertions, s.evictions,
);
```

`evictions` counts only capacity-driven evictions inside `insert`;
explicit removal (`remove`, `clear`, `retain`, `drain`) is not counted.

## API surface

- Lookups: `get`, `get_mut`, `peek`, `peek_mut`, `get_key_value`,
  `peek_key_value`, `contains_key`. All accept `Borrow<Q>` so a
  `Cache<String, V>` can be queried by `&str`.
- Mutation: `insert -> Option<(K, V)>` (returns the evicted pair on
  overflow), `remove`, `clear`, `retain`, `get_or_insert_with`.
- Iteration: `iter`, `iter_mut`, `keys`, `values`, `drain`,
  `IntoIterator for &Cache` / `&mut Cache`. Iteration is non-promoting
  and order is unspecified.
- Trait impls: `Clone` (where `K, V, H: Clone`), `Debug` (where
  `K, V: Debug`), `Extend<(K, V)>` and `Extend<(&K, &V)>`.

## Sharding

The shard count is the smallest power of two `N` such that
`ceil(capacity / N) <= 64`, chosen automatically by `Cache::new`. To
override (mainly for testing / oracle comparison) use
`Cache::with_shards(capacity, n)` with `n` a power of two satisfying
the same per-shard bound. Sharding is purely for keeping per-shard
SIEVE state inside the SIMD scan window — it does not enable
concurrent access.

## Benchmarks

A perf-regression gate (`benches/sieve_cache_perf.rs`) is included for
contributors who want to verify their changes don't hurt the hot path.
See [`docs/benchmarking.md`](docs/benchmarking.md) for the workflow,
plus notes on the research microbench and profiling setup.

## Reference

```bibtex
@inproceedings{zhang2024-sieve,
  title={SIEVE is Simpler than LRU: an Efficient Turn-Key Eviction Algorithm for Web Caches},
  author={Zhang, Yazhuo and Yang, Juncheng and Yue, Yao and Vigfusson, Ymir and Rashmi, K.V.},
  booktitle={USENIX Symposium on Networked Systems Design and Implementation (NSDI'24)},
  year={2024}
}
```

Paper: <https://yazhuozhang.com/assets/publication/nsdi24-sieve.pdf>
