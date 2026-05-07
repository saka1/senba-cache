//! J3 — 「Map を捨てる」設計の最小実装。1 セグメント版。
//!
//! 設計メモは `docs/reports/2026-05-04-improvement-ideas.md` の J 章を参照。
//! ここで検証したいのは「外部 HashMap を持たない 1 セグメント SIEVE が、
//! 小容量 (cap ~ 100) で `sieve_orig` より十分速くなれるか」という 1 点。
//! セグメント分割や merge 戦略は本実装の射程外。
//!
//! ## レイアウト
//!
//! ```text
//! tags:    [u8;            N]   // 0 = dead/empty、0x80..=0xFF = live tag
//! entries: [MaybeUninit;   N]   // tags[i] != EMPTY のときだけ初期化済み
//! ```
//!
//! - `N = order_cap = 2 * capacity` (compaction が走るまでの dead 比率上限 ~50%)
//! - tag は `(hash >> 56) | 0x80` の 7-bit、SwissTable と同じ流儀。
//! - **tag 配列が init bitmap を兼ねる**: `tags[i] == EMPTY` ⟺ `entries[i]` は
//!   未初期化。Option を別に持つと不変条件が二重化されるので持たない。
//! - `Entry { key, value, visited: bool }` で visited は **inline** (hit-path が
//!   別 cache line を踏まない、B1 の主張)。
//!
//! ## アルゴリズム
//!
//! - lookup: tags を `[0, tail)` 線形スキャン。x86_64+AVX2 では `vpcmpeqb` +
//!   `vpmovmskb` を明示。tag マッチで内側 key 等価を確認 (false-match ≈ 1/128)。
//! - SIEVE 意味論: 配列順 = 挿入順、`hand` を 0 → tail → 0 に walk。v3 と同じ
//!   「2 パス + first_live フォールバック」で oracle (sieve_orig) と evict 列が一致。
//! - compaction: `tail == order_cap` で全 live を左詰め。`dead = tail - len`、
//!   `order_cap = 2*capacity` なので「dead 比率 50%」も同タイミングで自動到達。
//!   別カウンタは持たない。

use crate::Xxh3Build;
use std::hash::{BuildHasher, Hash};
use std::mem::MaybeUninit;

/// `tags[i] == EMPTY` のとき slot は dead/uninit。
const EMPTY: u8 = 0;

struct Entry<K, V> {
    key: K,
    value: V,
    visited: bool,
}

pub struct SieveCache<K, V> {
    capacity: usize,

    /// 並列配列 #1: live tag (0x80..=0xFF) または EMPTY (0)。init bitmap も兼ねる。
    tags: Vec<u8>,
    /// 並列配列 #2: `tags[i] != EMPTY` のときだけ初期化済み。
    entries: Vec<MaybeUninit<Entry<K, V>>>,

    tail: usize, // 次の挿入位置 (= 物理 watermark)
    hand: usize, // SIEVE hand。tail を超えていたら 0 にラップ。
    len: usize,

    /// 全 variant 共通の BuildHasher (XXH3 統一、`src/hash.rs` 参照)。
    /// 型情報をデータ構造側に持たせて hash 戦略を局所化する。
    hasher: Xxh3Build,
}

impl<K, V> SieveCache<K, V>
where
    K: Hash + Eq,
{
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        // order_cap は **32 の倍数に切り上げ**: AVX2 SIMD が末尾まで scalar 落ち
        // せずに走り切れるようにする。tags[tail..order_cap] は常に EMPTY (= 0)
        // で、live tag は >= 0x80 なので、SIMD が tail 越えて読んでも false match
        // しない不変条件が保たれる。
        let raw = capacity.checked_mul(2).expect("capacity * 2 overflow");
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
            hasher: Xxh3Build,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn contains_key(&self, key: &K) -> bool {
        let tag = self.tag_of(key);
        self.find(key, tag).is_some()
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        let tag = self.tag_of(key);
        self.get_with_tag(key, tag)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        let tag = self.tag_of(&key);
        self.insert_with_tag(key, value, tag)
    }

    // ---------------- hash-aware path (j4 / j5 用) ----------------
    //
    // j4 は shard 選択用に hash を一度計算しているのに、内部 j3 が `tag_of` で
    // もう一度同じ key を hash していて double hash になる (sieve_j4.rs §既知の
    // handicap)。外側で計算した hash の上位 8-bit から作った tag を直接渡す経路
    // を生やして、その固定費をゼロにする。pub(crate) で単スレ前提のまま外に
    // 出さない。

    #[inline]
    pub(crate) fn tag_from_hash(hash: u64) -> u8 {
        // `tag_of` と同じ derivation。shard 選択が下位ビット、tag が上位 8-bit
        // なので独立 entropy を保てる (sieve_j4.rs §bit 配分 と整合)。
        ((hash >> 56) as u8) | 0x80
    }

    #[inline]
    pub(crate) fn contains_with_hash(&self, key: &K, hash: u64) -> bool {
        self.find(key, Self::tag_from_hash(hash)).is_some()
    }

    #[inline]
    pub(crate) fn get_with_hash(&mut self, key: &K, hash: u64) -> Option<&V> {
        self.get_with_tag(key, Self::tag_from_hash(hash))
    }

    #[inline]
    pub(crate) fn insert_with_hash(&mut self, key: K, value: V, hash: u64) -> Option<(K, V)> {
        self.insert_with_tag(key, value, Self::tag_from_hash(hash))
    }

    #[inline]
    fn get_with_tag(&mut self, key: &K, tag: u8) -> Option<&V> {
        let pos = self.find(key, tag)?;
        // SAFETY: find は tags[pos] != EMPTY を確認した位置のみ返すので init 済み。
        let entry = unsafe { self.entries[pos].assume_init_mut() };
        entry.visited = true;
        Some(&entry.value)
    }

    fn insert_with_tag(&mut self, key: K, value: V, tag: u8) -> Option<(K, V)> {
        if let Some(pos) = self.find(&key, tag) {
            // SAFETY: find が tags[pos] != EMPTY を保証。
            let entry = unsafe { self.entries[pos].assume_init_mut() };
            entry.value = value;
            entry.visited = true;
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
        self.tags[pos] = tag;
        // SAFETY: tags[pos] は直前まで EMPTY だった (= entries[pos] は uninit)。
        // write は drop せず上書きなので二重 drop しない。
        self.entries[pos].write(Entry {
            key,
            value,
            visited: false,
        });
        self.len += 1;

        evicted
    }

    // ---------------- internals ----------------

    #[inline]
    fn tag_of(&self, key: &K) -> u8 {
        // 全 variant で hash 戦略を XXH3 に揃える (NSDI'24 リファレンス C 実装と同じ)。
        // tag は 8-bit の rough filter で、false-match は内側の key 等価で必ず弾ける。
        // 上位 8-bit を取り、最上位ビットを立てて live (= != EMPTY) を保証。
        ((self.hasher.hash_one(key) >> 56) as u8) | 0x80
    }

    /// `[0, tail)` を線形スキャンして key にマッチする slot を返す。
    /// x86_64 + AVX2 では明示 SIMD 経路 (`vpcmpeqb` + `vpmovmskb`) を使い、
    /// 32 バイト/iter で tag を broadcast 比較。`is_x86_feature_detected!` の結果は
    /// std 内部でキャッシュされるので、毎呼び出しの overhead は実質ロード 1 回。
    /// マッチ後の inner key 比較は稀 (false-match 確率 ≈ 1/128 + 実 hit)。
    #[inline]
    fn find(&self, key: &K, tag: u8) -> Option<usize> {
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                return unsafe { self.find_avx2(key, tag) };
            }
        }
        self.find_scalar(key, tag)
    }

    #[inline]
    fn find_scalar(&self, key: &K, tag: u8) -> Option<usize> {
        for (i, &t) in self.tags[..self.tail].iter().enumerate() {
            if t == tag {
                // SAFETY: tags[i] != EMPTY なので entries[i] は init 済み。
                let e = unsafe { self.entries[i].assume_init_ref() };
                if &e.key == key {
                    return Some(i);
                }
            }
        }
        None
    }

    /// AVX2 で 32 バイトずつスキャン。
    ///
    /// `order_cap` を 32 の倍数に丸めてあるので **scalar 末尾なし** で走り切れる。
    /// tail を超えた位置は `tags[i] == EMPTY (= 0)` で live tag (>= 0x80) と
    /// マッチし得ないため、tail を超えた SIMD ロードは false-match を起こさない。
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn find_avx2(&self, key: &K, tag: u8) -> Option<usize> {
        use std::arch::x86_64::*;
        let limit = self.tags.len(); // 32 の倍数
        let tags_ptr = self.tags.as_ptr();
        let entries_ptr = self.entries.as_ptr();
        let needle = _mm256_set1_epi8(tag as i8);

        let mut i = 0usize;
        while i < limit {
            let v = unsafe { _mm256_loadu_si256(tags_ptr.add(i) as *const __m256i) };
            let cmp = _mm256_cmpeq_epi8(v, needle);
            let mut mask = _mm256_movemask_epi8(cmp) as u32;
            while mask != 0 {
                let bit = mask.trailing_zeros() as usize;
                let pos = i + bit;
                // SAFETY: SIMD マッチは tag != 0 を確認済み (live tag は >= 0x80)。
                // tags[pos] != EMPTY なので entries[pos] は init 済み。
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

    /// SIEVE の victim 探索。v3 と同じ「2 パス + first_live フォールバック」。
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
        // 全 live が visited → 上の 2 パスで visited を全クリア済み。
        // 残るは「ring 順で最初の live」。
        let pos = self
            .first_live(self.hand, self.tail)
            .or_else(|| self.first_live(0, self.hand))
            .expect("len > 0 implies at least one live slot");
        Some(self.do_evict(pos))
    }

    /// `[lo, hi)` を順に walk: visited を倒しつつ、最初の visited=0 な live slot
    /// を返す。tombstone (tags[i] == EMPTY) はスキップ。
    fn scan_evict(&mut self, lo: usize, hi: usize) -> Option<usize> {
        debug_assert!(lo <= hi && hi <= self.tail);
        for i in lo..hi {
            if self.tags[i] == EMPTY {
                continue;
            }
            // SAFETY: tags[i] != EMPTY なので entries[i] は init 済み。
            let entry = unsafe { self.entries[i].assume_init_mut() };
            if entry.visited {
                entry.visited = false;
            } else {
                return Some(i);
            }
        }
        None
    }

    /// `[lo, hi)` で最初の live slot を返す (visited は無視)。
    fn first_live(&self, lo: usize, hi: usize) -> Option<usize> {
        debug_assert!(lo <= hi && hi <= self.tail);
        (lo..hi).find(|&i| self.tags[i] != EMPTY)
    }

    fn do_evict(&mut self, pos: usize) -> (K, V) {
        debug_assert!(self.tags[pos] != EMPTY);
        // SAFETY: tags[pos] != EMPTY なので init 済み。read 後は uninit 扱い。
        let entry = unsafe { self.entries[pos].assume_init_read() };
        self.tags[pos] = EMPTY;
        self.len -= 1;
        self.hand = pos + 1;
        if self.hand >= self.tail {
            self.hand = 0;
        }
        (entry.key, entry.value)
    }

    /// 全 live を左詰め。tags / entries を物理移動するのみで、外部 Map が
    /// 無いから rehash も Map 書き換えも発生しない (= J 章の主張)。
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
                // SAFETY: tags[old_pos] != EMPTY なので entries[old_pos] は init。
                // entries[write] は (write < old_pos かつ write..old_pos の間に
                // 直前にスキップした EMPTY slot か上書き済み slot しかない)
                // ので、ここは uninit 扱いで write に直接書く。
                // tags[old_pos] は次のループで EMPTY 化する必要はない:
                // self.tail = write 後の Drop は tags[..write] しか見ないため。
                let v = unsafe { self.entries[old_pos].assume_init_read() };
                self.entries[write].write(v);
                self.tags[write] = self.tags[old_pos];
            }
            write += 1;
        }
        // 末尾の残骸 tag を一掃 (entries 側は logically uninit のまま放置で OK)。
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

impl<K, V> Drop for SieveCache<K, V> {
    fn drop(&mut self) {
        // tags が init bitmap。live slot だけ手で drop する。
        for i in 0..self.tail {
            if self.tags[i] != EMPTY {
                // SAFETY: tags[i] != EMPTY ⟹ entries[i] は init 済み。
                unsafe { self.entries[i].assume_init_drop() };
            }
        }
    }
}

impl<K, V> crate::CacheImpl<K, V> for SieveCache<K, V>
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

    // ---- sieve_orig のテストミラー ----

    #[test]
    fn cache_initially_empty() {
        let cache: SieveCache<i32, i32> = SieveCache::new(4);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.capacity(), 4);
        assert!(cache.is_empty());
    }

    #[test]
    fn insert_then_get() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(4);
        assert!(cache.insert(1, "a").is_none());
        assert_eq!(cache.get(&1), Some(&"a"));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn get_missing_returns_none() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(4);
        cache.insert(1, "a");
        assert_eq!(cache.get(&2), None);
    }

    #[test]
    fn contains_key_reflects_insertions() {
        let mut cache: SieveCache<i32, i32> = SieveCache::new(4);
        assert!(!cache.contains_key(&1));
        cache.insert(1, 10);
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
    }

    #[test]
    fn insert_existing_key_updates_value() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(4);
        cache.insert(1, "a");
        assert!(cache.insert(1, "b").is_none());
        assert_eq!(cache.get(&1), Some(&"b"));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn insert_under_capacity_does_not_evict() {
        let mut cache: SieveCache<i32, i32> = SieveCache::new(3);
        assert!(cache.insert(1, 10).is_none());
        assert!(cache.insert(2, 20).is_none());
        assert!(cache.insert(3, 30).is_none());
        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn evicts_oldest_when_full_and_unvisited() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(2);
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
        let mut cache: SieveCache<i32, &str> = SieveCache::new(2);
        cache.insert(1, "a");
        cache.insert(2, "b");
        cache.get(&1);
        let evicted = cache.insert(3, "c");
        assert_eq!(evicted, Some((2, "b")));
    }

    #[test]
    fn all_visited_clears_bits_then_evicts() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(2);
        cache.insert(1, "a");
        cache.insert(2, "b");
        cache.get(&1);
        cache.get(&2);
        let evicted = cache.insert(3, "c");
        assert_eq!(evicted, Some((1, "a")));
    }

    #[test]
    fn churn_keeps_recent_only() {
        let mut cache: SieveCache<i32, i32> = SieveCache::new(3);
        for i in 0..100 {
            cache.insert(i, i * 10);
            assert!(cache.len() <= cache.capacity());
        }
        assert_eq!(cache.len(), 3);
        for i in 97..100 {
            assert_eq!(cache.get(&i), Some(&(i * 10)));
        }
        for i in 0..97 {
            assert!(!cache.contains_key(&i));
        }
    }

    #[test]
    fn reinsert_after_eviction_works() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(2);
        cache.insert(1, "a");
        cache.insert(2, "b");
        cache.insert(3, "c");
        assert!(!cache.contains_key(&1));
        let evicted = cache.insert(1, "a2");
        assert!(evicted.is_some());
        assert_eq!(cache.get(&1), Some(&"a2"));
        assert_eq!(cache.len(), 2);
    }

    // ---- J3 固有: tag 衝突パターン ----

    /// 2 つの異なる key が **同じ tag** を持っても、key 等価チェックで分離されること。
    /// 1024 個入れれば 1/128 衝突率で複数ペアが確実に同じ tag に落ちる。
    #[test]
    fn distinct_keys_with_same_tag_are_separated() {
        let n: u64 = 1024;
        let cap: usize = n as usize; // 全部入る
        let mut cache: SieveCache<u64, u64> = SieveCache::new(cap);
        for k in 0..n {
            cache.insert(k, k * 7);
        }
        for k in 0..n {
            assert_eq!(cache.get(&k), Some(&(k * 7)), "miss for key {k}");
        }
    }

    /// SIEVE の minimal repro (orig と同じ trace でクラッシュしないこと)。
    /// この trace で `evict 列` が orig と一致するかは tests/oracle.rs で検証。
    #[test]
    fn minimal_repro_does_not_panic() {
        let mut cache: SieveCache<u64, u64> = SieveCache::new(3);
        for k in [1u64, 2, 3, 1, 2, 4, 5] {
            let _ = cache.insert(k, k);
        }
        assert!(cache.len() <= 3);
    }

    /// Drop が live slot のみ走ることの間接検証。Vec<String> を入れて leak しないこと
    /// (miri / sanitizer なしでは valgrind 相当の検出はできないが、少なくとも
    /// double-free / panic は起きないこと)。
    #[test]
    fn drop_runs_for_live_entries_only() {
        let mut cache: SieveCache<u64, String> = SieveCache::new(4);
        for k in 0..16u64 {
            cache.insert(k, format!("value-{k}"));
        }
        // 容量超過で 12 個 evict 済み、4 個 live が残っている。
        assert_eq!(cache.len(), 4);
        // ここで cache が drop され、残り 4 entries の String が解放される。
    }
}
