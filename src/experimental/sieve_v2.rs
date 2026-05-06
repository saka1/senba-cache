//! `sieve_v0` の派生。アルゴリズムは v0 と完全に同一だが、`order` の `Option`
//! ラッパを剥がす。
//!
//! v0 では `order: Vec<Option<EntryId>>` で「dead slot は None」を表現していたが、
//! `tombstone: BitSet` がすでに同じ情報 (qpos が dead か live か) を持っている。
//! `Option<usize>` は alignment で 16B/slot 占有していたのを `usize` 8B にできる
//! (cap=16384, order_cap=32768 なら 256KB → 128KB)。`order[pos]` の値は
//! `!tombstone.get(pos)` のときにのみ意味があり、dead slot 側にはもう書き込まない。
//!
//! eviction loop は v0 と同じ素直な線形スキャン (v1 の bit-parallel は混ぜない)。
//! Option 剥がし「だけ」の効果を v0 と直接比較するための変種。

use crate::sieve_cache::Xxh3Build;
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
    /// dead slot の値は読まれないので未定義 (上書きしない)。
    order: Vec<EntryId>,
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

    fn evict_one(&mut self) -> Option<(K, V)> {
        if self.len == 0 {
            return None;
        }

        if self.hand >= self.tail {
            self.hand = 0;
        }
        loop {
            if self.hand >= self.tail {
                self.hand = 0;
            }
            let pos = self.hand;
            if self.tombstone.get(pos) {
                self.hand += 1;
                continue;
            }
            let eid = self.order[pos];
            if self.visited.get(pos) {
                self.visited.clear(pos);
                self.hand += 1;
                continue;
            }

            // victim
            self.tombstone.set(pos);
            self.visited.clear(pos);
            // order[pos] は触らない。次回 tombstone 越しでしか参照されないので不要。
            self.dead += 1;
            self.len -= 1;
            self.hand += 1;
            if self.hand >= self.tail {
                self.hand = 0;
            }
            let entry = self.entries[eid].take().expect("live slot must have entry");
            self.index.remove(&entry.key);
            self.free_list.push(eid);

            return Some((entry.key, entry.value));
        }
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
