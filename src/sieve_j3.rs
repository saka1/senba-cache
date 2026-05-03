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
//! tags:    [u8;   N]   // 並列配列、0 = dead/empty、0x80..=0xFF = live tag
//! entries: [Opt;  N]   // 並列配列、live slot のみ Some(Entry)
//! ```
//!
//! - `N = order_cap = 2 * capacity` (compaction が走るまでの dead 比率上限 ~50%)
//! - tag は `(hash >> 56) | 0x80` で 7-bit、SwissTable と同じ流儀。
//! - 0 を sentinel に使い、live tag は最上位ビット必ず 1 — 1 命令の non-zero 判定で
//!   「live か?」が分かり、SIMD broadcast 比較とも素直に共存する。
//! - `Entry { key, value, visited: bool }` で visited は **inline**。hit-path が
//!   別 cache line を踏まないようにするため (B1 の主張をそのまま採用)。
//!
//! ## アルゴリズム
//!
//! - lookup: tags を `[0, tail)` 線形スキャン。`tags[i] == tag` を満たす i で
//!   `entries[i].key == key` を確認。LLVM が auto-vectorize して `vpcmpeqb` を
//!   出すことを期待し、明示 SIMD は書かない (cap=100 なら scan は数 word)。
//! - SIEVE 意味論: 配列順 = 挿入順、`hand` は配列を 0 → tail → 0 に walk。
//!   v3 と同じ「2 パス + first_live フォールバック」で oracle (sieve_orig) と
//!   evict 列が一致する。
//! - compaction: `tail == order_cap` または `dead >= len` で全 live を左詰め。
//!   外部 Map を持たないので Map 書き換えコストが構造的にゼロ。

use std::hash::{Hash, Hasher};

/// `tags[i] == EMPTY` のとき slot は dead/empty。
const EMPTY: u8 = 0;

/// FxHash 風の弱いハッシャ。tag は 8-bit の rough filter で false-match は内側の
/// key 等価で必ず弾けるので、SipHash の暗号的強度は不要。`u64` キーなら
/// ~3-5ns / op で済み、SipHash の ~15-20ns / op に比べ hot path が大幅に縮む。
#[derive(Default)]
struct FxHasher(u64);

const FX_ROT: u32 = 5;
const FX_SEED: u64 = 0x517c_c1b7_2722_0a95;

impl Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        for &b in bytes {
            h = (h.rotate_left(FX_ROT) ^ b as u64).wrapping_mul(FX_SEED);
        }
        self.0 = h;
    }
    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(FX_ROT) ^ n).wrapping_mul(FX_SEED);
    }
    #[inline]
    fn write_u32(&mut self, n: u32) {
        self.write_u64(n as u64);
    }
    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }
}

#[derive(Debug)]
struct Entry<K, V> {
    key: K,
    value: V,
    visited: bool,
}

pub struct SieveCache<K, V> {
    capacity: usize,

    /// 並列配列 #1: live tag (0x80..=0xFF) または EMPTY (0)。
    tags: Vec<u8>,
    /// 並列配列 #2: live slot のみ Some。
    entries: Vec<Option<Entry<K, V>>>,

    tail: usize, // 次の挿入位置 (= 物理 watermark)
    hand: usize, // SIEVE hand。tail を超えていたら 0 にラップ。
    len: usize,
    dead: usize,
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
        entries.resize_with(order_cap, || None);
        Self {
            capacity,
            tags: vec![EMPTY; order_cap],
            entries,
            tail: 0,
            hand: 0,
            len: 0,
            dead: 0,
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
        let pos = self.find(key, tag)?;
        let entry = self.entries[pos].as_mut().expect("live tag implies Some");
        entry.visited = true;
        Some(&entry.value)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        let tag = self.tag_of(&key);
        if let Some(pos) = self.find(&key, tag) {
            let entry = self.entries[pos].as_mut().expect("live tag implies Some");
            entry.value = value;
            entry.visited = true;
            return None;
        }

        let evicted = if self.len == self.capacity {
            self.evict_one()
        } else {
            None
        };

        self.maybe_compact();

        let pos = self.tail;
        self.tail += 1;
        self.tags[pos] = tag;
        self.entries[pos] = Some(Entry {
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
        let mut h = FxHasher::default();
        key.hash(&mut h);
        let raw = h.finish();
        // 上位 8-bit を取り、最上位ビットを立てて live (= != EMPTY) を保証。
        ((raw >> 56) as u8) | 0x80
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
        let tags = &self.tags[..self.tail];
        for (i, &t) in tags.iter().enumerate() {
            if t == tag {
                if let Some(e) = self.entries[i].as_ref() {
                    if &e.key == key {
                        return Some(i);
                    }
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
    /// 万一の保険として、内側の Option 検査が None を返したら次の候補へ進む
    /// (entries[pos] for pos>=tail は常に None)。
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
                // SAFETY: pos < limit == self.entries.len()。
                if let Some(e) = unsafe { &*entries_ptr.add(pos) }.as_ref() {
                    if &e.key == key {
                        return Some(pos);
                    }
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
    /// を返す。tombstone (tags[i] == 0) はスキップ。
    fn scan_evict(&mut self, lo: usize, hi: usize) -> Option<usize> {
        debug_assert!(lo <= hi && hi <= self.tail);
        for i in lo..hi {
            if self.tags[i] == EMPTY {
                continue;
            }
            let entry = self.entries[i].as_mut().expect("live tag implies Some");
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
        let entry = self.entries[pos].take().expect("victim slot must be live");
        self.tags[pos] = EMPTY;
        self.dead += 1;
        self.len -= 1;
        self.hand = pos + 1;
        if self.hand >= self.tail {
            self.hand = 0;
        }
        (entry.key, entry.value)
    }

    fn maybe_compact(&mut self) {
        if self.tail == self.tags.len() || self.dead >= self.len.max(1) {
            self.compact();
        }
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
                self.tags[write] = self.tags[old_pos];
                self.entries[write] = self.entries[old_pos].take();
            }
            write += 1;
        }
        // 末尾の残骸 (旧 dead slot や移動元) を一掃。
        for i in write..old_tail {
            self.tags[i] = EMPTY;
            self.entries[i] = None;
        }

        self.tail = write;
        self.dead = 0;
        self.hand = if self.len == 0 {
            0
        } else {
            new_hand.unwrap_or(0)
        };
        debug_assert_eq!(self.len, write);
    }
}

impl<K, V> crate::Cache<K, V> for SieveCache<K, V>
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
    /// RandomState seed は実行ごとに変わるので、たくさんの key を入れて衝突を起こす確率
    /// で攻める (1/128 衝突率なので 1000 個入れれば確実に複数ペアが同じ tag に落ちる)。
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
}
