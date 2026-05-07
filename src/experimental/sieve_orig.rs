//! NSDI'24 SIEVE オリジナル実装に忠実な Rust ポート。
//!
//! 参考: external/NSDI24-SIEVE/libCacheSim/libCacheSim/cache/eviction/Sieve.c
//!
//! - 双方向連結リスト (head=新しい / tail=古い)
//! - 各エントリは visited bit (`freq`: 0 or 1) を持つ
//! - 単一の hand ポインタ (`hand`) が tail から prev 方向 (head 側) に進む
//! - 安全 Rust では生ポインタ双方向リストを書きづらいので、
//!   arena (`Vec<MaybeUninit<Node>>` + `free_list`) + `NodeId` で代替。
//!   live/dead は `free_list` が単一の真実源で、Option discriminant の
//!   二重符号化は持たない (`Option<Node>` は u64+u64 ノードで 40B、
//!   MaybeUninit なら 32B)。
//! - リンク (prev/next/head/tail/hand) は原典の `obj_t*` (NULL 可) に倣って、
//!   `NodeId = u32` + sentinel `NIL = u32::MAX` で表現する。`Option<u32>` は
//!   alignment で 8B になり原典の 8B ポインタと同サイズだが、sentinel 表現の方が
//!   「値そのものに NULL 情報が乗る」という C ポインタの性質に近く、
//!   かつ 4B/リンクで詰められる。

use crate::Xxh3Build;
use std::collections::HashMap;
use std::hash::Hash;
use std::mem::MaybeUninit;

type NodeId = u32;
const NIL: NodeId = u32::MAX;

#[derive(Debug)]
struct Node<K, V> {
    key: K,
    value: V,
    freq: u8,
    prev: NodeId, // NIL when no previous link
    next: NodeId, // NIL when no next link
}

pub struct SieveCache<K, V> {
    capacity: usize,
    index: HashMap<K, NodeId, Xxh3Build>,
    /// `free_list` に入っている id のスロットだけが論理的に未初期化。
    /// 他のスロット (= live なノード) は `assume_init_*` で安全にアクセスできる。
    nodes: Vec<MaybeUninit<Node<K, V>>>,
    free_list: Vec<NodeId>,
    head: NodeId, // NIL when list is empty
    tail: NodeId, // NIL when list is empty
    hand: NodeId, // NIL when not yet positioned (= 原典 cache->cache_specific が NULL の状態)
    len: usize,
}

impl<K, V> SieveCache<K, V>
where
    K: Hash + Eq + Clone,
{
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        Self {
            capacity,
            index: HashMap::with_capacity_and_hasher(capacity, Xxh3Build),
            nodes: Vec::with_capacity(capacity),
            free_list: Vec::new(),
            head: NIL,
            tail: NIL,
            hand: NIL,
            len: 0,
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

    /// hit: visited bit (freq) を 1 にする。リスト位置は変更しない。
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let id = *self.index.get(key)?;
        let node = self.node_mut(id);
        node.freq = 1;
        Some(&self.node(id).value)
    }

    /// 既存キーなら値を更新し freq=1 (位置不変)、新規なら head に挿入。
    /// 容量超過のときは 1 件 evict してその (K,V) を返す。
    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        if let Some(&id) = self.index.get(&key) {
            let node = self.node_mut(id);
            node.value = value;
            node.freq = 1;
            return None;
        }

        let evicted = if self.len == self.capacity {
            self.evict_one()
        } else {
            None
        };

        let id = self.alloc_node(Node {
            key: key.clone(),
            value,
            freq: 0,
            prev: NIL,
            next: NIL,
        });
        self.link_at_head(id);
        self.index.insert(key, id);
        self.len += 1;

        evicted
    }

    /// 任意のキーを削除。原典 Sieve_remove_obj に対応し、
    /// hand が削除対象を指していた場合は obj.prev に逃がす。
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let id = self.index.remove(key)?;
        if self.hand == id {
            self.hand = self.node(id).prev;
        }
        self.unlink(id);
        let node = self.free_node(id);
        self.len -= 1;
        Some(node.value)
    }

    // ---------------- internals ----------------

    #[inline]
    fn node(&self, id: NodeId) -> &Node<K, V> {
        // SAFETY: 呼び出し元が `id` が live (= alloc_node 後 / free_node 前) で
        // あることを保証する。本モジュール内では index に登録されている id か、
        // alloc 直後の id しか node()/node_mut() に渡さない。
        unsafe { self.nodes[id as usize].assume_init_ref() }
    }

    #[inline]
    fn node_mut(&mut self, id: NodeId) -> &mut Node<K, V> {
        // SAFETY: 同上。
        unsafe { self.nodes[id as usize].assume_init_mut() }
    }

    fn alloc_node(&mut self, node: Node<K, V>) -> NodeId {
        if let Some(id) = self.free_list.pop() {
            self.nodes[id as usize].write(node);
            id
        } else {
            let id = self.nodes.len() as NodeId;
            self.nodes.push(MaybeUninit::new(node));
            id
        }
    }

    fn free_node(&mut self, id: NodeId) -> Node<K, V> {
        // SAFETY: 呼び出し元が `id` が live なスロットだと保証する
        // (eviction / remove 時に index から取り出した id のみ渡される)。
        // この時点でスロットは論理的に未初期化となり、free_list に積まれて
        // 次の alloc_node で再利用される。
        let node = unsafe { self.nodes[id as usize].assume_init_read() };
        self.free_list.push(id);
        node
    }

    /// 新規ノードを head に prepend。
    fn link_at_head(&mut self, id: NodeId) {
        let old_head = self.head;
        {
            let n = self.node_mut(id);
            n.prev = NIL;
            n.next = old_head;
        }
        if old_head != NIL {
            self.node_mut(old_head).prev = id;
        } else {
            // 空リストだった
            self.tail = id;
        }
        self.head = id;
    }

    /// `id` をリストから外す。head/tail も必要なら更新する。
    fn unlink(&mut self, id: NodeId) {
        let (prev, next) = {
            let n = self.node(id);
            (n.prev, n.next)
        };
        if prev != NIL {
            self.node_mut(prev).next = next;
        } else {
            self.head = next;
        }
        if next != NIL {
            self.node_mut(next).prev = prev;
        } else {
            self.tail = prev;
        }
        let n = self.node_mut(id);
        n.prev = NIL;
        n.next = NIL;
    }

    /// 原典 Sieve_evict (Sieve.c L218-232) を忠実に移植。
    fn evict_one(&mut self) -> Option<(K, V)> {
        // 初回 or 1周完了後は tail から開始
        let mut cur = if self.hand != NIL {
            self.hand
        } else {
            self.tail
        };
        if cur == NIL {
            return None;
        }

        loop {
            let node = self.node_mut(cur);
            if node.freq > 0 {
                node.freq = 0;
                let prev = node.prev;
                cur = if prev != NIL { prev } else { self.tail };
                debug_assert!(cur != NIL, "non-empty list during eviction");
            } else {
                break;
            }
        }

        // victim 確定。次回の hand は victim.prev (削除前に保存)
        self.hand = self.node(cur).prev;

        self.unlink(cur);
        let node = self.free_node(cur);
        self.index.remove(&node.key);
        self.len -= 1;
        Some((node.key, node.value))
    }
}

impl<K, V> Drop for SieveCache<K, V> {
    fn drop(&mut self) {
        // `MaybeUninit` は Drop を自動で走らせないので、live なノードを
        // 連結リスト経由で辿って明示的に落とす。free_list に積まれている
        // スロットは論理的に未初期化なので触らない。
        let mut cur = self.head;
        while cur != NIL {
            // SAFETY: head から next を辿って到達するスロットは live。
            let next = unsafe { self.nodes[cur as usize].assume_init_ref().next };
            unsafe {
                self.nodes[cur as usize].assume_init_drop();
            }
            cur = next;
        }
    }
}

impl<K, V> crate::CacheImpl<K, V> for SieveCache<K, V>
where
    K: Hash + Eq + Clone,
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

    // 新規作成直後は空で、容量を保持する
    #[test]
    fn cache_initially_empty() {
        let cache: SieveCache<i32, i32> = SieveCache::new(4);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.capacity(), 4);
        assert!(cache.is_empty());
    }

    // 容量未満のinsert後にgetすると同じ値が返る
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

    // second-chance: getでvisitedになったエントリは1周目を生き残る
    #[test]
    fn visited_entry_survives_first_pass() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(2);
        cache.insert(1, "a");
        cache.insert(2, "b");
        cache.get(&1); // 1 を visited
        let evicted = cache.insert(3, "c");
        assert_eq!(evicted, Some((2, "b")));
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }

    // 全エントリがvisitedなら、handがbitを落としつつ周回し最初のものを追い出す
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

    // 大量insertでも容量を超えず直近のキーが残る (no get → FIFO 同等)
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

    // 追い出されたキーを再insertすると新規挿入として扱われる
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

    // remove: 存在するキーを削除でき、ない場合は None
    #[test]
    fn remove_returns_value_or_none() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(3);
        cache.insert(1, "a");
        cache.insert(2, "b");
        assert_eq!(cache.remove(&1), Some("a"));
        assert!(!cache.contains_key(&1));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.remove(&999), None);
    }

    // remove で hand が指していたノードを消しても、次の eviction が壊れない。
    // 原典 Sieve_remove_obj (Sieve.c L237-238) の "pointer == obj なら obj.prev に
    // 逃がす" 挙動を直接観測する。
    #[test]
    fn remove_redirects_hand_when_targeting_pointer() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(3);
        // list (head→tail) = [3, 2, 1]
        cache.insert(1, "a");
        cache.insert(2, "b");
        cache.insert(3, "c");
        cache.get(&1);
        cache.get(&2);
        cache.get(&3);
        // insert(4) は tail=1 から走査し全 freq を落として 1 を victim にする。
        // 削除前に hand = 1.prev = 2 が保存される。
        let _ = cache.insert(4, "d");
        let hand_id = cache.hand;
        assert_ne!(hand_id, NIL, "hand should be set after eviction");
        assert_eq!(cache.node(hand_id).key, 2, "hand should track key=2");

        // hand が指す 2 を remove → hand は 2.prev に逃げる必要がある。
        assert_eq!(cache.remove(&2), Some("b"));
        // hand が dangling な 2 のままなら、次の evict 時に解放済みノードを参照して panic する。
        assert!(cache.insert(5, "e").is_none()); // len=2→3
        let ev = cache.insert(6, "f"); // 容量超過で evict 発生
        assert!(ev.is_some());
        assert_eq!(cache.len(), 3);
        assert!(cache.contains_key(&5));
        assert!(cache.contains_key(&6));
    }

    // 既存キーの insert で値・freq は更新されるがリスト先頭への移動は起きない
    #[test]
    fn insert_existing_does_not_move_to_head() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(2);
        cache.insert(1, "a"); // tail 側 (古い)
        cache.insert(2, "b"); // head 側 (新しい)
        // 1 を update — もし誤って head へ移動すると次の evict で 2 が落ちる
        cache.insert(1, "a2");
        // しかし実際は freq=1 になるだけで位置不変。
        // ただし freq=1 は visited entry survives first pass を引き起こすので、
        // 次の挿入で evict されるのは 2 になる。
        let evicted = cache.insert(3, "c");
        assert_eq!(evicted, Some((2, "b")));
        assert!(cache.contains_key(&1));
        assert!(cache.contains_key(&3));
    }

    // hand が head 端に達したあと tail にラップする
    #[test]
    fn hand_wraps_at_head_end() {
        let mut cache: SieveCache<i32, &str> = SieveCache::new(3);
        cache.insert(1, "a");
        cache.insert(2, "b");
        cache.insert(3, "c");
        // visited にするのは head 側 (3) だけ。tail 側 (1) は freq=0 のまま。
        cache.get(&3);
        // 1 件挿入 → tail から走査して最古の未 visited (1) を evict
        let evicted = cache.insert(4, "d");
        assert_eq!(evicted, Some((1, "a")));

        // 状態: list = [4(head), 3(visited), 2] (tail=2)
        // 次の evict では hand = (前の victim 1).prev = NIL なので tail から再開し、
        // 2 (freq=0) が即 victim になる。
        let evicted2 = cache.insert(5, "e");
        assert_eq!(evicted2, Some((2, "b")));

        // 状態: list = [5, 4, 3(visited)] (tail=3)
        // 続けて挿入。3 は visited なので freq を落としつつ通過、
        // hand は head 側に進み、prev=NIL でラップ → tail から再走査。
        let evicted3 = cache.insert(6, "f");
        assert_eq!(evicted3, Some((4, "d")));
        assert!(cache.contains_key(&3));
        assert!(cache.contains_key(&5));
        assert!(cache.contains_key(&6));
    }
}
