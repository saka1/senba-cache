//! Cross-checks `senba::concurrent::Cache` against the NSDI'24 author-
//! reference port [`senba_research::experimental::sieve_orig::SieveCache`]
//! when driven from a single thread. The concurrent cache's `insert`
//! returns `()`, so we can't directly compare evict-order step-by-step
//! against the oracle's `Option<(K, V)>` return — instead we compare the
//! final cache contents (every key in the universe `get`'d on both
//! sides) after replaying the same trace.
//!
//! Run with: `cargo test -p senba-research oracle_concurrent_cache_match`.
//! Gated to `non-miri` only — `senba::concurrent` now compiles on every
//! arch with a runtime AVX2/scalar dispatcher.

#![cfg(not(miri))]

use senba::concurrent::Cache as ConcurrentCache;
use senba_research::experimental::sieve_orig::SieveCache as Orig;

/// Final cache contents after the same Zipf trace must match `sieve_orig`
/// for every cap the per-shard limit (`MAX_PER_SHARD = 64`) lets us run
/// against a single shard. Above that bound the concurrent cache must
/// split into multiple shards, at which point the SIEVE eviction set
/// stops being byte-for-byte oracle-comparable (each shard runs its own
/// hand independently).
#[test]
fn final_contents_match_sieve_orig_single_shard_zipf1() {
    for cap in [16usize, 32, 64] {
        let mut oracle: Orig<u64, u64> = Orig::new(cap);
        let cc: ConcurrentCache<u64, u64> = ConcurrentCache::with_shards(cap, 1);
        for i in 0..20_000u64 {
            let key = (i.wrapping_mul(2654435761)) % (cap as u64 * 4);
            oracle.insert(key, key);
            cc.insert(key, key);
        }
        let universe = cap as u64 * 4;
        for k in 0..universe {
            assert_eq!(
                oracle.get(&k).copied(),
                cc.get(&k),
                "cap={cap} key={k} differs from oracle"
            );
        }
    }
}

/// High-skew variant (small key universe relative to cap): exercises the
/// SIEVE visited-promotion path. A miss-after-set or a falsely-evicted
/// hot key would be caught here.
#[test]
fn final_contents_match_sieve_orig_single_shard_high_skew() {
    for cap in [16usize, 32, 64] {
        let mut oracle: Orig<u64, u64> = Orig::new(cap);
        let cc: ConcurrentCache<u64, u64> = ConcurrentCache::with_shards(cap, 1);
        let universe = (cap as u64) / 2;
        for i in 0..10_000u64 {
            let key = (i.wrapping_mul(2654435761)) % universe.max(1);
            oracle.insert(key, key);
            cc.insert(key, key);
            if i % 3 == 0 {
                let g = (i.wrapping_mul(11400714819323198485)) % universe.max(1);
                let _ = oracle.get(&g);
                let _ = cc.get(&g);
            }
        }
        for k in 0..universe {
            assert_eq!(
                oracle.get(&k).copied(),
                cc.get(&k),
                "cap={cap} key={k} differs from oracle on high-skew trace"
            );
        }
    }
}

/// Interleaved insert + remove must keep the concurrent cache in lock-
/// step with the oracle on the surviving keys. (We don't assert evict
/// order, only final reachability.)
#[test]
fn remove_during_churn_matches_oracle() {
    let cap = 64usize;
    let mut oracle: Orig<u64, u64> = Orig::new(cap);
    let cc: ConcurrentCache<u64, u64> = ConcurrentCache::with_shards(cap, 1);
    for i in 0..3_000u64 {
        let key = (i.wrapping_mul(2654435761)) % 256;
        oracle.insert(key, key);
        cc.insert(key, key);
        if i % 5 == 0 {
            let rk = (i.wrapping_mul(11400714819323198485)) % 256;
            assert_eq!(
                oracle.remove(&rk),
                cc.remove(&rk),
                "remove return value differs at step={i} key={rk}"
            );
        }
    }
    for k in 0..256u64 {
        assert_eq!(
            oracle.get(&k).copied(),
            cc.get(&k),
            "post-churn key={k} differs from oracle"
        );
    }
}
