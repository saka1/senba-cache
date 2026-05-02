use rand::SeedableRng;
use rand::rngs::StdRng;
use rand_distr::{Distribution, Zipf};

use super::Key;

/// Seedable Zipf 整数キー列ジェネレータ。
///
/// `rand_distr::Zipf` は実数 ∈ [1, n_keys] を返すので、
/// `floor` して 0-origin (`[0, n_keys - 1]`) に正規化する。
pub struct ZipfGen {
    rng: StdRng,
    dist: Zipf<f64>,
    n_keys: u64,
}

impl ZipfGen {
    /// `skew` は `> 1.0` 必須 (rand_distr の `Zipf` 仕様)。
    pub fn new(skew: f64, n_keys: u64, seed: u64) -> Self {
        debug_assert!(n_keys > 0, "n_keys must be > 0");
        let dist = Zipf::new(n_keys as f64, skew).expect("invalid Zipf params (need skew > 1.0)");
        Self {
            rng: StdRng::seed_from_u64(seed),
            dist,
            n_keys,
        }
    }
}

impl Iterator for ZipfGen {
    type Item = Key;

    fn next(&mut self) -> Option<Self::Item> {
        let r: f64 = self.dist.sample(&mut self.rng);
        let k = (r as u64).saturating_sub(1).min(self.n_keys - 1);
        Some(k)
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
}
