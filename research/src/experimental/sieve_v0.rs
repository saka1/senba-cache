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
        let word_index = index / 64;
        let bit_index = index % 64;
        self.words[word_index] |= 1 << bit_index;
    }

    #[inline]
    fn get(&self, index: usize) -> bool {
        let word_index = index / 64;
        let bit_index = index % 64;
        (self.words[word_index] & (1 << bit_index)) != 0
    }

    #[inline]
    fn clear(&mut self, index: usize) {
        let word_index = index / 64;
        let bit_index = index % 64;
        self.words[word_index] &= !(1 << bit_index);
    }
}

#[derive(Debug)]
struct Entry<K, V> {
    key: K,
    value: V,
    qpos: usize, // order上の現在位置
}

pub struct SieveCache<K, V> {
    capacity: usize,
    index: HashMap<K, EntryId, Xxh3Build>,

    entries: Vec<Option<Entry<K, V>>>,
    free_list: Vec<EntryId>,

    order: Vec<Option<EntryId>>,
    visited: BitSet,
    tombstone: BitSet,

    // logical queue
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
        let order_cap = capacity * 2;
        Self {
            capacity,
            index: HashMap::with_capacity_and_hasher(capacity, Xxh3Build),
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

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn contains_key(&self, key: &K) -> bool {
        self.index.contains_key(key)
    }

    /// hit: visited bit を立てる
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let eid = *self.index.get(key)?;
        let qpos = self.entries[eid].as_ref()?.qpos;
        self.visited.set(qpos);
        Some(&self.entries[eid].as_ref().unwrap().value)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        if let Some(&eid) = self.index.get(&key) {
            // すでに存在するエントリを更新
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
                // すでに死んでいるエントリはスキップ
                self.hand += 1;
                continue;
            }
            let eid = self.order[pos].expect("live slot must have entry id");
            if self.visited.get(pos) {
                // second change
                self.visited.clear(pos);
                self.hand += 1;
                continue;
            }

            // victim
            self.tombstone.set(pos);
            self.visited.clear(pos);
            self.order[pos] = None;
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

        let mut new_order = vec![None; self.order.len()];
        let mut new_visited = BitSet::new(self.order.len());
        //let mut new_tombstone = BitSet::new(self.order.len());

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

    // 新規作成直後はすべてのビットが立っていない
    #[test]
    fn initially_empty() {
        let bs = BitSet::new(128);
        assert!((0..128).all(|i| !bs.get(i)));
    }

    // あるビットをsetすると、そのビットだけが立つ（隣接ビットに波及しない）
    #[test]
    fn set_isolates_target_bit() {
        let mut bs = BitSet::new(64);
        bs.set(7);
        assert!(bs.get(7) && !bs.get(6) && !bs.get(8));
    }

    // 64ビット境界をまたいでもsetとgetが正しく動く
    #[test]
    fn set_spans_word_boundary() {
        let mut bs = BitSet::new(128);
        bs.set(63);
        bs.set(64);
        assert!(bs.get(63) && bs.get(64) && !bs.get(62) && !bs.get(65));
    }

    // 複数のビットを独立してsetできる（先にsetしたビットが消えない）
    #[test]
    fn multiple_sets_are_independent() {
        let mut bs = BitSet::new(256);
        for i in [0, 1, 63, 64, 127, 255] {
            bs.set(i);
        }
        for i in [0, 1, 63, 64, 127, 255] {
            assert!(bs.get(i), "bit {i}");
        }
    }

    // setしたビットをclearすると、そのビットが0に戻る
    #[test]
    fn clear_resets_bit() {
        let mut bs = BitSet::new(64);
        bs.set(7);
        bs.clear(7);
        assert!(!bs.get(7));
    }

    // clearは隣接ビットに影響しない
    #[test]
    fn clear_isolates_target_bit() {
        let mut bs = BitSet::new(64);
        bs.set(6);
        bs.set(7);
        bs.set(8);
        bs.clear(7);
        assert!(!bs.get(7) && bs.get(6) && bs.get(8));
    }

    // 64ビット境界をまたいでもclearが正しく動く
    #[test]
    fn clear_spans_word_boundary() {
        let mut bs = BitSet::new(128);
        bs.set(63);
        bs.set(64);
        bs.clear(63);
        assert!(!bs.get(63) && bs.get(64));
    }

    // すでに0のビットをclearしても副作用がない
    #[test]
    fn clear_already_unset_is_noop() {
        let mut bs = BitSet::new(64);
        bs.set(5);
        bs.clear(3); // 3 は立てていない
        assert!(bs.get(5) && !bs.get(3));
    }

    // SieveCache: 新規作成直後は空で、指定した容量を保持する
    #[test]
    fn cache_initially_empty() {
        let cache: SieveCache<i32, i32> = SieveCache::new(4);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.capacity(), 4);
    }

    // 容量未満でのinsert後にgetすると同じ値が返る
    #[test]
    fn insert_then_get() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(4);
        assert!(cache.insert(1, "a").is_none());
        assert_eq!(cache.get(&1), Some(&"a"));
        assert_eq!(cache.len(), 1);
    }

    // 存在しないキーのgetはNone
    #[test]
    fn get_missing_returns_none() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(4);
        cache.insert(1, "a");
        assert_eq!(cache.get(&2), None);
    }

    // contains_keyは挿入の有無を反映する
    #[test]
    fn contains_key_reflects_insertions() {
        let mut cache: SieveCache<i32, i32> = SieveCache::new(4);
        assert!(!cache.contains_key(&1));
        cache.insert(1, 10);
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
    }

    // 同じキーで再insertすると値が更新され、lenは増えない
    #[test]
    fn insert_existing_key_updates_value() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(4);
        cache.insert(1, "a");
        assert!(cache.insert(1, "b").is_none());
        assert_eq!(cache.get(&1), Some(&"b"));
        assert_eq!(cache.len(), 1);
    }

    // 容量未満のinsertは何も追い出さない
    #[test]
    fn insert_under_capacity_does_not_evict() {
        let mut cache: SieveCache<i32, i32> = SieveCache::new(3);
        assert!(cache.insert(1, 10).is_none());
        assert!(cache.insert(2, 20).is_none());
        assert!(cache.insert(3, 30).is_none());
        assert_eq!(cache.len(), 3);
    }

    // 容量超過時、未visitedな最古エントリが追い出されて (K,V) が返る
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

    // second-chance: getでvisitedになったエントリは1周目を生き残り、次のものが追い出される
    #[test]
    fn visited_entry_survives_first_pass() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(2);
        cache.insert(1, "a");
        cache.insert(2, "b");
        cache.get(&1); // 1 を visited に
        let evicted = cache.insert(3, "c");
        assert_eq!(evicted, Some((2, "b")));
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }

    // 全エントリがvisitedなら、hand がbitを落としつつ周回し最初の未visitedを追い出す
    #[test]
    fn all_visited_clears_bits_then_evicts() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(2);
        cache.insert(1, "a");
        cache.insert(2, "b");
        cache.get(&1);
        cache.get(&2);
        let evicted = cache.insert(3, "c");
        assert_eq!(evicted, Some((1, "a")));
        assert!(!cache.contains_key(&1));
        assert!(cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }

    // 大量insertでcompactionをまたいでも、容量を超えず直近のキーが残る (no get → FIFO)
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

    // 追い出されたキーを再insertすると新規挿入として扱われ、別のエントリが追い出される
    #[test]
    fn reinsert_after_eviction_works() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(2);
        cache.insert(1, "a");
        cache.insert(2, "b");
        cache.insert(3, "c"); // 1 を追い出す
        assert!(!cache.contains_key(&1));
        let evicted = cache.insert(1, "a2");
        assert!(evicted.is_some());
        assert_eq!(cache.get(&1), Some(&"a2"));
        assert_eq!(cache.len(), 2);
    }
}
