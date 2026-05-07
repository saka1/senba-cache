# senba-cache
senba-cache is a small, fast, single-threaded in-memory cache.
Compared to well-known alternatives like moka and lru-cache, it has interesting characteristics:

- **High hit ratio**: Uses a SIEVE-like eviction policy — a sharded variant of SIEVE (NSDI'24, Zhang et al.) keyed by the upper bits of the hash. On web-style workloads, hit ratio is comparable to LRU and W-TinyLFU.
- **Low, predictable overhead**: Values are stored directly in a fixed-stride arena, and shards are scanned in parallel using SIMD, so both lookups and inserts stay cheap. The cache does not use the doubly-linked list and separate hash table from the original SIEVE paper.

The crate is single-threaded: every mutating operation takes `&mut self`.
Wrap in `Mutex<Cache>` / `RwLock<Cache>` if you need concurrent access.

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

## API at a glance

Full signatures and semantics live in the rustdoc; this is just the map.

- **Lookup** — `get`, `get_mut`, `get_key_value`, `peek`, `peek_mut`,
  `peek_key_value`, `contains_key`. All accept `Borrow<Q>`, so a
  `Cache<String, V>` can be queried by `&str`.
- **Mutation** — `insert`, `remove`, `clear`, `retain`,
  `get_or_insert_with`.
- **Iteration** — `iter`, `iter_mut`, `keys`, `values`, `drain`, plus
  `IntoIterator for &Cache` / `&mut Cache`. Order is unspecified.
- **Traits** — `Clone` (where `K, V, H: Clone`), `Debug` (where
  `K, V: Debug`), `Extend<(K, V)>`, `Extend<(&K, &V)>`.

Two semantic notes worth keeping in mind:

- `insert` returns `Option<(K, V)>` — the entry it evicted on capacity
  overflow, if any.
- `get*` promote on hit (and update hit/miss counters); `peek*`,
  `contains_key`, and iteration are non-promoting.

## Tuning

### Slot size

`Cache` stores each entry in a fixed-size slot. Three sizes are
available — pick the smallest one your `(K, V)` fits in:

| `SlotSize`         | Stride | Typical fit                                         |
| ------------------ | -----: | --------------------------------------------------- |
| `Slot16`           |   16 B | `(u32, u32)`, `(u64, u64)`                          |
| `Slot32` (default) |   32 B | `(String, V_small)`, `(Arc<str>, Arc<str>)`         |
| `Slot64`           |   64 B | `(String, String)`, `(K, V)` up to ~56 B payload    |

If the entry doesn't fit, the crate refuses to compile and the error
message tells you which size to use instead. Just bump it:

```rust
use senba_cache::{Cache, Slot64};

let cache: Cache<String, String, Slot64> = Cache::new(1024);
```

`Slot64` is the largest size supported. If your `(K, V)` doesn't fit
even there, store the value behind an indirection like `Box<V>` or
`Arc<V>` so the slot only has to hold a pointer.

### Custom hasher

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
