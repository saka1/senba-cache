//! sieve_orig (NSDI'24 著者参照ポート) を oracle として、
//! 他の variant が同じトレースで同じ evict 列を出すことを検証する差分テスト。

use senba_cache::workload::file;
use senba_cache::workload::zipf::ZipfGen;
use senba_cache::{Cache, sieve_orig, sieve_v0};

fn run<C: Cache<u64, u64>>(
    trace: impl Iterator<Item = u64>,
    cap: usize,
) -> Vec<Option<(u64, u64)>> {
    let mut c = C::new(cap);
    trace.map(|k| c.insert(k, k)).collect()
}

/// 完全一致を検証し、不一致なら最初の divergence を分かりやすく報告して panic。
/// `assert_eq!` で 100k 要素の Vec を dump するとログが破裂するので。
fn assert_eviction_streams_eq(
    orig: &[Option<(u64, u64)>],
    other: &[Option<(u64, u64)>],
    label: &str,
) {
    assert_eq!(
        orig.len(),
        other.len(),
        "[{label}] length mismatch: orig={} other={}",
        orig.len(),
        other.len()
    );
    if let Some((i, (o, x))) = orig
        .iter()
        .zip(other.iter())
        .enumerate()
        .find(|(_, (o, x))| o != x)
    {
        let ctx_lo = i.saturating_sub(3);
        let ctx_hi = (i + 4).min(orig.len());
        panic!(
            "[{label}] first divergence at index {i}\n  orig[{i}] = {o:?}\n  other[{i}] = {x:?}\n  context orig[{ctx_lo}..{ctx_hi}]  = {:?}\n  context other[{ctx_lo}..{ctx_hi}] = {:?}",
            &orig[ctx_lo..ctx_hi],
            &other[ctx_lo..ctx_hi]
        );
    }
}

#[test]
fn v0_matches_orig_on_synthetic_zipf() {
    for &(skew, cap) in &[
        (1.05_f64, 64usize),
        (1.1, 128),
        (1.2, 256),
        (1.5, 1024),
    ] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let v0 = run::<sieve_v0::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&orig, &v0, &format!("zipf skew={skew} cap={cap}"));
    }
}

#[test]
fn v0_matches_orig_on_bundled_zipf() {
    let path = "external/NSDI24-SIEVE/mydata/zipf/zipf_1.0";
    for &cap in &[256usize, 1024, 4096] {
        let trace_a = file::from_path(path).unwrap().take(100_000);
        let trace_b = file::from_path(path).unwrap().take(100_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let v0 = run::<sieve_v0::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&orig, &v0, &format!("bundled cap={cap}"));
    }
}
