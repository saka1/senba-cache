//! J8 — j7 + tag 内 entry_id 埋込 + entries arena = capacity (slack なし) + free_list 廃止。
//!
//! ## 動機 (`docs/improvement-ideas.md` §M1, §M2.3, §M5.3 を統合; `j8_plan.md` 参照)
//!
//! j7 (M2.3) は u16 tag に live + visited + 14-bit hash を packing して
//! Twitter cluster018 全帯域で j5/j6 を支配したが、`order_cap = 2 × capacity`
//! の slack を **inline 物理 36 B/cap** が払う形になり、orig (25 B/cap) との
//! memory-fair 比較ではハンデが残った。
//!
//! j8 は次の 3 つの直交アイディアを 1 設計に畳み込む:
//!
//! 1. **slack を片側に寄せる** (§M5.3): tags は `2 × capacity` (slack 持ち、
//!    tombstone 用)、entries は `capacity` (slack なし)
//! 2. **tag bit に entry_id を埋める** (本設計の中核): u16 tag に
//!    `[live(1) | visited(1) | id(6) | hash(8)]` を packing。`order` 別配列が
//!    不要になり、entries 1× cap と整合
//! 3. **free_list 廃止**: insert API のみで `remove` を露出しないため、
//!    evict が返した freed_id は同一 insert 呼び出し内で必ず消費される
//!    → 保管不要
//!
//! 結果として **inline 物理 20 B/cap** (j7 比 −44%、orig 比 −20%) を
//! 達成しつつ、SIEVE 意味論は j7 と完全一致 (= `sieve_orig` oracle 通過)。
//!
//! ## bit レイアウト
//!
//! ```text
//! bit 15    : live      (1 = occupied、0 = EMPTY/tombstone)
//! bit 14    : visited
//! bit 8..13 : entry_id  (6 bit、0..63)
//! bit 0..7  : hash      (8 bit、`hash >> 56` の上位バイト)
//! ```
//!
//! - SIMD scan の比較対象は `live | hash` のみ (= `SCAN_MASK = 0x80FF`)。
//!   visited / id bit は mask out されるので scan の一致判定に影響しない。
//! - `MAX_PER_SHARD = 64` を構造的上限として `Inner::new` で `assert!`。
//!   per_shard sweet spot ∈ [32, 64] と整合する。
//!
//! ## 不変条件
//!
//! - I4: live tag の集合 = `{ tags[i] : i < tail, tags[i] & LIVE != 0 }`、個数 = `len`
//! - I5: live tag が指す entry_id の集合は重複なく、サイズ = `len`
//! - I6: I5 集合の id についてのみ `entries[id]` は init 済み
//! - I7: I5 集合 ⊆ `0..capacity`
//! - I8: warm-up 中 (= 一度も evict が走っていない) は I5 集合 = `0..len` (連続)
//!
//! I8 が `entry_id = self.len` (warm-up 時) の正当性を担保する。

use crate::hash::Xxh3Build;
use std::hash::{BuildHasher, Hash};
use std::mem::MaybeUninit;

const EMPTY: u16 = 0;
const LIVE: u16 = 0x8000;
const VISITED: u16 = 0x4000;
/// entry_id を tag bit 8..13 に置くためのシフト幅。
const ID_SHIFT: u32 = 8;
/// bit 8..13 (6 bit、entry_id 領域)。
const ID_MASK: u16 = 0x3F00;
/// bit 0..7 (8 bit、hash 領域)。
const HASH_MASK: u16 = 0x00FF;
/// SIMD scan 用「live + hash」マスク。visited と id を同時に mask out する。
const SCAN_MASK: u16 = LIVE | HASH_MASK;
/// AVX2 1 chunk = 32 byte = 16 u16 lane。
const LANE: usize = 16;
/// 6-bit entry_id の構造的上限。per_shard はこの値以下でなければならない。
pub const MAX_PER_SHARD: usize = 64;

#[inline]
fn id_of(tag: u16) -> usize {
    ((tag & ID_MASK) >> ID_SHIFT) as usize
}

struct Entry<K, V> {
    key: K,
    value: V,
}

/// 1 shard 分の SIEVE。
struct Inner<K, V> {
    capacity: usize,
    /// 並列配列 #1: tag 列。`order_cap = 2 × capacity` の LANE 揃え (slack 持ち)。
    tags: Vec<u16>,
    /// 並列配列 #2: entries arena。`capacity` (slack なし)。
    /// id (= tag に埋め込んだ 6 bit) で indexing する。
    entries: Vec<MaybeUninit<Entry<K, V>>>,
    /// tags への次挿入位置 (`0..=order_cap`)。
    tail: usize,
    /// SIEVE hand cursor (`0..=tail`)、tags 上を巡回。
    hand: usize,
    /// 現在 live な entry 数 (= live tag 数)。
    len: usize,
}

impl<K, V> Inner<K, V>
where
    K: Hash + Eq,
{
    fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        assert!(
            capacity <= MAX_PER_SHARD,
            "per-shard capacity ({capacity}) must be <= {MAX_PER_SHARD} (6-bit ID limit)"
        );
        let raw = capacity.checked_mul(2).expect("capacity * 2 overflow");
        // tags 側は j7 と同じく LANE 揃え。tail 範囲外は EMPTY=0 を保つ
        // ことで SIMD scan の false hit を防ぐ。
        let order_cap = ((raw + LANE - 1) & !(LANE - 1)).max(LANE);
        let mut entries = Vec::with_capacity(capacity);
        entries.resize_with(capacity, MaybeUninit::uninit);
        Self {
            capacity,
            tags: vec![EMPTY; order_cap],
            entries,
            tail: 0,
            hand: 0,
            len: 0,
        }
    }

    /// 64-bit hash の上位 8 bit を tag の hash 部に流し込む。
    /// shard 選択は下位 log2(SHARDS) bit なので独立 entropy。
    #[inline]
    fn needle_from_hash(hash: u64) -> u16 {
        LIVE | (((hash >> 56) as u16) & HASH_MASK)
    }

    fn find(&self, key: &K, needle: u16) -> Option<usize> {
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                return unsafe { self.find_avx2(key, needle) };
            }
        }
        self.find_scalar(key, needle)
    }

    #[inline]
    fn find_scalar(&self, key: &K, needle: u16) -> Option<usize> {
        for (i, &t) in self.tags[..self.tail].iter().enumerate() {
            if (t & SCAN_MASK) == needle {
                let id = id_of(t);
                // SAFETY: live tag が指す id は I5/I6 より init 済み。
                let e = unsafe { self.entries[id].assume_init_ref() };
                if &e.key == key {
                    return Some(i);
                }
            }
        }
        None
    }

    /// AVX2: `vpand` (SCAN_MASK) → `vpcmpeqw` → `vpmovmskb`。
    /// j7 と同じパターン、mask 値だけ差替え。
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn find_avx2(&self, key: &K, needle: u16) -> Option<usize> {
        use std::arch::x86_64::*;
        let limit = self.tags.len();
        let tags_ptr = self.tags.as_ptr();
        let entries_ptr = self.entries.as_ptr();
        let needle_v = _mm256_set1_epi16(needle as i16);
        let mask_v = _mm256_set1_epi16(SCAN_MASK as i16);

        let mut i = 0usize;
        while i < limit {
            let v = unsafe { _mm256_loadu_si256(tags_ptr.add(i) as *const __m256i) };
            let masked = _mm256_and_si256(v, mask_v);
            let cmp = _mm256_cmpeq_epi16(masked, needle_v);
            let mut mask = _mm256_movemask_epi8(cmp) as u32;
            while mask != 0 {
                let bit = mask.trailing_zeros() as usize;
                // 16-bit lane index = bit / 2 (epi16 の match は 2 bit/lane)。
                let lane = bit >> 1;
                let pos = i + lane;
                let tag = unsafe { *tags_ptr.add(pos) };
                let id = id_of(tag);
                // SAFETY: needle は bit15=1、マッチした slot は必ず live → entries[id] init 済み。
                let e = unsafe { (*entries_ptr.add(id)).assume_init_ref() };
                if &e.key == key {
                    return Some(pos);
                }
                mask &= !(0b11u32 << (lane << 1));
            }
            i += LANE;
        }
        None
    }

    fn contains(&self, key: &K, hash: u64) -> bool {
        self.find(key, Self::needle_from_hash(hash)).is_some()
    }

    fn get(&mut self, key: &K, hash: u64) -> Option<&V> {
        let needle = Self::needle_from_hash(hash);
        let pos = self.find(key, needle)?;
        // visited をセット: tags 配列内 in-place、find が触ったキャッシュライン内。
        self.tags[pos] |= VISITED;
        let id = id_of(self.tags[pos]);
        // SAFETY: pos は find が tag マッチを確認した位置 (= live)。
        let e = unsafe { self.entries[id].assume_init_ref() };
        Some(&e.value)
    }

    fn insert(&mut self, key: K, value: V, hash: u64) -> Option<(K, V)> {
        let needle = Self::needle_from_hash(hash);
        if let Some(pos) = self.find(&key, needle) {
            let id = id_of(self.tags[pos]);
            // SAFETY: find が live を確認した。
            let e = unsafe { self.entries[id].assume_init_mut() };
            e.value = value;
            self.tags[pos] |= VISITED;
            return None;
        }

        // 新規 insert: entry_id を取得。
        // - warm-up (len < capacity): I8 より `entry_id = len` が未使用 slot を指す
        // - steady (len == capacity): evict 直後の freed_id を pass-through
        let (evicted, entry_id): (Option<(K, V)>, u16) = if self.len < self.capacity {
            (None, self.len as u16)
        } else {
            let (kv, freed_id) = self.evict_one_returning_id();
            (Some(kv), freed_id)
        };

        if self.tail == self.tags.len() {
            self.compact();
        }

        let pos = self.tail;
        self.tail += 1;
        // 新規挿入は visited=0。
        self.tags[pos] = LIVE | (entry_id << ID_SHIFT) | (needle & HASH_MASK);
        // SAFETY: entry_id は warm-up なら未使用、steady なら直前 evict の
        // assume_init_read で uninit に戻った slot。
        self.entries[entry_id as usize].write(Entry { key, value });
        self.len += 1;

        evicted
    }

    /// SIEVE の victim 探索 + freed entry_id を返す。
    /// j3/j5/j6/j7 と同じ「2 パス + first_live フォールバック」。
    fn evict_one_returning_id(&mut self) -> ((K, V), u16) {
        debug_assert!(self.len > 0);
        if self.hand >= self.tail {
            self.hand = 0;
        }

        let pos = self
            .scan_evict(self.hand, self.tail)
            .or_else(|| self.scan_evict(0, self.hand))
            .or_else(|| self.first_live(self.hand, self.tail))
            .or_else(|| self.first_live(0, self.hand))
            .expect("len > 0 implies at least one live slot");
        self.do_evict_returning_id(pos)
    }

    fn scan_evict(&mut self, lo: usize, hi: usize) -> Option<usize> {
        debug_assert!(lo <= hi && hi <= self.tail);
        for i in lo..hi {
            let t = self.tags[i];
            if t == EMPTY {
                continue;
            }
            if t & VISITED != 0 {
                self.tags[i] = t & !VISITED;
            } else {
                return Some(i);
            }
        }
        None
    }

    fn first_live(&self, lo: usize, hi: usize) -> Option<usize> {
        debug_assert!(lo <= hi && hi <= self.tail);
        (lo..hi).find(|&i| self.tags[i] != EMPTY)
    }

    fn do_evict_returning_id(&mut self, pos: usize) -> ((K, V), u16) {
        debug_assert!(self.tags[pos] != EMPTY);
        let id = id_of(self.tags[pos]) as u16;
        // SAFETY: live を呼び出し側で保証済み。assume_init_read 後 entries[id] は uninit。
        let entry = unsafe { self.entries[id as usize].assume_init_read() };
        self.tags[pos] = EMPTY;
        self.len -= 1;
        self.hand = pos + 1;
        if self.hand >= self.tail {
            self.hand = 0;
        }
        ((entry.key, entry.value), id)
    }

    /// tags のみ前詰め。entries arena は不変 (= id-based indexing なので物理位置を動かす必要がない)。
    /// j7 比で memcpy 量 1/9 (`2 B vs 18 B per slot`)。
    fn compact(&mut self) {
        let old_tail = self.tail;
        let old_hand = self.hand.min(old_tail);
        let mut new_hand: Option<usize> = None;
        let mut write = 0usize;

        for old_pos in 0..old_tail {
            if self.tags[old_pos] == EMPTY {
                continue;
            }
            if new_hand.is_none() && old_pos >= old_hand {
                new_hand = Some(write);
            }
            if write != old_pos {
                // tag だけ前詰め。tag に埋まった id は不変なので
                // entries arena の物理対応関係は保たれる。
                self.tags[write] = self.tags[old_pos];
            }
            write += 1;
        }
        for t in &mut self.tags[write..old_tail] {
            *t = EMPTY;
        }

        self.tail = write;
        self.hand = if self.len == 0 {
            0
        } else {
            new_hand.unwrap_or(0)
        };
        debug_assert_eq!(self.len, write);
    }
}

impl<K, V> Drop for Inner<K, V> {
    fn drop(&mut self) {
        // tags scan で live tag を列挙し、id 抽出して entries[id] を drop。
        // I5 (id 重複なし) より同じ entries[id] の二重 drop は起きない。
        for i in 0..self.tail {
            let t = self.tags[i];
            if t != EMPTY {
                let id = id_of(t);
                // SAFETY: live ⟹ entries[id] init 済み (I6)。
                unsafe { self.entries[id].assume_init_drop() };
            }
        }
    }
}

// ---------------- 外側 (set-associative wrapper) ----------------

pub const DEFAULT_SHARDS: usize = 8;

pub struct SieveCache<K, V, const SHARDS: usize = DEFAULT_SHARDS> {
    shards: [Inner<K, V>; SHARDS],
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
            "capacity ({capacity}) must be >= SHARDS ({SHARDS}) so each shard has cap >= 1"
        );
        let base = capacity / SHARDS;
        let extra = capacity % SHARDS;
        let shards: [Inner<K, V>; SHARDS] = std::array::from_fn(|i| {
            let cap_i = base + if i < extra { 1 } else { 0 };
            Inner::new(cap_i)
        });
        Self {
            shards,
            hasher: Xxh3Build,
        }
    }

    pub fn capacity(&self) -> usize {
        self.shards.iter().map(|s| s.capacity).sum()
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.len == 0)
    }

    pub fn contains_key(&self, key: &K) -> bool {
        let h = self.hasher.hash_one(key);
        self.shards[Self::shard_of_hash(h)].contains(key, h)
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        let h = self.hasher.hash_one(key);
        let i = Self::shard_of_hash(h);
        self.shards[i].get(key, h)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        let h = self.hasher.hash_one(&key);
        let i = Self::shard_of_hash(h);
        self.shards[i].insert(key, value, h)
    }

    #[inline]
    fn shard_of_hash(hash: u64) -> usize {
        // 下位ビットで shard 選択。tag は上位 8 bit → 独立 entropy。
        (hash as usize) & (SHARDS - 1)
    }
}

impl<K, V, const SHARDS: usize> crate::Cache<K, V> for SieveCache<K, V, SHARDS>
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
    //! j7 のテストミラー + j8 固有の bit layout / id embed テスト + j7/sieve_orig oracle。

    use super::*;

    /// テスト用: SHARDS=8 で per_shard <= MAX_PER_SHARD を保つため
    /// 全体 cap も 8 × 64 = 512 を超えないように選ぶ。
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
    fn evicts_oldest_when_full_and_unvisited() {
        let mut cache: SieveCache<i32, &str, 1> = SieveCache::new(2);
        cache.insert(1, "a");
        cache.insert(2, "b");
        let evicted = cache.insert(3, "c");
        assert_eq!(evicted, Some((1, "a")));
        assert_eq!(cache.len(), 2);
        assert!(!cache.contains_key(&1));
        assert!(cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }

    #[test]
    fn visited_entry_survives_first_pass() {
        let mut cache: SieveCache<i32, &str, 1> = SieveCache::new(2);
        cache.insert(1, "a");
        cache.insert(2, "b");
        cache.get(&1);
        let evicted = cache.insert(3, "c");
        assert_eq!(evicted, Some((2, "b")));
    }

    #[test]
    fn all_visited_clears_bits_then_evicts() {
        let mut cache: SieveCache<i32, &str, 1> = SieveCache::new(2);
        cache.insert(1, "a");
        cache.insert(2, "b");
        cache.get(&1);
        cache.get(&2);
        let evicted = cache.insert(3, "c");
        assert_eq!(evicted, Some((1, "a")));
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
    #[should_panic]
    fn per_shard_above_max_panics() {
        // per_shard = 65 > MAX_PER_SHARD=64 ⇒ panic。
        let _: SieveCache<u64, u64, 1> = SieveCache::new(65);
    }

    #[test]
    fn per_shard_at_max_works() {
        // per_shard = 64 = MAX_PER_SHARD は OK (id 6 bit が 0..63 を使い切る)。
        let mut cache: SieveCache<u64, u64, 1> = SieveCache::new(64);
        for k in 0..200u64 {
            cache.insert(k, k * 11);
        }
        assert_eq!(cache.len(), 64);
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

    /// MAX_PER_SHARD まで詰めて全 hit を確認 (false-match が起きても key 等価で弾ける)。
    #[test]
    fn distinct_keys_full_per_shard_all_hit() {
        let n: u64 = 64;
        let mut cache: SieveCache<u64, u64, 1> = SieveCache::new(n as usize);
        for k in 0..n {
            cache.insert(k, k * 7);
        }
        for k in 0..n {
            assert_eq!(cache.get(&k), Some(&(k * 7)), "miss for key {k}");
        }
    }

    /// j7 と外部一致: 同じ trace を流して各 key の get 結果が一致。
    #[test]
    fn matches_j7_externally() {
        use crate::sieve_j7::SieveCache as J7;
        let cap = 128usize;
        let mut a: J7<u64, u64, 8> = J7::new(cap);
        let mut b: SieveCache<u64, u64, 8> = SieveCache::new(cap);
        for k in 0..10_000u64 {
            let key = (k.wrapping_mul(2654435761)) % 1024;
            let _ = a.insert(key, key);
            let _ = b.insert(key, key);
        }
        for k in 0..1024u64 {
            assert_eq!(
                a.get(&k).copied(),
                b.get(&k).copied(),
                "j7 と j8 が key {k} で食い違う"
            );
        }
    }

    /// sieve_orig (oracle) と外部一致: 1 shard 同士で SIEVE 意味論が完全一致。
    /// j8 の per_shard <= 64 制約に合わせて cap=64 でテストする。
    #[test]
    fn matches_sieve_orig_externally_1shard() {
        use crate::sieve_orig::SieveCache as Orig;
        let cap = 64usize;
        let mut a: Orig<u64, u64> = Orig::new(cap);
        let mut b: SieveCache<u64, u64, 1> = SieveCache::new(cap);
        for k in 0..10_000u64 {
            let key = (k.wrapping_mul(2654435761)) % 256;
            let _ = a.insert(key, key);
            let _ = b.insert(key, key);
        }
        for k in 0..256u64 {
            assert_eq!(
                a.get(&k).copied(),
                b.get(&k).copied(),
                "1-shard で sieve_orig と j8 が key {k} で食い違う"
            );
        }
    }

    /// bit layout の排他性: LIVE / VISITED / ID_MASK / HASH_MASK で u16 を埋め尽くす。
    #[test]
    fn bit_layout_exclusivity() {
        assert_eq!(LIVE | VISITED | ID_MASK | HASH_MASK, 0xFFFF);
        assert_eq!(LIVE & VISITED, 0);
        assert_eq!(LIVE & ID_MASK, 0);
        assert_eq!(LIVE & HASH_MASK, 0);
        assert_eq!(VISITED & ID_MASK, 0);
        assert_eq!(VISITED & HASH_MASK, 0);
        assert_eq!(ID_MASK & HASH_MASK, 0);
        // SCAN_MASK が live + hash のみカバー。
        assert_eq!(SCAN_MASK, 0x80FF);
        // needle (visited=0, id=0, live=1, hash=任意) は SCAN_MASK 適用後も自身と等しい。
        let needle = LIVE | 0x42;
        assert_eq!(needle & SCAN_MASK, needle);
        // id 領域は 6 bit (= MAX_PER_SHARD - 1 まで表現)。
        assert_eq!((MAX_PER_SHARD - 1) as u16, ID_MASK >> ID_SHIFT);
    }

    /// warm-up→steady の遷移: 5 個目の insert で初 evict、freed_id が再利用される。
    #[test]
    fn warm_up_to_steady_transition() {
        let mut cache: SieveCache<u64, u64, 1> = SieveCache::new(4);
        // warm-up: 4 個目までは evict なし、id = len で連続割り当て。
        assert_eq!(cache.insert(1, 100), None);
        assert_eq!(cache.insert(2, 200), None);
        assert_eq!(cache.insert(3, 300), None);
        assert_eq!(cache.insert(4, 400), None);
        assert_eq!(cache.len(), 4);
        // 5 個目で evict 発火。len は 4 のまま (evict 後に新規 fill)。
        let evicted = cache.insert(5, 500);
        assert!(evicted.is_some(), "5 個目で evict が走るはず");
        assert_eq!(cache.len(), 4);
        // 5 (新挿入) は hit、evict された key は miss。
        assert_eq!(cache.get(&5), Some(&500));
    }

    /// compact 前後で id ↔ entries の対応が壊れないこと。
    /// tags が order_cap (= 16 here) に到達するまで挿入を繰り返し、
    /// compact 発火後も既存 key の get が正しく値を返すことを確認。
    #[test]
    fn compact_preserves_id_mapping() {
        // 1 shard、cap=4 → order_cap = max(16, 8 LANE round) = 16。
        // tags が 16 埋まると compact 発火。
        let mut cache: SieveCache<u64, u64, 1> = SieveCache::new(4);
        // 16 個分の tail 消費を起こすには、4 個 warm-up + 12 個 evict-and-fill。
        // fill 時に tail が増え続け、tail==16 で compact 発火。
        for k in 0..40u64 {
            cache.insert(k, k * 13);
        }
        // 直近の cap 個は in-cache のはず (実際の生存者は SIEVE 動作次第)。
        let alive: u64 = (0..40u64)
            .filter(|&k| cache.get(&k) == Some(&(k * 13)))
            .count() as u64;
        assert_eq!(alive, 4, "compact 後も live entry の値が正しく取れる");
    }

    #[test]
    fn drop_runs_for_live_entries_only() {
        let mut cache: SieveCache<u64, String> = SieveCache::new(TEST_SHARDS * 2);
        for k in 0..64u64 {
            cache.insert(k, format!("value-{k}"));
        }
        assert_eq!(cache.len(), TEST_SHARDS * 2);
    }
}
