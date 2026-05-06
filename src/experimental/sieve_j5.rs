//! J5 — j4 から double-hash を排除した版。
//!
//! ## 動機
//!
//! `2026-05-05-sieve-j4-pershard-vs-footprint.md` で「j4 がまだ orig より遅い
//! ~3 ns の正体」を H3 (double-hash 固定費) と仮置きしたが、未測定。j4 は
//! per-op で **2 回** key を hash する:
//!
//! 1. shard 選択用 (`shard_of` で `hash_one(key)`)
//! 2. 内部 `SieveJ3::tag_of` で再 hash → tag
//!
//! 両者は同じ XXH3 を回しているだけなので、外で 1 度計算した 64-bit hash の
//! 下位ビットで shard を選び、上位 8-bit から tag を作って j3 に渡せば 2 を
//! 削れる。それが j5。
//!
//! ## j4 との差分
//!
//! - j4 (`sieve_j4.rs`) は触らない。同一 run の AB ベースラインとして残す。
//! - j5 は j3 の `pub(crate) fn {get,insert,contains}_with_hash` を呼ぶ。
//!   それ以外の構造 (shard 配列 / const generic SHARDS / cap 分配) は完全に
//!   j4 を踏襲するので、観測される差は double-hash 1 個分。
//!
//! ## 期待される結果
//!
//! 親レポートの予想: cap=256 / SHARDS=8 (per_shard=32, scan が SIMD 1 chunk で
//! 飽和済み) で j4=34 ns。これが j5 で 28 ns 程度まで落ちれば H3 ≈ 5–10 ns/op
//! の妥当性が立つ。落ちなければ H3 は弱い (= 残り差は double-hash 以外の何か)。

use crate::experimental::sieve_j3::SieveCache as J3;
use crate::sieve_cache::Xxh3Build;
use std::hash::{BuildHasher, Hash};

/// j4 と同じ既定。cap=1024 を per-shard=128 に分割するのが当面の主実験帯。
pub const DEFAULT_SHARDS: usize = 8;

pub struct SieveCache<K, V, const SHARDS: usize = DEFAULT_SHARDS> {
    shards: [J3<K, V>; SHARDS],
    hasher: Xxh3Build,
}

impl<K, V, const SHARDS: usize> SieveCache<K, V, SHARDS>
where
    K: Hash + Eq,
{
    pub fn new(capacity: usize) -> Self {
        assert!(SHARDS > 0, "SHARDS must be > 0");
        assert!(
            SHARDS.is_power_of_two(),
            "SHARDS ({SHARDS}) must be a power of two so shard select can be a bit mask"
        );
        assert!(
            capacity >= SHARDS,
            "capacity ({capacity}) must be >= SHARDS ({SHARDS}) so that each shard has cap >= 1"
        );
        let base = capacity / SHARDS;
        let extra = capacity % SHARDS;
        let shards: [J3<K, V>; SHARDS] = std::array::from_fn(|i| {
            let cap_i = base + if i < extra { 1 } else { 0 };
            J3::new(cap_i)
        });
        Self {
            shards,
            hasher: Xxh3Build,
        }
    }

    pub fn capacity(&self) -> usize {
        self.shards.iter().map(|s| s.capacity()).sum()
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.is_empty())
    }

    pub fn contains_key(&self, key: &K) -> bool {
        let h = self.hasher.hash_one(key);
        self.shards[Self::shard_of_hash(h)].contains_with_hash(key, h)
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        let h = self.hasher.hash_one(key);
        let idx = Self::shard_of_hash(h);
        self.shards[idx].get_with_hash(key, h)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        let h = self.hasher.hash_one(&key);
        let idx = Self::shard_of_hash(h);
        self.shards[idx].insert_with_hash(key, value, h)
    }

    #[inline]
    fn shard_of_hash(hash: u64) -> usize {
        // 下位ビットで shard 選択。j3 の tag (上位 8 bit) と独立 entropy。
        (hash as usize) & (SHARDS - 1)
    }
}

impl<K, V, const SHARDS: usize> crate::CacheImpl<K, V> for SieveCache<K, V, SHARDS>
where
    K: Hash + Eq,
{
    fn new(capacity: usize) -> Self {
        Self::new(capacity)
    }
    fn capacity(&self) -> usize {
        self.capacity()
    }
    fn len(&self) -> usize {
        self.len()
    }
    fn get(&mut self, key: &K) -> Option<&V> {
        self.get(key)
    }
    fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        self.insert(key, value)
    }
    fn contains_key(&self, key: &K) -> bool {
        self.contains_key(key)
    }
}

#[cfg(test)]
mod tests {
    //! j4 のテストミラー。same shape, same invariants。j4 と同じ trace に対し
    //! 同じ evict 列を返すかは tests/oracle.rs (or 既存の oracle test) で別途。

    use super::*;

    const TEST_SHARDS: usize = DEFAULT_SHARDS;

    #[test]
    fn cache_initially_empty() {
        let cache: SieveCache<i32, i32> = SieveCache::new(TEST_SHARDS * 4);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.capacity(), TEST_SHARDS * 4);
        assert!(cache.is_empty());
    }

    #[test]
    fn insert_then_get() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(TEST_SHARDS * 4);
        assert!(cache.insert(1, "a").is_none());
        assert_eq!(cache.get(&1), Some(&"a"));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn get_missing_returns_none() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(TEST_SHARDS * 4);
        cache.insert(1, "a");
        assert_eq!(cache.get(&2), None);
    }

    #[test]
    fn contains_key_reflects_insertions() {
        let mut cache: SieveCache<i32, i32> = SieveCache::new(TEST_SHARDS * 4);
        assert!(!cache.contains_key(&1));
        cache.insert(1, 10);
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
    }

    #[test]
    fn insert_existing_key_updates_value() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(TEST_SHARDS * 4);
        cache.insert(1, "a");
        assert!(cache.insert(1, "b").is_none());
        assert_eq!(cache.get(&1), Some(&"b"));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn total_capacity_is_respected_under_churn() {
        let cap = TEST_SHARDS * 16;
        let mut cache: SieveCache<u64, u64> = SieveCache::new(cap);
        for k in 0..10_000u64 {
            cache.insert(k, k);
            assert!(cache.len() <= cap);
        }
        assert_eq!(cache.len(), cap);
    }

    #[test]
    fn churn_keeps_a_full_capacity_set() {
        let cap = TEST_SHARDS * 16;
        let mut cache: SieveCache<u64, u64> = SieveCache::new(cap);
        for k in 0..50_000u64 {
            cache.insert(k, k * 3);
        }
        assert_eq!(cache.len(), cap);
        let mut alive = 0;
        for k in 0..50_000u64 {
            if cache.get(&k) == Some(&(k * 3)) {
                alive += 1;
            }
        }
        assert_eq!(alive, cap);
    }

    #[test]
    #[should_panic]
    fn capacity_below_shards_panics() {
        let _: SieveCache<u64, u64> = SieveCache::new(TEST_SHARDS - 1);
    }

    #[test]
    #[should_panic]
    fn non_power_of_two_shards_panics() {
        let _: SieveCache<u64, u64, 3> = SieveCache::new(9);
    }

    #[test]
    fn works_with_non_default_shards() {
        let mut cache_2: SieveCache<u64, u64, 2> = SieveCache::new(64);
        let mut cache_16: SieveCache<u64, u64, 16> = SieveCache::new(64);
        for k in 0..1000u64 {
            cache_2.insert(k, k);
            cache_16.insert(k, k);
        }
        assert!(cache_2.len() <= 64);
        assert!(cache_16.len() <= 64);
        assert_eq!(cache_2.capacity(), 64);
        assert_eq!(cache_16.capacity(), 64);
    }

    /// j4 と j5 が同じ trace で同じ最終状態 (= 同じ key set / 同じ value)
    /// を返すことを確認。double-hash の有無は外部から観測できる差分を生まない
    /// (shard 選択も tag も同じ XXH3 の同じビット範囲から作るため)。
    #[test]
    fn matches_j4_externally() {
        use crate::experimental::sieve_j4::SieveCache as J4;
        let cap = 128usize;
        let mut a: J4<u64, u64, 8> = J4::new(cap);
        let mut b: SieveCache<u64, u64, 8> = SieveCache::new(cap);
        // Zipf 風に偏らせた trace。同じ seed / 同じ key 列なら state が一致する。
        for k in 0..10_000u64 {
            let key = (k * 2654435761) % 1024;
            let _ = a.insert(key, key);
            let _ = b.insert(key, key);
        }
        for k in 0..1024u64 {
            assert_eq!(
                a.get(&k).copied(),
                b.get(&k).copied(),
                "j4 と j5 が key {k} で食い違う"
            );
        }
    }
}
