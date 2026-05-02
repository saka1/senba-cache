//! `sieve_v0` の派生。挙動は v0 と同一だが、`evict_one` の線形スキャンを
//! `(visited | tombstone)` の 64bit word 単位の bit-parallel 走査に置き換える。
//!
//! - `visited` / `tombstone` は qpos 空間の packed bitmap (v0 と同じレイアウト)
//! - `find_victim_in_range` は `~(visited | tombstone)` の `trailing_zeros` で
//!   1 ワードあたり最大 64 エントリを 1 命令でスキップする
//! - 通過した範囲の visited は word 単位の `&= !mask` でまとめて落とす
//!
//! 期待される効き場: hand..tail 間に visited 連続帯や tombstone 連続帯が
//! 走るようなワークロード (mid-skew Zipf, churn 後の compaction 直前など)。

use std::collections::HashMap;

type EntryId = usize;

#[derive(Debug, Clone)]
struct BitSet {
    words: Vec<u64>,
}

impl BitSet {
    fn new(nbits: usize) -> Self {
        let num_words = (nbits + 63) / 64;
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
/// `hi <= lo` なら 0。
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
    index: HashMap<K, EntryId>,

    entries: Vec<Option<Entry<K, V>>>,
    free_list: Vec<EntryId>,

    order: Vec<Option<EntryId>>,
    visited: BitSet,
    tombstone: BitSet,

    tail: usize,
    hand: usize,
    len: usize,
    dead: usize,
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
            index: HashMap::with_capacity(capacity),
            entries: Vec::with_capacity(capacity),
            free_list: Vec::new(),

            order: vec![None; order_cap],
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

        self.order[qpos] = Some(eid);
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

    /// `[lo, hi)` の範囲を qpos 昇順に走査して、最初の
    /// `(visited == 0 && tombstone == 0)` な qpos を返す。通過した
    /// visited bit は word 単位でまとめて 0 に落とす (second-chance)。
    /// 見つからなければ `None`。
    fn find_victim_in_range(&mut self, lo: usize, hi: usize) -> Option<usize> {
        debug_assert!(lo <= hi);
        debug_assert!(hi <= self.order.len());
        let mut p = lo;
        while p < hi {
            let w = p / 64;
            let b = p % 64;
            // この word 内で見るべき範囲 [b, end_b)
            let end_b = (hi - w * 64).min(64);

            let combined = self.visited.words[w] | self.tombstone.words[w];
            let valid = bit_range_mask(b, end_b);
            // valid 内で combined=0 のビットが候補
            let candidates = !combined & valid;
            if candidates != 0 {
                let v_bit = candidates.trailing_zeros() as usize;
                let v_pos = w * 64 + v_bit;
                // [b, v_bit) を通過 → その範囲の visited を一括クリア
                let traversed = bit_range_mask(b, v_bit);
                self.visited.words[w] &= !traversed;
                return Some(v_pos);
            }
            // この word 内には victim なし。[b, end_b) の visited を一括クリア。
            let traversed = bit_range_mask(b, end_b);
            self.visited.words[w] &= !traversed;
            p = (w + 1) * 64;
        }
        None
    }

    fn evict_one(&mut self) -> Option<(K, V)> {
        if self.len == 0 {
            return None;
        }
        if self.hand >= self.tail {
            self.hand = 0;
        }

        // [hand, tail) → wrap して [0, hand) で 1 周。ここで visited が全部落ちる。
        // 落ちた後に同じ順 ([hand, tail) → [0, hand)) でもう 1 周することで、
        // v0 のリングスキャン (= hand 起点で全 visited を消したあと、最初に hand
        // 位置の slot を victim に拾う) と一致させる。
        // 単に [0, tail) を 3 段目に走らせると常に slot 0 から拾ってしまうので NG。
        let pos = self
            .find_victim_in_range(self.hand, self.tail)
            .or_else(|| self.find_victim_in_range(0, self.hand))
            .or_else(|| self.find_victim_in_range(self.hand, self.tail))
            .or_else(|| self.find_victim_in_range(0, self.hand))
            .expect("len > 0 must yield a victim after one full visited-clear pass");

        let eid = self.order[pos].expect("victim slot must be live");

        self.tombstone.set(pos);
        self.visited.clear(pos);
        self.order[pos] = None;
        self.dead += 1;
        self.len -= 1;
        self.hand = pos + 1;
        if self.hand >= self.tail {
            self.hand = 0;
        }

        let entry = self.entries[eid].take().expect("victim entry must be live");
        self.index.remove(&entry.key);
        self.free_list.push(eid);

        Some((entry.key, entry.value))
    }

    fn maybe_compact(&mut self) {
        if self.tail == self.order.len() || self.dead >= self.len.max(1) {
            self.compact();
        }
    }

    fn compact(&mut self) {
        let old_tail = self.tail;
        let old_hand = self.hand.min(old_tail);

        let mut new_order = vec![None; self.order.len()];
        let mut new_visited = BitSet::new(self.order.len());

        let mut write = 0usize;
        let mut new_hand: Option<usize> = None;

        for old_pos in 0..old_tail {
            if self.tombstone.get(old_pos) {
                continue;
            }
            let eid = self.order[old_pos].expect("live slot must have entry id");
            if new_hand.is_none() && old_pos >= old_hand {
                new_hand = Some(write);
            }
            new_order[write] = Some(eid);

            if self.visited.get(old_pos) {
                new_visited.set(write);
            }
            let ent = self.entries[eid].as_mut().expect("live slot must have entry");
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

impl<K, V> crate::Cache<K, V> for SieveCache<K, V>
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

    // bit_range_mask のスポット
    #[test]
    fn bit_range_mask_basics() {
        assert_eq!(bit_range_mask(0, 0), 0);
        assert_eq!(bit_range_mask(5, 5), 0);
        assert_eq!(bit_range_mask(0, 1), 1);
        assert_eq!(bit_range_mask(0, 64), !0u64);
        assert_eq!(bit_range_mask(1, 64), !1u64);
        assert_eq!(bit_range_mask(0, 8), 0xFF);
        assert_eq!(bit_range_mask(8, 16), 0xFF00);
        assert_eq!(bit_range_mask(63, 64), 1u64 << 63);
    }

    // 単一 word の途中から走査して、tombstone も visited もない最初の 0 位置を返す
    #[test]
    fn find_victim_in_range_basic() {
        let mut c: SieveCache<i32, i32> = SieveCache::new(4);
        // 仕込む: tail=8, qpos 0..8 の半分を visited にする
        c.tail = 8;
        c.len = 8;
        for i in 0..8 {
            c.order[i] = Some(0);
        }
        // visited: 0,1,2 のみ
        for i in 0..3 {
            c.visited.set(i);
        }
        // 走査開始 = 0 → 最初の未 visited は 3
        assert_eq!(c.find_victim_in_range(0, 8), Some(3));
        // 通過した 0..3 の visited は落ちている
        for i in 0..3 {
            assert!(!c.visited.get(i), "visited[{i}] should be cleared");
        }
    }

    // word 境界をまたいで走査
    #[test]
    fn find_victim_in_range_spans_words() {
        let mut c: SieveCache<i32, i32> = SieveCache::new(80);
        c.tail = 130;
        c.len = 130;
        for i in 0..130 {
            c.order[i] = Some(0);
        }
        // 0..70 まで全部 visited、71 以降は未 visited
        for i in 0..70 {
            c.visited.set(i);
        }
        // hand=10 から走査 → victim は 70
        assert_eq!(c.find_victim_in_range(10, 130), Some(70));
        // 10..70 が落ちている (0..10 はそのまま)
        for i in 0..10 {
            assert!(c.visited.get(i), "visited[{i}] must remain");
        }
        for i in 10..70 {
            assert!(!c.visited.get(i), "visited[{i}] must be cleared");
        }
    }

    // 全部 visited なら None を返し、その範囲の visited を全部落とす
    #[test]
    fn find_victim_returns_none_clears_visited() {
        let mut c: SieveCache<i32, i32> = SieveCache::new(20);
        c.tail = 30;
        c.len = 30;
        for i in 0..30 {
            c.order[i] = Some(0);
            c.visited.set(i);
        }
        assert_eq!(c.find_victim_in_range(0, 30), None);
        for i in 0..30 {
            assert!(!c.visited.get(i));
        }
    }

    // tombstone も victim にならない (skip される)
    #[test]
    fn find_victim_skips_tombstones() {
        let mut c: SieveCache<i32, i32> = SieveCache::new(10);
        c.tail = 10;
        c.len = 7;
        for i in 0..10 {
            c.order[i] = Some(0);
        }
        // 0,1,2 は tombstone (= 死), 3 から live
        for i in 0..3 {
            c.tombstone.set(i);
            c.order[i] = None;
        }
        assert_eq!(c.find_victim_in_range(0, 10), Some(3));
    }

    // ---- 以下は v0 のテストミラー (挙動が同一であることの確認) ----

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
