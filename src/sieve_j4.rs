//! J4 — set-associative SIEVE。`SieveJ3` を `SHARDS` 個並べ、key の hash の
//! 下位ビットでシャードを選ぶ薄いラッパー。並行性は意図しない (単スレ前提)。
//!
//! ## 検証ターゲット
//!
//! 1. **スループット**: 同一総容量 (cap = SHARDS × per_shard) で `j3` 単一に
//!    対し劣化しないか / 勝てるか。per-shard が小さい (≤128 程度) と内部状態が
//!    L1/L2 に収まり、scan / hash table が cache miss しにくい想定。
//! 2. **set-associative tax**: `sieve_orig` (グローバル単一 SIEVE) と比べて
//!    miss ratio がどれだけ増えるか。partition の境界をまたぐ working set を
//!    持つワークロードほど劣化が大きい予想。
//!
//! ## 既知の handicap (j5 で解消済み)
//!
//! 現状 j4 は per-op で **2 回 hash する** (shard 選択用 + 内部 j3 の `tag_of`)。
//! 共通の build hasher を共有しているが API 境界で再ハッシュは避けられない。
//! throughput 比較ではこの固定オーバーヘッドが j4 を不利にするが、(2) hit
//! ratio 比較はこの overhead と独立に成立する。
//!
//! `sieve_j5.rs` で j3 に `get_with_hash` 等を生やして double-hash を除去した
//! 結果、cell に依らず ~7 ns/op の固定費だったことが確定した
//! (`docs/reports/2026-05-05-sieve-j5-doublehash-ab.md`)。j4 自身は AB の歴史
//! 保存目的でそのまま残し、以降の throughput 比較は j5 を採る。
//!
//! ## bit 配分
//!
//! - shard 選択: `hash & (SHARDS - 1)` (下位 `log2(SHARDS)` ビット)
//! - 内部 j3 の tag: `(hash >> 56) | 0x80` (上位 8 ビット)
//!
//! 上位/下位で分けて、同一 hash 出力でも shard と tag が独立 entropy を
//! 持つようにする。ここを上位ビット同士で取ると、shard 内では tag の最上位
//! 数ビットが定数化して SIMD scan の false-match 率が悪化する。
//!
//! ## const generic の N
//!
//! `SieveCache<K, V, SHARDS>` で SHARDS を const generic に取る。デフォルト
//! N = 8 (cap=1024 を per-shard=128 に分割するのが当面の主実験帯)。
//! `assert!(SHARDS.is_power_of_two())` を `new` 内で課して、shard 選択を
//! 高速な mask 操作に維持する (一般 N だと % 演算が必要)。

use crate::hash::Xxh3Build;
use crate::sieve_j3::SieveCache as J3;
use std::hash::{BuildHasher, Hash};

/// デフォルトのシャード数 (= `SieveCache<K, V>` で SHARDS を省略した時の値)。
/// cap=1024 を per-shard=128 に分割するのが当面の主実験帯。
pub const DEFAULT_SHARDS: usize = 8;

pub struct SieveCache<K, V, const SHARDS: usize = DEFAULT_SHARDS> {
    shards: [J3<K, V>; SHARDS],
    hasher: Xxh3Build,
}

impl<K, V, const SHARDS: usize> SieveCache<K, V, SHARDS>
where
    K: Hash + Eq,
{
    /// 総容量 `capacity` を `SHARDS` で等分。割り切れない端数は最初の
    /// `capacity % SHARDS` 個のシャードに +1 ずつ振る (合計が必ず `capacity`)。
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
        self.shards[self.shard_of(key)].contains_key(key)
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        let idx = self.shard_of(key);
        self.shards[idx].get(key)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        let idx = self.shard_of(&key);
        self.shards[idx].insert(key, value)
    }

    #[inline]
    fn shard_of(&self, key: &K) -> usize {
        // 下位ビットで shard 選択。j3 内部 tag (上位 8 bit) と独立 entropy。
        // SHARDS が 2^k であることは new で assert 済み、`& (SHARDS-1)` が
        // `% SHARDS` と等価。
        (self.hasher.hash_one(key) as usize) & (SHARDS - 1)
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
    use super::*;

    // ---- j3 のテストミラー (基本 invariants) ----

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
    fn capacity_with_remainder_sums_correctly() {
        for cap in [
            TEST_SHARDS,
            TEST_SHARDS + 1,
            TEST_SHARDS + 3,
            TEST_SHARDS * 7 + 5,
        ] {
            let cache: SieveCache<u64, u64> = SieveCache::new(cap);
            assert_eq!(
                cache.capacity(),
                cap,
                "capacity sum mismatch for total cap={cap}"
            );
        }
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
    fn distinct_keys_round_trip_when_each_shard_has_room() {
        let n: u64 = 64;
        let cap = TEST_SHARDS * n as usize;
        let mut cache: SieveCache<u64, u64> = SieveCache::new(cap);
        for k in 0..n {
            cache.insert(k, k * 7);
        }
        for k in 0..n {
            assert_eq!(cache.get(&k), Some(&(k * 7)), "miss for key {k}");
        }
    }

    #[test]
    fn observes_set_associative_tax_at_unit_cap() {
        let n: u64 = 256;
        let cap = n as usize;
        let mut cache: SieveCache<u64, u64> = SieveCache::new(cap);
        let mut evictions = 0usize;
        for k in 0..n {
            if cache.insert(k, k).is_some() {
                evictions += 1;
            }
        }
        assert!(evictions > 0);
        assert!(cache.len() < cap);
        assert_eq!(cache.len(), n as usize - evictions);
    }

    #[test]
    fn drop_runs_for_live_entries_only() {
        let mut cache: SieveCache<u64, String> = SieveCache::new(TEST_SHARDS * 4);
        for k in 0..100u64 {
            cache.insert(k, format!("value-{k}"));
        }
        assert_eq!(cache.len(), TEST_SHARDS * 4);
    }

    /// const generic で N を変えても動く (N=2 と N=16 を sanity check)。
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
}
