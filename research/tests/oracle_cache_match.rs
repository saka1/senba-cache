//! Cross-checks senba's library-grade [`senba::Cache`] against the NSDI'24
//! author-reference port [`senba_research::experimental::sieve_orig::SieveCache`]
//! across the trace patterns we care about.
//!
//! These tests lived inside `senba/src/tests/cache.rs` before the workspace
//! split, gated by `#[cfg(feature = "experimental")]`. They survive verbatim
//! here because they only ever touched `senba::Cache`'s public API.

use senba::{Cache, Slot16, Slot32, Slot64};
use senba_research::experimental::sieve_orig::SieveCache as Orig;

/// Cross-checks insert/get behavior against sieve_orig (oracle) with a single shard.
#[test]
fn matches_sieve_orig_externally_1shard() {
    let cap = 64usize;
    let mut a: Orig<u64, u64> = Orig::new(cap);
    let mut b: Cache<u64, u64, Slot32> = Cache::with_shards(cap, 1);
    for k in 0..10_000u64 {
        let key = (k.wrapping_mul(2654435761)) % 256;
        let _ = a.insert(key, key);
        let _ = b.insert(key, key);
    }
    for k in 0..256u64 {
        assert_eq!(
            a.get(&k).copied(),
            b.get(&k).copied(),
            "1-shard mismatch with sieve_orig at key {k}"
        );
    }
}

/// All three brackets (Slot16/32/64) must match sieve_orig semantics for Entry<u64,u64>.
#[test]
fn matches_sieve_orig_per_slot() {
    let cap = 32usize;
    let mut oracle: Orig<u64, u64> = Orig::new(cap);
    let mut s16: Cache<u64, u64, Slot16> = Cache::with_shards(cap, 1);
    let mut s32: Cache<u64, u64, Slot32> = Cache::with_shards(cap, 1);
    let mut s64: Cache<u64, u64, Slot64> = Cache::with_shards(cap, 1);
    for k in 0..5_000u64 {
        let key = (k.wrapping_mul(2654435761)) % 128;
        oracle.insert(key, key);
        s16.insert(key, key);
        s32.insert(key, key);
        s64.insert(key, key);
    }
    for k in 0..128u64 {
        let g = oracle.get(&k).copied();
        assert_eq!(s16.get(&k).copied(), g, "Slot16 mismatch key={k}");
        assert_eq!(s32.get(&k).copied(), g, "Slot32 mismatch key={k}");
        assert_eq!(s64.get(&k).copied(), g, "Slot64 mismatch key={k}");
    }
}

/// Cross-checks remove behavior against sieve_orig with interleaved operations.
#[test]
fn remove_during_churn_oracle_match() {
    let cap = 32usize;
    let mut a: Orig<u64, u64> = Orig::new(cap);
    let mut b: Cache<u64, u64, Slot32> = Cache::with_shards(cap, 1);
    for k in 0..3_000u64 {
        let key = (k.wrapping_mul(2654435761)) % 128;
        let ai = a.insert(key, key);
        let bi = b.insert(key, key);
        assert_eq!(ai, bi, "insert eviction mismatch step={k} key={key}");
        if k % 5 == 0 {
            let rk = (k.wrapping_mul(11400714819323198485)) % 128;
            let ar = a.remove(&rk);
            let br = b.remove(&rk);
            assert_eq!(ar, br, "remove mismatch step={k} key={rk}");
        }
    }
    for k in 0..128u64 {
        assert_eq!(
            a.get(&k).copied(),
            b.get(&k).copied(),
            "oracle mismatch key={k}"
        );
    }
}

/// `retain` must leave the cache in a state byte-equivalent (under SIEVE
/// semantics) to a sequence of `remove` calls on the same victims, so future
/// eviction order matches the oracle.
#[test]
fn retain_matches_remove_loop_for_eviction_order() {
    let cap = 32usize;

    let mut a: Orig<u64, u64> = Orig::new(cap);
    let mut b: Cache<u64, u64, Slot32> = Cache::with_shards(cap, 1);
    for k in 0..200u64 {
        let key = (k.wrapping_mul(2654435761)) % 64;
        a.insert(key, key);
        b.insert(key, key);
    }

    let victims: Vec<u64> = (0..64u64).filter(|k| k % 2 == 0).collect();
    for k in &victims {
        let _ = a.remove(k);
    }
    b.retain(|k, _| k % 2 != 0);

    for k in 200..400u64 {
        let key = (k.wrapping_mul(2654435761)) % 64;
        let ai = a.insert(key, key);
        let bi = b.insert(key, key);
        assert_eq!(ai, bi, "post-retain eviction mismatch step={k} key={key}");
    }
    for k in 0..64u64 {
        assert_eq!(
            a.get(&k).copied(),
            b.get(&k).copied(),
            "post-retain final state mismatch key={k}"
        );
    }
}

/// Regression test for the `find_avx2` `limit = round_up(tail, LANE)` change.
/// At low fill ratios `tail << tags.len()`, so the SIMD path's bound diverges
/// from the scalar path. Both must still agree with `sieve_orig` on every key,
/// covering misses (no key in tags) and hits (key present at small `tail`).
#[test]
fn find_avx2_low_fill_matches_oracle() {
    let cap = 64usize;
    let mut a: Orig<u64, u64> = Orig::new(cap);
    let mut b: Cache<u64, u64, Slot32> = Cache::with_shards(cap, 1);
    for k in 0..8u64 {
        a.insert(k, k * 7);
        b.insert(k, k * 7);
    }
    for k in 0..256u64 {
        assert_eq!(
            a.get(&k).copied(),
            b.get(&k).copied(),
            "low-fill SIMD bound regression at key {k}"
        );
    }
}
