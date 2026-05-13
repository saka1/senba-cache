# Changelog

All notable changes to this project will be documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] — Unreleased

### Added

- `senba::concurrent::Cache` — sharded, lock-free-reader concurrent SIEVE
  cache promoted from the `c17s` research variant. AVX2 SIMD tag scan +
  entry-version seqlock on the reader path; per-shard
  `parking_lot::Mutex` only for Path B/C writers. Internally each
  entry's value is held behind `Arc<V>`, with writer-side drops deferred
  through `crossbeam-epoch` so `V: Clone + Send + Sync + 'static` is
  sound — matching `moka::sync::Cache<K, V>`. Available on
  `x86_64 + AVX2` only via the `concurrent` Cargo feature.
- Auto-shard heuristic for `Cache::new`:
  `next_pow2(cap/8)` clamped by `MIN_PER_SHARD = 4` and
  `MAX_PER_SHARD = 64` — chosen by the 56 (workload × T) cell sweep in
  `docs/reports/2026-05-13-c17s-shard-heuristic.md`.

### Removed (breaking)

- `senba::concurrent::PartitionedCache` is gone. The
  `senba::concurrent::Cache` introduced above pareto-dominates the
  partitioned design on every HR-preserving cell of the sweep, so the
  partitioned baseline is retired in this release rather than carried
  as a deprecated alias. Constructors map cleanly:
  `PartitionedCache::new(cap, parts)` →
  `Cache::with_shards(cap, parts)`.
- `bench_concurrent` and `bench_vtune_concurrent`'s `--variant
  partitioned` arms are likewise removed; the new
  `--variant senba_concurrent` covers the same surface.
  `docs/benchmark/partitioned-sweep/run.sh` and
  `docs/benchmark/partitioned-cap1024-sweep/run.sh` are renamed to
  `.archived` (their `data/` and `figures/` directories stay as
  research history).

### Dependencies

- `concurrent` feature now pulls in `crossbeam-epoch ^0.9` in addition
  to `parking_lot ^0.12`. Both are MIT/Apache-licensed and widely used
  in the Rust ecosystem (tokio, dashmap, flurry).

## [0.2.0] — 2026-04-21

Initial public release. `senba::Cache` (single-threaded SIEVE) +
experimental `senba::concurrent::PartitionedCache` (now removed in
0.3.0).
