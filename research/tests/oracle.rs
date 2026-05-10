//! sieve_orig (NSDI'24 著者参照ポート) を oracle として、
//! 他の variant が同じトレースで同じ evict 列を出すことを検証する差分テスト。

#[cfg(feature = "external-traces")]
use senba_research::workload::file;
use senba_research::workload::zipf::ZipfGen;
use senba_research::{
    CacheImpl,
    experimental::{sieve_j3, sieve_j8, sieve_orig, sieve_v0, sieve_v1, sieve_v2, sieve_v3},
};

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
    for &(skew, cap) in &[(1.05_f64, 64usize), (1.1, 128), (1.2, 256), (1.5, 1024)] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let v0 = run::<sieve_v0::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&orig, &v0, &format!("zipf skew={skew} cap={cap}"));
    }
}

#[cfg(feature = "external-traces")]
#[test]
fn v0_matches_orig_on_bundled_zipf() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../external/NSDI24-SIEVE/mydata/zipf/zipf_1.0"
    );
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
    for &(skew, cap) in &[(1.05_f64, 64usize), (1.1, 128), (1.2, 256), (1.5, 1024)] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let v0 = run::<sieve_v0::SieveCache<u64, u64>>(trace_a, cap);
        let v1 = run::<sieve_v1::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&v0, &v1, &format!("v1 vs v0 zipf skew={skew} cap={cap}"));
    }
}

#[test]
fn v1_matches_orig_on_synthetic_zipf() {
    for &(skew, cap) in &[(1.05_f64, 64usize), (1.1, 128), (1.2, 256), (1.5, 1024)] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let v1 = run::<sieve_v1::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(
            &orig,
            &v1,
            &format!("v1 vs orig zipf skew={skew} cap={cap}"),
        );
    }
}

#[cfg(feature = "external-traces")]
#[test]
fn v1_matches_orig_on_bundled_zipf() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../external/NSDI24-SIEVE/mydata/zipf/zipf_1.0"
    );
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
    for &(skew, cap) in &[(1.05_f64, 64usize), (1.1, 128), (1.2, 256), (1.5, 1024)] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let v0 = run::<sieve_v0::SieveCache<u64, u64>>(trace_a, cap);
        let v2 = run::<sieve_v2::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&v0, &v2, &format!("v2 vs v0 zipf skew={skew} cap={cap}"));
    }
}

#[test]
fn v2_matches_orig_on_synthetic_zipf() {
    for &(skew, cap) in &[(1.05_f64, 64usize), (1.1, 128), (1.2, 256), (1.5, 1024)] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let v2 = run::<sieve_v2::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(
            &orig,
            &v2,
            &format!("v2 vs orig zipf skew={skew} cap={cap}"),
        );
    }
}

#[cfg(feature = "external-traces")]
#[test]
fn v2_matches_orig_on_bundled_zipf() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../external/NSDI24-SIEVE/mydata/zipf/zipf_1.0"
    );
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
    for &(skew, cap) in &[(1.05_f64, 64usize), (1.1, 128), (1.2, 256), (1.5, 1024)] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let v1 = run::<sieve_v1::SieveCache<u64, u64>>(trace_a, cap);
        let v3 = run::<sieve_v3::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(&v1, &v3, &format!("v3 vs v1 zipf skew={skew} cap={cap}"));
    }
}

#[test]
fn v3_matches_orig_on_synthetic_zipf() {
    for &(skew, cap) in &[(1.05_f64, 64usize), (1.1, 128), (1.2, 256), (1.5, 1024)] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let v3 = run::<sieve_v3::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(
            &orig,
            &v3,
            &format!("v3 vs orig zipf skew={skew} cap={cap}"),
        );
    }
}

#[cfg(feature = "external-traces")]
#[test]
fn v3_matches_orig_on_bundled_zipf() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../external/NSDI24-SIEVE/mydata/zipf/zipf_1.0"
    );
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
    for &(skew, cap) in &[(1.05_f64, 64usize), (1.1, 128), (1.2, 256), (1.5, 1024)] {
        let trace_a = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let trace_b = ZipfGen::new(skew, 10_000, 42).take(200_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let j3 = run::<sieve_j3::SieveCache<u64, u64>>(trace_b, cap);
        assert_eviction_streams_eq(
            &orig,
            &j3,
            &format!("j3 vs orig zipf skew={skew} cap={cap}"),
        );
    }
}

#[cfg(feature = "external-traces")]
#[test]
fn j3_matches_orig_on_bundled_zipf() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../external/NSDI24-SIEVE/mydata/zipf/zipf_1.0"
    );
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

/// c9 (per-shard `Mutex<Shard>` で wrap した SIEVE) は senba::Cache の shift-on-evict
/// を移植した形なので、1 shard 同期で sieve_orig と eviction stream が完全一致するはず。
/// c9 の API は `get -> Option<V>` (native の `&self` 経由) なので、CacheImpl trait
/// 経由ではなく直接呼ぶ。
#[test]
fn c9_1shard_matches_orig_on_synthetic_zipf() {
    use senba_research::experimental::sieve_c9::ConcurrentSieveCache as C9;
    for &(skew, cap) in &[(1.05_f64, 16usize), (1.1, 32), (1.2, 64), (1.5, 64)] {
        let trace_a: Vec<u64> = ZipfGen::new(skew, 10_000, 42).take(200_000).collect();
        let mut orig: Vec<Option<(u64, u64)>> = Vec::with_capacity(trace_a.len());
        let mut c9_evicts: Vec<Option<(u64, u64)>> = Vec::with_capacity(trace_a.len());
        let mut a: sieve_orig::SieveCache<u64, u64> = sieve_orig::SieveCache::new(cap);
        let b: C9<u64, u64> = C9::with_shards(cap, 1);
        for k in &trace_a {
            orig.push(a.insert(*k, *k));
            c9_evicts.push(b.insert(*k, *k));
        }
        assert_eviction_streams_eq(
            &orig,
            &c9_evicts,
            &format!("c9(1-shard) vs orig zipf skew={skew} cap={cap}"),
        );
    }
}

/// 既存 trace ファイルでも 1-shard で完全一致を確認 (cap=64 まで)。
#[cfg(feature = "external-traces")]
#[test]
fn c9_1shard_matches_orig_on_bundled_zipf() {
    use senba_research::experimental::sieve_c9::ConcurrentSieveCache as C9;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../external/NSDI24-SIEVE/mydata/zipf/zipf_1.0"
    );
    for &cap in &[16usize, 32, 64] {
        let trace_a: Vec<u64> = file::from_path(path).unwrap().take(100_000).collect();
        let mut orig: Vec<Option<(u64, u64)>> = Vec::with_capacity(trace_a.len());
        let mut c9_evicts: Vec<Option<(u64, u64)>> = Vec::with_capacity(trace_a.len());
        let mut a: sieve_orig::SieveCache<u64, u64> = sieve_orig::SieveCache::new(cap);
        let b: C9<u64, u64> = C9::with_shards(cap, 1);
        for k in &trace_a {
            orig.push(a.insert(*k, *k));
            c9_evicts.push(b.insert(*k, *k));
        }
        assert_eviction_streams_eq(&orig, &c9_evicts, &format!("c9(1-shard) bundled cap={cap}"));
    }
}

/// **c13s は SIEVE 等価**: lock-free Path A は eviction を起こさず、Path B/C は writer
/// Mutex 配下で senba::Cache 流の shift-on-evict を実行するため、eviction stream と
/// cache contents が `sieve_orig` と完全一致する。c12s が install-at-evicted-pos で
/// divergent だった (`c12s_1shard_diverges_from_orig_on_synthetic_zipf`) のと対比的な
/// 正テスト。
///
/// 設計詳細は `research/src/experimental/sieve_c13s.rs` の module doc 参照。
#[test]
fn c13s_1shard_matches_orig_on_synthetic_zipf() {
    use senba_research::experimental::sieve_c13s::ConcurrentSieveCache as C13s;
    let mut total_diff = 0usize;
    let mut total_ops = 0usize;
    for &(skew, cap) in &[(1.05_f64, 16usize), (1.1, 32), (1.2, 64), (1.5, 64)] {
        let trace: Vec<u64> = ZipfGen::new(skew, 10_000, 42).take(200_000).collect();
        let mut a: sieve_orig::SieveCache<u64, u64> = sieve_orig::SieveCache::new(cap);
        let b: C13s<u64, u64, 1> = C13s::new(cap);
        for k in &trace {
            a.insert(*k, *k);
            b.insert(*k, *k);
        }
        let mut diff = 0usize;
        for &k in &trace {
            if a.get(&k).copied() != b.get(&k) {
                diff += 1;
            }
        }
        eprintln!(
            "[c13s vs orig] skew={skew} cap={cap}: diff={diff}/{} ({:.4}%)",
            trace.len(),
            100.0 * diff as f64 / trace.len() as f64
        );
        total_diff += diff;
        total_ops += trace.len();
    }
    assert_eq!(
        total_diff, 0,
        "c13s が sieve_orig と divergent ({total_ops} ops 中 {total_diff} diff): \
         Path A の lock-free CAS が SIEVE 不変条件を破っている (= 設計通りでない)"
    );
}

/// **c16s は SIEVE 等価**: c14s と同型のロジック (Path A lock-free / Path B/C
/// Mutex 配下 shift-on-evict) で、変更箇所は per-shard layout (`ShardHot` への
/// hot field 集約 + visited word index 縮退) のみ。state machine は不変なので
/// `sieve_orig` と eviction stream / cache contents が一致する。
///
/// 設計詳細は `docs/reports/2026-05-10-c16s-design.md` 参照。
#[test]
fn c16s_1shard_matches_orig_on_synthetic_zipf() {
    use senba_research::experimental::sieve_c16s::ConcurrentSieveCache as C16s;
    let mut total_diff = 0usize;
    let mut total_ops = 0usize;
    for &(skew, cap) in &[(1.05_f64, 16usize), (1.1, 32), (1.2, 64), (1.5, 64)] {
        let trace: Vec<u64> = ZipfGen::new(skew, 10_000, 42).take(200_000).collect();
        let mut a: sieve_orig::SieveCache<u64, u64> = sieve_orig::SieveCache::new(cap);
        let b: C16s<u64, u64, 1> = C16s::new(cap);
        for k in &trace {
            a.insert(*k, *k);
            b.insert(*k, *k);
        }
        let mut diff = 0usize;
        for &k in &trace {
            if a.get(&k).copied() != b.get(&k) {
                diff += 1;
            }
        }
        eprintln!(
            "[c16s vs orig] skew={skew} cap={cap}: diff={diff}/{} ({:.4}%)",
            trace.len(),
            100.0 * diff as f64 / trace.len() as f64
        );
        total_diff += diff;
        total_ops += trace.len();
    }
    assert_eq!(
        total_diff, 0,
        "c16s が sieve_orig と divergent ({total_ops} ops 中 {total_diff} diff): \
         per-shard layout 変更が SIEVE 不変条件を破っている (= 設計通りでない)"
    );
}

/// **c17s は SIEVE 等価**: 同期通知を tag → entry version に逃がした (G2-α-1) variant。
/// Path A は tag を一切触らず entry version 偶奇 flip だけで reader 同期、tag VERSION
/// bit 削除で HASH を 9 bit (c11s と同等) に拡張。state machine は c14s/c16s と同型なので
/// `sieve_orig` と eviction stream / cache contents が一致する。
///
/// 設計詳細は `docs/reports/2026-05-11-c17s-design.md` 参照。
#[test]
fn c17s_1shard_matches_orig_on_synthetic_zipf() {
    use senba_research::experimental::sieve_c17s::ConcurrentSieveCache as C17s;
    let mut total_diff = 0usize;
    let mut total_ops = 0usize;
    for &(skew, cap) in &[(1.05_f64, 16usize), (1.1, 32), (1.2, 64), (1.5, 64)] {
        let trace: Vec<u64> = ZipfGen::new(skew, 10_000, 42).take(200_000).collect();
        let mut a: sieve_orig::SieveCache<u64, u64> = sieve_orig::SieveCache::new(cap);
        let b: C17s<u64, u64, 1> = C17s::new(cap);
        for k in &trace {
            a.insert(*k, *k);
            b.insert(*k, *k);
        }
        let mut diff = 0usize;
        for &k in &trace {
            if a.get(&k).copied() != b.get(&k) {
                diff += 1;
            }
        }
        eprintln!(
            "[c17s vs orig] skew={skew} cap={cap}: diff={diff}/{} ({:.4}%)",
            trace.len(),
            100.0 * diff as f64 / trace.len() as f64
        );
        total_diff += diff;
        total_ops += trace.len();
    }
    assert_eq!(
        total_diff, 0,
        "c17s が sieve_orig と divergent ({total_ops} ops 中 {total_diff} diff): \
         entry-level seqlock + tag VERSION bit 削除が SIEVE 不変条件を破っている (= 設計通りでない)"
    );
}

/// **c12s は SIEVE と等価ではない** という研究結果を記録する負テスト。
///
/// 設計文書 `docs/reports/2026-05-08-c12s-cas-slot-claim-design.md` §3 では
/// 「install-at-evicted-pos は SIEVE 等価」と仮説していたが、Phase 2 oracle 検証で
/// **eviction stream / cache contents の双方で c12s ≠ sieve_orig** が判明した。
///
/// 原因: install-at-evicted-pos は新 entry を hand 直前の pos (= 次の sweep 対象位置)
/// に置く。sieve_orig (linked list で head insert) と senba::Cache (shift-on-evict で
/// tail insert) は「新 entry が tail 側にあり最後に sweep される」のに対し、c12s は
/// 「新 entry が次の sweep 対象になる」 → 高 churn の zipf trace で頻繁に新 entry が
/// 即 evict され、cache contents 自体が divergent になる。
///
/// 本テストは **divergent であること** を eviction stream の length-mismatch (実際は
/// 同じだが、unconditional な assert で意図的に失敗させない) ではなく diff 件数 > 0 で
/// 確認する形にしてある。**default では `#[ignore]`** で skip されるが、`cargo test
/// --ignored` で実走できる。詳細は report 参照。
#[test]
#[ignore = "c12s install-at-evicted-pos is not SIEVE-equivalent — divergent eviction policy by design (see docs/reports/2026-05-08-c12s-cas-slot-claim.md §3)"]
fn c12s_1shard_diverges_from_orig_on_synthetic_zipf() {
    use senba_research::experimental::sieve_c12s::ConcurrentSieveCache as C12s;
    let mut total_diff = 0usize;
    let mut total_ops = 0usize;
    for &(skew, cap) in &[(1.05_f64, 16usize), (1.1, 32), (1.2, 64), (1.5, 64)] {
        let trace: Vec<u64> = ZipfGen::new(skew, 10_000, 42).take(200_000).collect();
        let mut a: sieve_orig::SieveCache<u64, u64> = sieve_orig::SieveCache::new(cap);
        let b: C12s<u64, u64, 1> = C12s::new(cap);
        for k in &trace {
            a.insert(*k, *k);
            b.insert(*k, *k);
        }
        let mut diff = 0usize;
        for &k in &trace {
            if a.get(&k).copied() != b.get(&k) {
                diff += 1;
            }
        }
        eprintln!(
            "[c12s vs orig] skew={skew} cap={cap}: diff={diff}/{} ({:.1}%)",
            trace.len(),
            100.0 * diff as f64 / trace.len() as f64
        );
        total_diff += diff;
        total_ops += trace.len();
    }
    assert!(
        total_diff > 0,
        "c12s と orig が完全一致してしまった ({total_ops} ops 中 0 diff): \
         install-at-evicted-pos 設計が変更されたか、テスト trace の特性が変わった"
    );
}

/// 既存 trace ファイルでも 1-shard で完全一致を確認 (cap=64 まで)。
#[cfg(feature = "external-traces")]
#[test]
fn j8_1shard_matches_orig_on_bundled_zipf() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../external/NSDI24-SIEVE/mydata/zipf/zipf_1.0"
    );
    for &cap in &[16usize, 32, 64] {
        let trace_a = file::from_path(path).unwrap().take(100_000);
        let trace_b = file::from_path(path).unwrap().take(100_000);
        let orig = run::<sieve_orig::SieveCache<u64, u64>>(trace_a, cap);
        let j8 = run::<sieve_j8::SieveCache<u64, u64, 1>>(trace_b, cap);
        assert_eviction_streams_eq(&orig, &j8, &format!("j8(1-shard) bundled cap={cap}"));
    }
}
