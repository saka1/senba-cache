use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use super::Key;

/// Seedable Zipf 整数キー列ジェネレータ。
///
/// 有限 N に対する一般化 Zipf 分布 P(K = i) = (1/(i+1)^α) / H_N(α) を、
/// CDF テーブル + uniform 二分探索でサンプリングする。
///
/// `rand_distr::Zipf` と違って α > 0 ならどんな値でもよい
/// (NSDI'24 SIEVE 論文の synthetic 実験域 α ∈ {0.2, ..., 1.6} を全部カバー)。
/// 構築コストは O(N), per-sample コストは O(log N), メモリは 8N バイト。
pub struct ZipfGen {
    rng: StdRng,
    /// `cdf[i]` は P(K ≤ i) ∈ (0, 1]。狭義単調増加。
    cdf: Vec<f64>,
}

impl ZipfGen {
    /// `skew` は > 0。
    pub fn new(skew: f64, n_keys: u64, seed: u64) -> Self {
        debug_assert!(n_keys > 0, "n_keys must be > 0");
        debug_assert!(
            skew > 0.0 && skew.is_finite(),
            "skew must be finite and > 0"
        );

        let n = n_keys as usize;
        let mut cdf = Vec::with_capacity(n);
        let mut acc = 0.0f64;
        for i in 0..n {
            acc += 1.0 / ((i + 1) as f64).powf(skew);
            cdf.push(acc);
        }
        let total = cdf[n - 1];
        for v in &mut cdf {
            *v /= total;
        }
        // 浮動小数の累積誤差で末尾が 1.0 にならないことがあるので固定。
        cdf[n - 1] = 1.0;

        Self {
            rng: StdRng::seed_from_u64(seed),
            cdf,
        }
    }
}

impl Iterator for ZipfGen {
    type Item = Key;

    fn next(&mut self) -> Option<Self::Item> {
        let u: f64 = self.rng.random_range(0.0..1.0);
        // 最小の i で cdf[i] >= u
        let i = self.cdf.partition_point(|&p| p < u);
        Some(i as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // 同じ seed なら同じ列を生成する (再現性)
    #[test]
    fn same_seed_same_sequence() {
        let a: Vec<_> = ZipfGen::new(1.2, 1000, 42).take(1000).collect();
        let b: Vec<_> = ZipfGen::new(1.2, 1000, 42).take(1000).collect();
        assert_eq!(a, b);
    }

    // 全キーが [0, n_keys) に収まる
    #[test]
    fn keys_in_range() {
        let n = 100u64;
        let bad = ZipfGen::new(1.1, n, 7).take(10_000).any(|k| k >= n);
        assert!(!bad);
    }

    // skew>1.0 ではキー空間の一部に集中する (=ユニーク数 < キー空間)
    #[test]
    fn high_skew_concentrates() {
        let n = 10_000u64;
        let unique: HashSet<_> = ZipfGen::new(2.0, n, 42).take(5_000).collect();
        assert!(unique.len() < n as usize);
    }

    // α ≤ 1.0 (NSDI'24 SIEVE が主に使う領域) でもサンプルできる
    #[test]
    fn alpha_le_one_works() {
        for skew in [0.6f64, 0.8, 1.0] {
            let n = 10_000u64;
            let xs: Vec<_> = ZipfGen::new(skew, n, 42).take(5_000).collect();
            assert_eq!(xs.len(), 5_000);
            assert!(xs.iter().all(|&k| k < n));
        }
    }

    // skew が小さいほど分布は flat に近づく → ユニーク数が増える
    #[test]
    fn lower_skew_is_flatter() {
        let n = 10_000u64;
        let take = 5_000;
        let u_low: HashSet<_> = ZipfGen::new(0.6, n, 42).take(take).collect();
        let u_high: HashSet<_> = ZipfGen::new(1.4, n, 42).take(take).collect();
        assert!(
            u_low.len() > u_high.len(),
            "expected α=0.6 to produce more uniques than α=1.4 ({} vs {})",
            u_low.len(),
            u_high.len()
        );
    }

    // CDF の最頻キーは 0 (= 一番人気) であるべき
    #[test]
    fn mode_is_zero() {
        let n = 100u64;
        let mut counts = vec![0u32; n as usize];
        for k in ZipfGen::new(1.2, n, 42).take(50_000) {
            counts[k as usize] += 1;
        }
        let mode = counts
            .iter()
            .enumerate()
            .max_by_key(|&(_, &c)| c)
            .unwrap()
            .0;
        assert_eq!(mode, 0);
    }
}
