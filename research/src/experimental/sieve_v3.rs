//! v1 (bit-parallel scan) + v2 (Option 剥がし) を合流させた変種。さらに `evict_one`
//! の 4 パス構造を 2 パスに圧縮する。
//!
//! v1 の `evict_one` は `[hand,tail) → [0,hand)` を最大 4 周していた:
//!   pass 1+2: `!combined` (= visited も tombstone も 0) を探しつつ visited を全クリア
//!   pass 3+4: visited が全消えた状態で `!tombstone` の最初を拾う
//! steady state (ほぼ全 visited) では word 読みが 2 倍になっていた。
//!
//! v3 では `find_victim_in_range` を拡張し、同一スイープ中に
//!
//! - `!combined` の最初の qpos (=即 victim)
//! - `!tombstone` の最初の qpos (=visited 全消し後の victim 候補)
//!
//! を同時に記録する。これで pass 3+4 を畳んで最大 2 パスで終わる。
//!
//! `order` の `Option` は v2 と同じ理由で外す: tombstone bitmap が同じ情報を持つ。

use senba::Xxh3Build;
use std::collections::HashMap;

type EntryId = usize;

#[derive(Debug, Clone)]
struct BitSet {
    words: Vec<u64>,
}

impl BitSet {
    fn new(nbits: usize) -> Self {
        let num_words = nbits.div_ceil(64);
        Self {
            words: vec![0; num_words],
        }
    }

    #[inline]
    fn set(&mut self, index: usize) {
        let w = index / 64;
        let b = index % 64;
        self.words[w] |= 1u64 << b;
    }

    #[inline]
    fn get(&self, index: usize) -> bool {
        let w = index / 64;
        let b = index % 64;
        (self.words[w] & (1u64 << b)) != 0
    }

    #[inline]
    fn clear(&mut self, index: usize) {
        let w = index / 64;
        let b = index % 64;
        self.words[w] &= !(1u64 << b);
    }
}

/// 1 ワード内の bit range `[lo, hi)` のマスク。`lo, hi` は 0..=64。
#[inline]
fn bit_range_mask(lo: usize, hi: usize) -> u64 {
    debug_assert!(lo <= 64 && hi <= 64);
    if hi <= lo {
        return 0;
    }
    let high = if hi == 64 { !0u64 } else { (1u64 << hi) - 1 };
    let low = if lo == 0 { 0 } else { (1u64 << lo) - 1 };
    high & !low
}

#[derive(Debug)]
struct Entry<K, V> {
    key: K,
    value: V,
    qpos: usize,
}

pub struct SieveCache<K, V> {
    capacity: usize,
    index: HashMap<K, EntryId, Xxh3Build>,

    entries: Vec<Option<Entry<K, V>>>,
    free_list: Vec<EntryId>,

    /// qpos -> EntryId。`tombstone.get(qpos) == false` のときのみ意味がある。
    order: Vec<EntryId>,
    visited: BitSet,
    tombstone: BitSet,

    tail: usize,
    hand: usize,
    len: usize,
    dead: usize,
}

/// `find_victim_in_range` の結果。
/// - `victim`: `!combined` (= visited も tombstone も 0) を満たす最初の qpos。
///   これがあればそれが即 victim。
/// - `first_live`: `!tombstone` を満たす最初の qpos。`victim` が None で
///   visited が全クリアされた後、これが victim になる。
struct ScanResult {
    victim: Option<usize>,
    first_live: Option<usize>,
}

impl<K, V> SieveCache<K, V>
where
    K: std::hash::Hash + Eq + Clone,
{
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        let order_cap = capacity * 2;
        Self {
            capacity,
            index: HashMap::with_capacity_and_hasher(capacity, Xxh3Build),
            entries: Vec::with_capacity(capacity),
            free_list: Vec::new(),

            order: vec![0; order_cap],
            visited: BitSet::new(order_cap),
            tombstone: BitSet::new(order_cap),

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
        self.index.contains_key(key)
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        let eid = *self.index.get(key)?;
        let qpos = self.entries[eid].as_ref()?.qpos;
        self.visited.set(qpos);
        Some(&self.entries[eid].as_ref().unwrap().value)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        if let Some(&eid) = self.index.get(&key) {
            let qpos = {
                let entry = self.entries[eid].as_mut().unwrap();
                entry.value = value;
                entry.qpos
            };
            self.visited.set(qpos);
            return None;
        }

        let evicted = if self.len == self.capacity {
            self.evict_one()
        } else {
            None
        };

        self.maybe_compact();

        let qpos = self.tail;
        self.tail += 1;

        let eid = self.alloc_entry(Entry {
            key: key.clone(),
            value,
            qpos,
        });

        self.order[qpos] = eid;
        self.visited.clear(qpos);
        self.tombstone.clear(qpos);
        self.index.insert(key, eid);
        self.len += 1;

        evicted
    }

    fn alloc_entry(&mut self, entry: Entry<K, V>) -> EntryId {
        if let Some(eid) = self.free_list.pop() {
            self.entries[eid] = Some(entry);
            eid
        } else {
            let eid = self.entries.len();
            self.entries.push(Some(entry));
            eid
        }
    }

    /// `[lo, hi)` を qpos 昇順にスイープし:
    ///
    /// - `!combined` (visited=0 かつ tombstone=0) の最初の qpos を `victim` に
    /// - `!tombstone` の最初の qpos を `first_live` に
    ///
    /// それぞれ記録する。`victim` が見つかったら、その qpos より前の visited は
    /// word 単位で 0 にクリアされた状態でリターンする。`victim` が見つからな
    /// かった場合 (= 範囲内すべて visited か tombstone) は、範囲全体の visited
    /// が 0 にクリアされた状態でリターンする (これが SIEVE の second-chance)。
    fn find_victim_in_range(&mut self, lo: usize, hi: usize) -> ScanResult {
        debug_assert!(lo <= hi);
        debug_assert!(hi <= self.order.len());
        let mut first_live: Option<usize> = None;
        let mut p = lo;
        while p < hi {
            let w = p / 64;
            let b = p % 64;
            let end_b = (hi - w * 64).min(64);
            let valid = bit_range_mask(b, end_b);

            let visited_w = self.visited.words[w];
            let tomb_w = self.tombstone.words[w];
            let combined = visited_w | tomb_w;

            // victim = !combined & valid の最下位ビット
            let candidates = !combined & valid;
            if candidates != 0 {
                let v_bit = candidates.trailing_zeros() as usize;
                let v_pos = w * 64 + v_bit;

                // first_live がまだなら、この word の [b, v_bit) 区間で探す。
                // ただし v_pos 自身も live なので first_live = victim 位置でも OK。
                if first_live.is_none() {
                    first_live = Some(v_pos);
                }

                // [b, v_bit) を通過 → visited を一括クリア
                let traversed = bit_range_mask(b, v_bit);
                self.visited.words[w] &= !traversed;

                return ScanResult {
                    victim: Some(v_pos),
                    first_live,
                };
            }

            // この word に victim はない。first_live をついでに探す。
            if first_live.is_none() {
                let live_in_word = !tomb_w & valid;
                if live_in_word != 0 {
                    first_live = Some(w * 64 + live_in_word.trailing_zeros() as usize);
                }
            }

            // [b, end_b) の visited を一括クリア
            let traversed = bit_range_mask(b, end_b);
            self.visited.words[w] &= !traversed;
            p = (w + 1) * 64;
        }
        ScanResult {
            victim: None,
            first_live,
        }
    }

    fn evict_one(&mut self) -> Option<(K, V)> {
        if self.len == 0 {
            return None;
        }
        if self.hand >= self.tail {
            self.hand = 0;
        }

        // pass 1: [hand, tail)
        let r1 = self.find_victim_in_range(self.hand, self.tail);
        if let Some(pos) = r1.victim {
            return Some(self.do_evict(pos));
        }
        // pass 2: [0, hand)。終わった時点で [0, tail) の visited は全部 0。
        let r2 = self.find_victim_in_range(0, self.hand);
        if let Some(pos) = r2.victim {
            return Some(self.do_evict(pos));
        }

        // ここまで来たら全 visited を消した。リング順 (hand 起点) で最初の live を取る。
        let pos = r1
            .first_live
            .or(r2.first_live)
            .expect("len > 0 must yield a live slot");
        Some(self.do_evict(pos))
    }

    fn do_evict(&mut self, pos: usize) -> (K, V) {
        let eid = self.order[pos];
        self.tombstone.set(pos);
        self.visited.clear(pos);
        self.dead += 1;
        self.len -= 1;
        self.hand = pos + 1;
        if self.hand >= self.tail {
            self.hand = 0;
        }
        let entry = self.entries[eid].take().expect("victim entry must be live");
        self.index.remove(&entry.key);
        self.free_list.push(eid);
        (entry.key, entry.value)
    }

    fn maybe_compact(&mut self) {
        if self.tail == self.order.len() || self.dead >= self.len.max(1) {
            self.compact();
        }
    }

    fn compact(&mut self) {
        let old_tail = self.tail;
        let old_hand = self.hand.min(old_tail);

        let mut new_order = vec![0; self.order.len()];
        let mut new_visited = BitSet::new(self.order.len());

        let mut write = 0usize;
        let mut new_hand: Option<usize> = None;

        for old_pos in 0..old_tail {
            if self.tombstone.get(old_pos) {
                continue;
            }
            let eid = self.order[old_pos];
            if new_hand.is_none() && old_pos >= old_hand {
                new_hand = Some(write);
            }
            new_order[write] = eid;

            if self.visited.get(old_pos) {
                new_visited.set(write);
            }
            let ent = self.entries[eid]
                .as_mut()
                .expect("live slot must have entry");
            ent.qpos = write;
            write += 1;
        }

        self.order = new_order;
        self.visited = new_visited;
        self.tombstone = BitSet::new(self.order.len());

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

impl<K, V> crate::CacheImpl<K, V> for SieveCache<K, V>
where
    K: std::hash::Hash + Eq + Clone,
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

    #[test]
    fn bit_range_mask_basics() {
        assert_eq!(bit_range_mask(0, 0), 0);
        assert_eq!(bit_range_mask(0, 64), !0u64);
        assert_eq!(bit_range_mask(8, 16), 0xFF00);
    }

    #[test]
    fn scan_finds_victim_and_first_live() {
        let mut c: SieveCache<i32, i32> = SieveCache::new(4);
        c.tail = 8;
        c.len = 8;
        for i in 0..8 {
            c.order[i] = 0;
        }
        // visited: 0,1,2 のみ
        for i in 0..3 {
            c.visited.set(i);
        }
        let r = c.find_victim_in_range(0, 8);
        assert_eq!(r.victim, Some(3));
        assert_eq!(r.first_live, Some(3));
        for i in 0..3 {
            assert!(!c.visited.get(i));
        }
    }

    #[test]
    fn scan_all_visited_returns_none_and_clears() {
        let mut c: SieveCache<i32, i32> = SieveCache::new(20);
        c.tail = 30;
        c.len = 30;
        for i in 0..30 {
            c.order[i] = 0;
            c.visited.set(i);
        }
        let r = c.find_victim_in_range(0, 30);
        assert_eq!(r.victim, None);
        // 全部 visited だったので live は (tombstone=0 の最初) = 0
        assert_eq!(r.first_live, Some(0));
        for i in 0..30 {
            assert!(!c.visited.get(i));
        }
    }

    #[test]
    fn scan_skips_tombstones_for_first_live() {
        let mut c: SieveCache<i32, i32> = SieveCache::new(10);
        c.tail = 10;
        c.len = 7;
        for i in 0..10 {
            c.order[i] = 0;
        }
        for i in 0..3 {
            c.tombstone.set(i);
        }
        // 0..3 が tombstone, 残りは visited も tombstone も 0
        let r = c.find_victim_in_range(0, 10);
        assert_eq!(r.victim, Some(3));
        assert_eq!(r.first_live, Some(3));
    }

    // ---- v0 のテストミラー ----

    #[test]
    fn cache_initially_empty() {
        let cache: SieveCache<i32, i32> = SieveCache::new(4);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.capacity(), 4);
    }

    #[test]
    fn insert_then_get() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(4);
        assert!(cache.insert(1, "a").is_none());
        assert_eq!(cache.get(&1), Some(&"a"));
        assert_eq!(cache.len(), 1);
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
    fn churn_triggers_compaction_and_keeps_recent() {
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
}
