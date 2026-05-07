//! J6 — j3 + j5 + M2.1 (visited bit packed into the tag MSB-1).
//!
//! ## 動機 (`docs/improvement-ideas.md` §M2.1)
//!
//! j5 は per-entry に `visited: bool` を inline で持ち、`Entry { key:u64,
//! value:u64, visited:bool }` が 8 B align で 24 B (うち 7 B padding 死)
//! になっていた。memfair レポート (2026-05-05) で「j5 inline 34 B/cap vs
//! orig 25 B/cap」が確定し、padding を取れば -7 B/slot × 2 (entries +
//! tombstone slack) ≈ -14 B/cap でちょうど orig と memory-fair にぶつかる
//! 水準まで縮む。
//!
//! M2.1 はその padding を「visited を tag バイトの bit6 に同居させる」
//! 形で取る案。tag バイトのレイアウトは:
//!
//! ```text
//! bit 7: live (1=occupied, 0=EMPTY/tombstone)
//! bit 6: visited
//! bit 0..5: 6-bit hash
//! ```
//!
//! - `EMPTY = 0x00` (live=0)。tombstone も同じ値。
//! - live + visited=0 の tag 範囲: `0x80..=0xBF` (= 探索用 needle)
//! - live + visited=1 の tag 範囲: `0xC0..=0xFF`
//! - `tags[i] & 0xBF == needle` で visited bit を無視して tag 比較
//! - `tags[i] != 0` で「live か?」 (live は必ず bit7 が立つ)
//! - `tags[i] & 0x40` で「visited か?」
//! - visited セット: `tags[i] |= 0x40` (tags 配列上で in-place、別 cache line を踏まない)
//! - visited クリア: `tags[i] &= !0x40`
//!
//! false-match 確率は j3/j5 の 1/128 から 1/64 に倍増するが、内側の key
//! 等価で必ず弾けるため correctness には効かない。SIMD 比較は `vpand`
//! 1 命令増だけ (32 B 単位で 1 cycle ぶん)。
//!
//! ## j5 との関係
//!
//! - j6 は **standalone**: j3/j5 のコードに依存しない。set-associative の
//!   外側ラッパも内側 1 セグメント実装も全部このファイルで完結する。
//! - shard 選択 / per-shard cap 配分 / hash の bit 配分 (下位ビット =
//!   shard, 上位ビット = tag-hash) は j5 と完全に同じ。よって同じ trace
//!   に対し evict 列・最終 key set は j5 と一致するはず (tag bit 数が
//!   違っても、find は最終的に key 等価で確定するので外部観測上は一致)。
//! - 期待される差分: メモリ -28% (Entry padding 消失) + hit-path -1〜2 ns
//!   程度 (visited RMW が tags 同居になり別 cache line 不要)。
//!
//! ## レイアウト
//!
//! ```text
//! tags:    [u8;            order_cap]   // 並列配列 #1
//! entries: [MaybeUninit;   order_cap]   // 並列配列 #2: Entry { key, value }
//! ```
//!
//! - `order_cap = max(32, ceil_to_32(2 * capacity))` (j3/j5 と同じ)
//! - `Entry` から `visited: bool` が消えているので K=V=u64 で 16 B (padding 0)

use senba::Xxh3Build;
use std::hash::{BuildHasher, Hash};
use std::mem::MaybeUninit;

const EMPTY: u8 = 0;
const LIVE: u8 = 0x80;
const VISITED: u8 = 0x40;
/// visited bit を落とした「tag 比較用マスク」。`tags[i] & TAG_MASK == needle`
/// で visited の有無を問わず tag を比較できる。
const TAG_MASK: u8 = 0xBF;
/// tag 内で hash に使える bit 幅 (= 6 bit)。`hash >> 58` を取って下位 6 bit へ。
const HASH_MASK: u8 = 0x3F;

struct Entry<K, V> {
    key: K,
    value: V,
}

/// 1 shard 分の SIEVE。j3 から `visited: bool` を削り、tag 側に移したもの。
struct Inner<K, V> {
    capacity: usize,
    tags: Vec<u8>,
    entries: Vec<MaybeUninit<Entry<K, V>>>,
    tail: usize,
    hand: usize,
    len: usize,
}

impl<K, V> Inner<K, V>
where
    K: Hash + Eq,
{
    fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        let raw = capacity.checked_mul(2).expect("capacity * 2 overflow");
        // 32 B (AVX2 1 chunk) の倍数に切り上げて末尾 scalar 落ちを避ける。
        // tail を超えた tags は EMPTY=0 で、live tag (>=0x80) と false-match
        // しない不変条件を保つ。
        let order_cap = ((raw + 31) & !31).max(32);
        let mut entries = Vec::with_capacity(order_cap);
        entries.resize_with(order_cap, MaybeUninit::uninit);
        Self {
            capacity,
            tags: vec![EMPTY; order_cap],
            entries,
            tail: 0,
            hand: 0,
            len: 0,
        }
    }

    /// 64-bit hash の上位 6 bit を tag の hash 部分に流し込む。
    /// shard 選択 (下位 log2(SHARDS) bit) と独立 entropy。
    #[inline]
    fn needle_from_hash(hash: u64) -> u8 {
        LIVE | (((hash >> 58) as u8) & HASH_MASK)
    }

    fn find(&self, key: &K, needle: u8) -> Option<usize> {
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                return unsafe { self.find_avx2(key, needle) };
            }
        }
        self.find_scalar(key, needle)
    }

    #[inline]
    fn find_scalar(&self, key: &K, needle: u8) -> Option<usize> {
        for (i, &t) in self.tags[..self.tail].iter().enumerate() {
            if (t & TAG_MASK) == needle {
                // SAFETY: live (bit7) が立っているので entries[i] は init 済み。
                let e = unsafe { self.entries[i].assume_init_ref() };
                if &e.key == key {
                    return Some(i);
                }
            }
        }
        None
    }

    /// AVX2: visited bit を落とした上で tag を broadcast 比較。
    /// `vpand` (mask) → `vpcmpeqb` (cmp) → `vpmovmskb` (mask)。j3 比 +1 命令。
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn find_avx2(&self, key: &K, needle: u8) -> Option<usize> {
        use std::arch::x86_64::*;
        let limit = self.tags.len();
        let tags_ptr = self.tags.as_ptr();
        let entries_ptr = self.entries.as_ptr();
        let needle_v = _mm256_set1_epi8(needle as i8);
        let mask_v = _mm256_set1_epi8(TAG_MASK as i8);

        let mut i = 0usize;
        while i < limit {
            let v = unsafe { _mm256_loadu_si256(tags_ptr.add(i) as *const __m256i) };
            let masked = _mm256_and_si256(v, mask_v);
            let cmp = _mm256_cmpeq_epi8(masked, needle_v);
            let mut mask = _mm256_movemask_epi8(cmp) as u32;
            while mask != 0 {
                let bit = mask.trailing_zeros() as usize;
                let pos = i + bit;
                // SAFETY: needle has bit7=1, so マッチした slot は必ず live。
                let e = unsafe { (*entries_ptr.add(pos)).assume_init_ref() };
                if &e.key == key {
                    return Some(pos);
                }
                mask &= mask - 1;
            }
            i += 32;
        }
        None
    }

    fn contains(&self, key: &K, hash: u64) -> bool {
        self.find(key, Self::needle_from_hash(hash)).is_some()
    }

    fn get(&mut self, key: &K, hash: u64) -> Option<&V> {
        let needle = Self::needle_from_hash(hash);
        let pos = self.find(key, needle)?;
        // visited をセット: tags 配列内 in-place なので、find が触ったキャッシュ
        // ラインに収まる (entries に出ていく必要なし)。これが M2.1 の hit-path
        // 改善の主張点。
        self.tags[pos] |= VISITED;
        // SAFETY: pos は find が tag マッチを確認した位置 (= live)。
        let e = unsafe { self.entries[pos].assume_init_ref() };
        Some(&e.value)
    }

    fn insert(&mut self, key: K, value: V, hash: u64) -> Option<(K, V)> {
        let needle = Self::needle_from_hash(hash);
        if let Some(pos) = self.find(&key, needle) {
            // SAFETY: find が live を確認した。
            let e = unsafe { self.entries[pos].assume_init_mut() };
            e.value = value;
            self.tags[pos] |= VISITED;
            return None;
        }

        let evicted = if self.len == self.capacity {
            self.evict_one()
        } else {
            None
        };

        if self.tail == self.tags.len() {
            self.compact();
        }

        let pos = self.tail;
        self.tail += 1;
        // 新規挿入は visited=0。ちょうど needle と同じバイト値。
        self.tags[pos] = needle;
        // SAFETY: tags[pos] は直前まで EMPTY (= entries[pos] は uninit)。
        self.entries[pos].write(Entry { key, value });
        self.len += 1;

        evicted
    }

    /// SIEVE の victim 探索。j3/j5 と同じ「2 パス + first_live フォールバック」。
    fn evict_one(&mut self) -> Option<(K, V)> {
        if self.len == 0 {
            return None;
        }
        if self.hand >= self.tail {
            self.hand = 0;
        }

        if let Some(pos) = self.scan_evict(self.hand, self.tail) {
            return Some(self.do_evict(pos));
        }
        if let Some(pos) = self.scan_evict(0, self.hand) {
            return Some(self.do_evict(pos));
        }
        let pos = self
            .first_live(self.hand, self.tail)
            .or_else(|| self.first_live(0, self.hand))
            .expect("len > 0 implies at least one live slot");
        Some(self.do_evict(pos))
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

    fn do_evict(&mut self, pos: usize) -> (K, V) {
        debug_assert!(self.tags[pos] != EMPTY);
        // SAFETY: live を呼び出し側で保証済み。
        let entry = unsafe { self.entries[pos].assume_init_read() };
        self.tags[pos] = EMPTY;
        self.len -= 1;
        self.hand = pos + 1;
        if self.hand >= self.tail {
            self.hand = 0;
        }
        (entry.key, entry.value)
    }

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
                // SAFETY: live なので init 済み。write 位置は uninit / 上書き済み。
                // visited bit ごと tag をコピー (= 履歴を保存)。
                let v = unsafe { self.entries[old_pos].assume_init_read() };
                self.entries[write].write(v);
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
        for i in 0..self.tail {
            if self.tags[i] != EMPTY {
                // SAFETY: live ⟹ init 済み。
                unsafe { self.entries[i].assume_init_drop() };
            }
        }
    }
}

// ---------------- 外側 (set-associative wrapper) ----------------

/// j5 と同じ既定。
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
        // 下位ビットで shard 選択。tag は上位 6 bit → 独立 entropy。
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
    //! j3 / j5 のテストミラー + j5 との外部一致性確認。

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

    /// 1024 個入れて全部 hit すること (1/64 false-match でも内側 key 等価で
    /// 必ず弾けることの確認 — j6 は j3/j5 より tag bit が 1 本少ない)。
    #[test]
    fn distinct_keys_with_same_tag_are_separated() {
        // SHARDS=1 にして「全部入る」保証を取る (8 shards だと per-shard で
        // 偏って evict が起きる)。1/64 false-match でも内側 key 等価で必ず
        // 弾けることの確認。
        let n: u64 = 1024;
        let cap: usize = n as usize;
        let mut cache: SieveCache<u64, u64, 1> = SieveCache::new(cap);
        for k in 0..n {
            cache.insert(k, k * 7);
        }
        for k in 0..n {
            assert_eq!(cache.get(&k), Some(&(k * 7)), "miss for key {k}");
        }
    }

    /// j5 と外部から見て同じ振る舞いをすること (= 同じ key set を保持し、
    /// 同じ get 結果を返す)。tag bit 数が違うので tag 比較経路は別だが、
    /// 最終的な find は key 等価で確定するため evict 列は一致するはず。
    #[test]
    fn matches_j5_externally() {
        use crate::experimental::sieve_j5::SieveCache as J5;
        let cap = 128usize;
        let mut a: J5<u64, u64, 8> = J5::new(cap);
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
                "j5 と j6 が key {k} で食い違う"
            );
        }
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
