//! sieve_orig (NSDI'24 著者参照ポート) を oracle として、
//! 他の variant が同じトレースで同じ evict 列を出すことを検証する差分テスト。

use senba_cache::workload::file;
use senba_cache::workload::zipf::ZipfGen;
use senba_cache::{CacheImpl, sieve_j3, sieve_j8, sieve_orig, sieve_v0, sieve_v1, sieve_v2, sieve_v3};

fn run<C: CacheImpl<u64, u64>>(
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

/// 最小再現: cap=3, トレース [1,2,3,1,2,4,5]。
/// insert(4) で victim=3 (= 当時の tail-1, つまり最新エントリ) を evict したあと、
/// v0 は hand=3 のまま insert(4) で qpos=3 に新エントリを置くため、hand が
/// 新エントリ "4" を指してしまう。次の insert(5) で v0 は 4 を即 evict する一方、
/// orig は hand=None → tail から再開し 1 を evict する。
#[test]
fn v0_diverges_when_victim_is_newest_entry() {
    let trace: Vec<u64> = vec![1, 2, 3, 1, 2, 4, 5];
    let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace.iter().copied(), 3);
    let v0 = run::<sieve_v0::SieveCache<u64, u64>>(trace.iter().copied(), 3);
    assert_eviction_streams_eq(&orig, &v0, "minimal repro");
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

// v1 (word-scan eviction) は v0 と同じレイアウトを取るはずなので、
// v0 が落ちた最小再現も同じ形で落ちる。
#[test]
fn v1_diverges_when_victim_is_newest_entry() {
    let trace: Vec<u64> = vec![1, 2, 3, 1, 2, 4, 5];
    let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace.iter().copied(), 3);
    let v1 = run::<sieve_v1::SieveCache<u64, u64>>(trace.iter().copied(), 3);
    assert_eviction_streams_eq(&orig, &v1, "minimal repro (v1)");
}

#[test]
fn v1_matches_v0_on_synthetic_zipf() {
    for &(skew, cap) in &[
        (1.05_f64, 64usize),
        (1.1, 128),
        (1.2, 256),
        (1.5, 1024),
    ] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let v0 = run::<sieve_v0::SieveCache<u64, u64>>(trace_a, cap);
        let v1 = run::<sieve_v1::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&v0, &v1, &format!("v1 vs v0 zipf skew={skew} cap={cap}"));
    }
}

#[test]
fn v1_matches_orig_on_synthetic_zipf() {
    for &(skew, cap) in &[
        (1.05_f64, 64usize),
        (1.1, 128),
        (1.2, 256),
        (1.5, 1024),
    ] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let v1 = run::<sieve_v1::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&orig, &v1, &format!("v1 vs orig zipf skew={skew} cap={cap}"));
    }
}

#[test]
fn v1_matches_orig_on_bundled_zipf() {
    let path = "external/NSDI24-SIEVE/mydata/zipf/zipf_1.0";
    for &cap in &[256usize, 1024, 4096] {
        let trace_a = file::from_path(path).unwrap().take(100_000);
        let trace_b = file::from_path(path).unwrap().take(100_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let v1 = run::<sieve_v1::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&orig, &v1, &format!("v1 bundled cap={cap}"));
    }
}

// v2 (= v0 + order の Option 剥がし) も同じ minimal divergence を踏むはず。
#[test]
fn v2_diverges_when_victim_is_newest_entry() {
    let trace: Vec<u64> = vec![1, 2, 3, 1, 2, 4, 5];
    let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace.iter().copied(), 3);
    let v2 = run::<sieve_v2::SieveCache<u64, u64>>(trace.iter().copied(), 3);
    assert_eviction_streams_eq(&orig, &v2, "minimal repro (v2)");
}

#[test]
fn v2_matches_v0_on_synthetic_zipf() {
    for &(skew, cap) in &[
        (1.05_f64, 64usize),
        (1.1, 128),
        (1.2, 256),
        (1.5, 1024),
    ] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let v0 = run::<sieve_v0::SieveCache<u64, u64>>(trace_a, cap);
        let v2 = run::<sieve_v2::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&v0, &v2, &format!("v2 vs v0 zipf skew={skew} cap={cap}"));
    }
}

#[test]
fn v2_matches_orig_on_synthetic_zipf() {
    for &(skew, cap) in &[
        (1.05_f64, 64usize),
        (1.1, 128),
        (1.2, 256),
        (1.5, 1024),
    ] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let v2 = run::<sieve_v2::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&orig, &v2, &format!("v2 vs orig zipf skew={skew} cap={cap}"));
    }
}

#[test]
fn v2_matches_orig_on_bundled_zipf() {
    let path = "external/NSDI24-SIEVE/mydata/zipf/zipf_1.0";
    for &cap in &[256usize, 1024, 4096] {
        let trace_a = file::from_path(path).unwrap().take(100_000);
        let trace_b = file::from_path(path).unwrap().take(100_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let v2 = run::<sieve_v2::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&orig, &v2, &format!("v2 bundled cap={cap}"));
    }
}

// v3 (= v1 bit-parallel + v2 Option 剥がし + 2-pass evict) も v1/v2 と同様に
// minimal divergence を踏み、oracle (orig) と比べると同じ位置で別れる。
#[test]
fn v3_diverges_when_victim_is_newest_entry() {
    let trace: Vec<u64> = vec![1, 2, 3, 1, 2, 4, 5];
    let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace.iter().copied(), 3);
    let v3 = run::<sieve_v3::SieveCache<u64, u64>>(trace.iter().copied(), 3);
    assert_eviction_streams_eq(&orig, &v3, "minimal repro (v3)");
}

#[test]
fn v3_matches_v1_on_synthetic_zipf() {
    for &(skew, cap) in &[
        (1.05_f64, 64usize),
        (1.1, 128),
        (1.2, 256),
        (1.5, 1024),
    ] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let v1 = run::<sieve_v1::SieveCache<u64, u64>>(trace_a, cap);
        let v3 = run::<sieve_v3::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&v1, &v3, &format!("v3 vs v1 zipf skew={skew} cap={cap}"));
    }
}

#[test]
fn v3_matches_orig_on_synthetic_zipf() {
    for &(skew, cap) in &[
        (1.05_f64, 64usize),
        (1.1, 128),
        (1.2, 256),
        (1.5, 1024),
    ] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let v3 = run::<sieve_v3::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&orig, &v3, &format!("v3 vs orig zipf skew={skew} cap={cap}"));
    }
}

#[test]
fn v3_matches_orig_on_bundled_zipf() {
    let path = "external/NSDI24-SIEVE/mydata/zipf/zipf_1.0";
    for &cap in &[256usize, 1024, 4096] {
        let trace_a = file::from_path(path).unwrap().take(100_000);
        let trace_b = file::from_path(path).unwrap().take(100_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let v3 = run::<sieve_v3::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&orig, &v3, &format!("v3 bundled cap={cap}"));
    }
}

// j3 (= 1 セグメント、外部 HashMap なし、tag 並列配列) が
// 同じ trace で sieve_orig と完全に同じ evict 列を出すことを検証する。
// J3 は SIEVE 意味論を完全に保持する設計なので、minimal repro / Zipf / bundled
// のいずれでも oracle と一致しなければならない。
#[test]
fn j3_matches_orig_on_minimal_repro() {
    let trace: Vec<u64> = vec![1, 2, 3, 1, 2, 4, 5];
    let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace.iter().copied(), 3);
    let j3 = run::<sieve_j3::SieveCache<u64, u64>>(trace.iter().copied(), 3);
    assert_eviction_streams_eq(&orig, &j3, "minimal repro (j3)");
}

#[test]
fn j3_matches_orig_on_synthetic_zipf() {
    for &(skew, cap) in &[
        (1.05_f64, 64usize),
        (1.1, 128),
        (1.2, 256),
        (1.5, 1024),
    ] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let j3 = run::<sieve_j3::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&orig, &j3, &format!("j3 vs orig zipf skew={skew} cap={cap}"));
    }
}

#[test]
fn j3_matches_orig_on_bundled_zipf() {
    let path = "external/NSDI24-SIEVE/mydata/zipf/zipf_1.0";
    for &cap in &[256usize, 1024, 4096] {
        let trace_a = file::from_path(path).unwrap().take(100_000);
        let trace_b = file::from_path(path).unwrap().take(100_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let j3 = run::<sieve_j3::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&orig, &j3, &format!("j3 bundled cap={cap}"));
    }
}

/// j8 (1-shard) は SIEVE 意味論を保つので sieve_orig と eviction stream が完全一致するはず。
/// 構造的制約 per_shard <= 64 (= MAX_PER_SHARD) より cap は 64 が上限。
#[test]
fn j8_1shard_matches_orig_on_synthetic_zipf() {
    for &(skew, cap) in &[(1.05_f64, 16usize), (1.1, 32), (1.2, 64), (1.5, 64)] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let j8 = run::<sieve_j8::SieveCache<u64, u64, 1>>(trace_b, cap);
        assert_eviction_streams_eq(
            &orig,
            &j8,
            &format!("j8(1-shard) vs orig zipf skew={skew} cap={cap}"),
        );
    }
}

/// 既存 trace ファイルでも 1-shard で完全一致を確認 (cap=64 まで)。
#[test]
fn j8_1shard_matches_orig_on_bundled_zipf() {
    let path = "external/NSDI24-SIEVE/mydata/zipf/zipf_1.0";
    for &cap in &[16usize, 32, 64] {
        let trace_a = file::from_path(path).unwrap().take(100_000);
        let trace_b = file::from_path(path).unwrap().take(100_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let j8 = run::<sieve_j8::SieveCache<u64, u64, 1>>(trace_b, cap);
        assert_eviction_streams_eq(&orig, &j8, &format!("j8(1-shard) bundled cap={cap}"));
    }
}
