//! `sieve_c9`: per-shard `Mutex<Shard>` で wrap した並行 SIEVE。
//!
//! # 設計の要約
//!
//! - **read / write 両方が Mutex 配下**: `&self` で `get` / `insert` / `contains_key` を
//!   呼べるが、内部は `parking_lot::Mutex<Shard>` を取って senba::Cache (ST) と
//!   同じ shift-on-evict ロジックを直列実行する。
//! - **lock-free 経路 / atomic / seqlock dance / UnsafeCell は登場しない**。
//!   data race UB は構造的に存在せず、miri が走る。
//! - 並列性は SHARDS で決まる。hot key が乗る 1 shard だけ直列化、残り SHARDS-1 は無競合。
//!
//! # API
//!
//! `K: Hash + Eq + Send + Sync, V: Send + Sync` (get で `V: Clone`)。
//! `get` は `Option<V>` を clone で返す (業界主流: moka, mini-moka, quick_cache, jedisct1)。
//!
//! # c8 との比較
//!
//! | | c8 | c9 |
//! |---|---|---|
//! | 値型 | `K, V: Copy` | `K: Hash+Eq+Send+Sync, V: Send+Sync` (get で `V: Clone`) |
//! | 直列版下敷き | j8 (tail/compact あり) | senba::Cache (shift-on-evict、tail/compact なし) |
//! | read path | lock-free seqlock-via-tag | Mutex 配下 ST get |
//! | write path | per-shard Mutex (writer のみ) | per-shard Mutex (read/write 共用) |
//! | 形式的健全性 | 抽象機械上 data race UB | 完全に well-defined |
//!
//! 詳細設計は `docs/reports/2026-05-08-sieve-c9-design.md` 参照。

use parking_lot::Mutex;
use senba::{Stats, Xxh3Build};
use std::hash::{BuildHasher, Hash};
use std::mem::MaybeUninit;

const EMPTY: u16 = 0;
const LIVE: u16 = 0x8000;
const VISITED: u16 = 0x4000;
/// AVX2 1 chunk = 32 byte = 16 u16 lane。
const LANE: usize = 16;
/// 6-bit entry_id の構造的上限。per_shard はこの値以下でなければならない。
pub const MAX_PER_SHARD: usize = 64;

/// `sizeof(Entry)` から ID_SHIFT (= log2(sizeof)) を const-eval で算出。
const fn id_shift_from_entry_size(s: usize) -> u32 {
    assert!(
        s.is_power_of_two(),
        "sieve_c9: sizeof(Entry<K,V>) must be a power of two"
    );
    assert!(s <= 256, "sieve_c9: sizeof(Entry<K,V>) must be <= 256");
    s.trailing_zeros()
}

const fn id_mask_from_shift(id_shift: u32) -> u16 {
    ((MAX_PER_SHARD - 1) as u16) << id_shift
}

const fn hash_mask_from_id_mask(id_mask: u16) -> u16 {
    0x3FFF & !id_mask
}

struct Entry<K, V> {
    key: K,
    value: V,
}

/// One AVX2-load worth of tags. `align(32)` makes the address of every chunk
/// (and therefore the start of the flat `[u16]` view) suitable for `vmovdqa`.
#[repr(C, align(32))]
#[derive(Clone, Copy)]
struct TagsChunk([u16; LANE]);

/// `Vec<TagsChunk>` storage that derefs as a flat `&[u16]` view. Same shape as
/// `senba::shard::AlignedTags`.
struct AlignedTags {
    chunks: Vec<TagsChunk>,
}

impl AlignedTags {
    fn zeroed(order_cap: usize) -> Self {
        debug_assert!(order_cap > 0 && order_cap.is_multiple_of(LANE));
        let n_chunks = order_cap / LANE;
        Self {
            chunks: vec![TagsChunk([EMPTY; LANE]); n_chunks],
        }
    }
}

impl std::ops::Deref for AlignedTags {
    type Target = [u16];
    #[inline]
    fn deref(&self) -> &[u16] {
        let n = self.chunks.len() * LANE;
        // SAFETY: `TagsChunk` is `#[repr(C, align(32))]` over `[u16; LANE]`,
        // so `Vec<TagsChunk>` is layout-equivalent to `[u16; n_chunks*LANE]`.
        unsafe { std::slice::from_raw_parts(self.chunks.as_ptr().cast::<u16>(), n) }
    }
}

impl std::ops::DerefMut for AlignedTags {
    #[inline]
    fn deref_mut(&mut self) -> &mut [u16] {
        let n = self.chunks.len() * LANE;
        // SAFETY: see Deref impl.
        unsafe { std::slice::from_raw_parts_mut(self.chunks.as_mut_ptr().cast::<u16>(), n) }
    }
}

/// 1 shard 分の SIEVE 状態 (senba::Cache の Shard を移植、SlotSize は固定 stride)。
pub(crate) struct Shard<K, V> {
    capacity: usize,
    tags: AlignedTags,
    entries: Vec<MaybeUninit<Entry<K, V>>>,
    hand: usize,
    len: usize,
    hits: u64,
    misses: u64,
    insertions: u64,
    evictions: u64,
}

impl<K, V> Shard<K, V> {
    const ENTRY_SIZE: usize = std::mem::size_of::<Entry<K, V>>();
    const ID_SHIFT: u32 = id_shift_from_entry_size(Self::ENTRY_SIZE);
    const ID_MASK: u16 = id_mask_from_shift(Self::ID_SHIFT);
    const HASH_MASK: u16 = hash_mask_from_id_mask(Self::ID_MASK);
    const SCAN_MASK: u16 = LIVE | Self::HASH_MASK;

    /// Const-eval: `TagsChunk` must be 32-byte aligned for `vmovdqa`.
    const _TAGSCHUNK_ALIGN_OK: () = assert!(
        std::mem::align_of::<TagsChunk>() == 32,
        "sieve_c9: TagsChunk must be 32-byte aligned for vmovdqa"
    );

    #[inline]
    fn id_of(tag: u16) -> usize {
        ((tag & Self::ID_MASK) >> Self::ID_SHIFT) as usize
    }

    #[inline]
    fn entry_ptr(&self, id: usize) -> *const Entry<K, V> {
        self.entries[id].as_ptr()
    }

    #[inline]
    fn entry_ptr_mut(&mut self, id: usize) -> *mut Entry<K, V> {
        self.entries[id].as_mut_ptr()
    }
}

impl<K, V> Shard<K, V>
where
    K: Hash + Eq,
{
    fn new(capacity: usize) -> Self {
        let _: () = Self::_TAGSCHUNK_ALIGN_OK;
        assert!(capacity > 0, "capacity must be > 0");
        assert!(
            capacity <= MAX_PER_SHARD,
            "per-shard capacity ({capacity}) must be <= {MAX_PER_SHARD} (6-bit ID limit)"
        );
        let order_cap = ((capacity + LANE - 1) & !(LANE - 1)).max(LANE);
        let mut entries = Vec::with_capacity(capacity);
        entries.resize_with(capacity, MaybeUninit::uninit);
        Self {
            capacity,
            tags: AlignedTags::zeroed(order_cap),
            entries,
            hand: 0,
            len: 0,
            hits: 0,
            misses: 0,
            insertions: 0,
            evictions: 0,
        }
    }

    /// senba::Cache と同形の hash → tag bit spread。
    #[inline]
    fn needle_from_hash(hash: u64) -> u16 {
        let h = (hash >> 56) as u8;
        let s = Self::ID_SHIFT;
        let spread = if s >= 8 {
            h as u16
        } else {
            let low_mask: u8 = ((1u32 << s) - 1) as u8;
            let low = (h & low_mask) as u16;
            let high = ((h & !low_mask) as u16) << 6;
            low | high
        };
        LIVE | spread
    }

    fn find(&self, key: &K, needle: u16, has_avx2_bmi1: bool) -> Option<usize> {
        #[cfg(target_arch = "x86_64")]
        {
            if has_avx2_bmi1 {
                // SAFETY: `has_avx2_bmi1` was set from `is_x86_feature_detected!("avx2")`
                // at construction; valid for the process lifetime.
                return unsafe { self.find_avx2(key, needle) };
            }
        }
        let _ = has_avx2_bmi1;
        self.find_scalar(key, needle)
    }

    #[inline]
    fn find_scalar(&self, key: &K, needle: u16) -> Option<usize> {
        for (i, &t) in self.tags[..self.len].iter().enumerate() {
            if (t & Self::SCAN_MASK) == needle {
                let id = Self::id_of(t);
                // SAFETY: live tag ⟹ entries[id] initialized.
                let e = unsafe { &*self.entry_ptr(id) };
                if &e.key == key {
                    return Some(i);
                }
            }
        }
        None
    }

    /// AVX2 + BMI1 scan over `tags[..]`. Same shape as senba::Cache::find_avx2.
    ///
    /// # Safety
    /// Caller must ensure host CPU supports AVX2 + BMI1.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_avx2(&self, key: &K, needle: u16) -> Option<usize> {
        use std::arch::x86_64::*;
        let limit = (self.len + LANE - 1) & !(LANE - 1);
        debug_assert!(limit <= self.tags.len());
        let tags_ptr = self.tags.as_ptr();
        debug_assert_eq!(
            (tags_ptr as usize) & 31,
            0,
            "tags storage must be 32-byte aligned for vmovdqa"
        );
        let tags_byte_ptr = tags_ptr as *const u8;
        let entries_byte_ptr = self.entries.as_ptr() as *const u8;
        unsafe {
            let needle_v = _mm256_set1_epi16(needle as i16);
            let mask_v = _mm256_set1_epi16(Self::SCAN_MASK as i16);
            let id_mask_u32 = Self::ID_MASK as u32;

            let mut i = 0usize;
            while i < limit {
                let v = _mm256_load_si256(tags_ptr.add(i) as *const __m256i);
                let masked = _mm256_and_si256(v, mask_v);
                let cmp = _mm256_cmpeq_epi16(masked, needle_v);
                let mut mask = _mm256_movemask_epi8(cmp) as u32;

                let chunk_byte_ptr = tags_byte_ptr.add(i * 2);

                while mask != 0 {
                    let bit = mask.trailing_zeros() as usize;
                    let tag = *(chunk_byte_ptr.add(bit) as *const u16) as u32;
                    let id_bytes = (tag & id_mask_u32) as usize;
                    let entry_ptr = entries_byte_ptr.add(id_bytes) as *const Entry<K, V>;
                    let e = &*entry_ptr;
                    if &e.key == key {
                        let lane = bit >> 1;
                        return Some(i + lane);
                    }
                    mask = _blsr_u32(mask);
                    mask = _blsr_u32(mask);
                }
                i += LANE;
            }
            None
        }
    }

    fn contains(&self, key: &K, hash: u64, has_avx2_bmi1: bool) -> bool {
        self.find(key, Self::needle_from_hash(hash), has_avx2_bmi1)
            .is_some()
    }

    /// Promoting lookup: returns `&V` and sets VISITED on hit.
    fn get(&mut self, key: &K, hash: u64, has_avx2_bmi1: bool) -> Option<&V> {
        let needle = Self::needle_from_hash(hash);
        let pos = match self.find(key, needle, has_avx2_bmi1) {
            Some(p) => {
                self.hits += 1;
                p
            }
            None => {
                self.misses += 1;
                return None;
            }
        };
        self.tags[pos] |= VISITED;
        let id = Self::id_of(self.tags[pos]);
        // SAFETY: pos was confirmed live by find.
        let e = unsafe { &*self.entry_ptr(id) };
        Some(&e.value)
    }

    fn insert(&mut self, key: K, value: V, hash: u64, has_avx2_bmi1: bool) -> Option<(K, V)> {
        self.insertions += 1;
        let needle = Self::needle_from_hash(hash);
        if let Some(pos) = self.find(&key, needle, has_avx2_bmi1) {
            let id = Self::id_of(self.tags[pos]);
            // SAFETY: find confirmed the tag is live.
            let e = unsafe { &mut *self.entry_ptr_mut(id) };
            e.value = value;
            self.tags[pos] |= VISITED;
            return None;
        }

        let (evicted, write_pos, entry_id) = if self.len < self.capacity {
            let pos = self.len;
            let id = self.len as u16;
            self.len += 1;
            (None, pos, id)
        } else {
            self.evictions += 1;
            let pos = self.find_evict_pos();
            let id = Self::id_of(self.tags[pos]) as u16;
            // SAFETY: live ⟹ entries[id] initialized. After read, slot is uninit;
            // re-initialized via ptr::write below.
            let entry = unsafe { std::ptr::read(self.entry_ptr(id as usize)) };

            let last = self.len - 1;
            self.tags.copy_within(pos + 1..self.len, pos);

            self.hand = if pos < last { pos } else { 0 };

            (Some((entry.key, entry.value)), last, id)
        };

        self.tags[write_pos] = LIVE | (entry_id << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        // SAFETY: entry_id is either an unused warm-up slot or one just freed by evict.
        unsafe {
            std::ptr::write(self.entry_ptr_mut(entry_id as usize), Entry { key, value });
        }
        evicted
    }

    fn find_evict_pos(&mut self) -> usize {
        debug_assert!(self.len > 0 && self.len == self.capacity);
        if self.hand >= self.len {
            self.hand = 0;
        }
        self.scan_evict(self.hand, self.len)
            .or_else(|| self.scan_evict(0, self.hand))
            .unwrap_or(self.hand)
    }

    fn scan_evict(&mut self, lo: usize, hi: usize) -> Option<usize> {
        debug_assert!(lo <= hi && hi <= self.len);
        for i in lo..hi {
            let t = self.tags[i];
            debug_assert!(t & LIVE != 0, "tags[0..len] must be LIVE under I4'");
            if t & VISITED != 0 {
                self.tags[i] = t & !VISITED;
            } else {
                return Some(i);
            }
        }
        None
    }

    #[cfg(test)]
    fn live_count(&self) -> usize {
        let mut n = 0;
        for i in 0..self.len {
            if self.tags[i] & LIVE != 0 {
                n += 1;
            }
        }
        n
    }

    #[cfg(test)]
    fn live_ids(&self) -> Vec<usize> {
        let mut ids = Vec::new();
        for i in 0..self.len {
            let t = self.tags[i];
            if t & LIVE != 0 {
                ids.push(Self::id_of(t));
            }
        }
        ids
    }
}

impl<K, V> Drop for Shard<K, V> {
    fn drop(&mut self) {
        for i in 0..self.len {
            let t = self.tags[i];
            debug_assert!(t & LIVE != 0, "tags[0..len] must be LIVE under I4'");
            let id = Self::id_of(t);
            // SAFETY: live ⟹ entries[id] initialized.
            unsafe { std::ptr::drop_in_place(self.entry_ptr_mut(id)) };
        }
    }
}

// ---------------- 外側 (set-associative wrapper) ----------------

/// per-shard `Mutex<Shard>` で wrap した並行 SIEVE。
///
/// `&self` で `get` / `insert` / `contains_key` 等を呼べる。
/// 内部は `parking_lot::Mutex` を 1 shard ずつ取り、senba::Cache (ST) と
/// 同じ shift-on-evict ロジックを直列実行する。
pub struct ConcurrentSieveCache<K, V> {
    shards: Box<[Mutex<Shard<K, V>>]>,
    /// `shards.len() - 1`. Cached so `shard_of_hash` is a single AND.
    shard_mask: usize,
    hasher: Xxh3Build,
    has_avx2_bmi1: bool,
}

impl<K, V> ConcurrentSieveCache<K, V>
where
    K: Hash + Eq + Send + Sync,
    V: Send + Sync,
{
    /// senba::Cache の auto-shard ロジックを継承: shards = next_pow2(ceil(cap / MAX_PER_SHARD))。
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        let n_min = capacity.div_ceil(MAX_PER_SHARD).max(1);
        let shards = n_min.next_power_of_two();
        Self::with_shards(capacity, shards)
    }

    /// shard 数を明示指定する。bench harness 用。
    pub fn with_shards(capacity: usize, shards: usize) -> Self {
        assert!(shards > 0, "shards must be > 0");
        assert!(
            shards.is_power_of_two(),
            "shards ({shards}) must be a power of two so shard select can be a bit mask"
        );
        assert!(
            capacity >= shards,
            "capacity ({capacity}) must be >= shards ({shards}) so each shard has cap >= 1"
        );
        let base = capacity / shards;
        let extra = capacity % shards;
        let built: Vec<Mutex<Shard<K, V>>> = (0..shards)
            .map(|i| {
                let cap_i = base + if i < extra { 1 } else { 0 };
                Mutex::new(Shard::new(cap_i))
            })
            .collect();
        let has_avx2_bmi1 = {
            #[cfg(target_arch = "x86_64")]
            {
                std::is_x86_feature_detected!("avx2")
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                false
            }
        };
        Self {
            shards: built.into_boxed_slice(),
            shard_mask: shards - 1,
            hasher: Xxh3Build,
            has_avx2_bmi1,
        }
    }

    /// 全 shard の capacity 合計。
    pub fn capacity(&self) -> usize {
        self.shards.iter().map(|s| s.lock().capacity).sum()
    }

    /// 全 shard の live entry 数の合計。各 shard の Mutex を 1 回ずつ取って合計するため、
    /// 並列観測の整合性は保証しない (ある shard が読み取り後に更新される可能性あり)。
    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lock().len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.lock().len == 0)
    }

    /// shard 数 (= `shard_mask + 1`)。
    pub fn shards(&self) -> usize {
        self.shard_mask + 1
    }

    /// 全 shard の counter を集約した [`Stats`]。bench 終了時に呼ぶ前提
    /// (各 shard を 1 回ずつ lock するので並列実行中は重い)。
    pub fn stats(&self) -> Stats {
        let mut s = Stats::default();
        for sh in self.shards.iter() {
            let g = sh.lock();
            s.hits += g.hits;
            s.misses += g.misses;
            s.insertions += g.insertions;
            s.evictions += g.evictions;
        }
        s
    }

    pub fn contains_key(&self, key: &K) -> bool {
        let h = self.hasher.hash_one(key);
        let i = self.shard_of_hash(h);
        self.shards[i].lock().contains(key, h, self.has_avx2_bmi1)
    }

    /// `&self` 経由で値を `Option<V>` で返す (clone)。VISITED bit を立てる。
    pub fn get(&self, key: &K) -> Option<V>
    where
        V: Clone,
    {
        let h = self.hasher.hash_one(key);
        let i = self.shard_of_hash(h);
        let mut sh = self.shards[i].lock();
        sh.get(key, h, self.has_avx2_bmi1).cloned()
    }

    pub fn insert(&self, key: K, value: V) -> Option<(K, V)> {
        let h = self.hasher.hash_one(&key);
        let i = self.shard_of_hash(h);
        self.shards[i]
            .lock()
            .insert(key, value, h, self.has_avx2_bmi1)
    }

    #[inline]
    fn shard_of_hash(&self, hash: u64) -> usize {
        (hash as usize) & self.shard_mask
    }

    /// テスト用: 指定 shard の Mutex guard を取って観測する。
    #[cfg(test)]
    fn lock_shard(&self, idx: usize) -> parking_lot::MutexGuard<'_, Shard<K, V>> {
        self.shards[idx].lock()
    }
}

#[cfg(test)]
mod tests {
    //! senba::Cache / sieve_orig の test 群を mirror + 並行 invariants test。

    use super::*;
    use std::sync::Arc;

    #[test]
    fn cache_initially_empty() {
        let cache: ConcurrentSieveCache<i32, i32> = ConcurrentSieveCache::with_shards(64, 8);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.capacity(), 64);
        assert!(cache.is_empty());
        assert_eq!(cache.shards(), 8);
    }

    #[test]
    fn insert_then_get() {
        let cache: ConcurrentSieveCache<i32, i32> = ConcurrentSieveCache::with_shards(64, 8);
        assert!(cache.insert(1, 10).is_none());
        assert_eq!(cache.get(&1), Some(10));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn get_missing_returns_none() {
        let cache: ConcurrentSieveCache<i32, i32> = ConcurrentSieveCache::with_shards(64, 8);
        cache.insert(1, 10);
        assert_eq!(cache.get(&2), None);
    }

    #[test]
    fn contains_key_reflects_insertions() {
        let cache: ConcurrentSieveCache<i32, i32> = ConcurrentSieveCache::with_shards(64, 8);
        assert!(!cache.contains_key(&1));
        cache.insert(1, 10);
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
    }

    #[test]
    fn insert_existing_key_updates_value() {
        let cache: ConcurrentSieveCache<i32, i32> = ConcurrentSieveCache::with_shards(64, 8);
        cache.insert(1, 10);
        assert!(cache.insert(1, 20).is_none());
        assert_eq!(cache.get(&1), Some(20));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn evicts_oldest_when_full_and_unvisited() {
        let cache: ConcurrentSieveCache<i32, i32> = ConcurrentSieveCache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((1, 10)));
        assert_eq!(cache.len(), 2);
        assert!(!cache.contains_key(&1));
        assert!(cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }

    #[test]
    fn visited_entry_survives_first_pass() {
        let cache: ConcurrentSieveCache<i32, i32> = ConcurrentSieveCache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((2, 20)));
    }

    #[test]
    fn all_visited_clears_bits_then_evicts() {
        let cache: ConcurrentSieveCache<i32, i32> = ConcurrentSieveCache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        cache.get(&2);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((1, 10)));
    }

    #[test]
    fn total_capacity_is_respected_under_churn() {
        let cap = 128usize;
        let cache: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::with_shards(cap, 8);
        for k in 0..10_000u64 {
            cache.insert(k, k);
            assert!(cache.len() <= cap);
        }
        assert_eq!(cache.len(), cap);
    }

    #[test]
    fn churn_keeps_a_full_capacity_set() {
        let cap = 128usize;
        let cache: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::with_shards(cap, 8);
        for k in 0..50_000u64 {
            cache.insert(k, k * 3);
        }
        assert_eq!(cache.len(), cap);
        let mut alive = 0;
        for k in 0..50_000u64 {
            if cache.get(&k) == Some(k * 3) {
                alive += 1;
            }
        }
        assert_eq!(alive, cap);
    }

    #[test]
    #[should_panic]
    fn capacity_below_shards_panics() {
        let _: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::with_shards(4, 8);
    }

    #[test]
    #[should_panic]
    fn non_power_of_two_shards_panics() {
        let _: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::with_shards(9, 3);
    }

    #[test]
    #[should_panic]
    fn per_shard_above_max_panics() {
        let _: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::with_shards(65, 1);
    }

    #[test]
    fn per_shard_at_max_works() {
        let cache: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::with_shards(64, 1);
        for k in 0..200u64 {
            cache.insert(k, k * 11);
        }
        assert_eq!(cache.len(), 64);
    }

    #[test]
    fn auto_shard_picks_power_of_two() {
        let cache: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::new(1000);
        assert!(cache.shards().is_power_of_two());
        assert_eq!(cache.capacity(), 1000);
    }

    /// MAX_PER_SHARD まで詰めて全 hit を確認。
    #[test]
    fn distinct_keys_full_per_shard_all_hit() {
        let n: u64 = 64;
        let cache: ConcurrentSieveCache<u64, u64> =
            ConcurrentSieveCache::with_shards(n as usize, 1);
        for k in 0..n {
            cache.insert(k, k * 7);
        }
        for k in 0..n {
            assert_eq!(cache.get(&k), Some(k * 7), "miss for key {k}");
        }
    }

    /// sieve_orig (oracle) と外部一致: 1 shard 同期で SIEVE 意味論完全一致。
    #[test]
    fn matches_sieve_orig_externally_1shard() {
        use crate::experimental::sieve_orig::SieveCache as Orig;
        let cap = 64usize;
        let mut a: Orig<u64, u64> = Orig::new(cap);
        let b: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::with_shards(cap, 1);
        for k in 0..10_000u64 {
            let key = (k.wrapping_mul(2654435761)) % 256;
            let _ = a.insert(key, key);
            let _ = b.insert(key, key);
        }
        for k in 0..256u64 {
            assert_eq!(
                a.get(&k).copied(),
                b.get(&k),
                "1-shard で sieve_orig と c9 が key {k} で食い違う"
            );
        }
    }

    /// senba::Cache (ST) と 1 shard 同期で外部一致: c9 は senba::Cache の shift-on-evict
    /// を移植した形なので、shift-on-evict 内部の挙動が正しいことの直接確認。
    #[test]
    fn matches_senba_cache_externally_1shard() {
        use senba::{Cache, Slot16};
        let cap = 64usize;
        let mut a: Cache<u64, u64, Slot16> = Cache::with_shards(cap, 1);
        let b: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::with_shards(cap, 1);
        for k in 0..10_000u64 {
            let key = (k.wrapping_mul(2654435761)) % 256;
            let _ = a.insert(key, key);
            let _ = b.insert(key, key);
        }
        for k in 0..256u64 {
            assert_eq!(
                a.get(&k).copied(),
                b.get(&k),
                "1-shard で senba::Cache と c9 が key {k} で食い違う"
            );
        }
    }

    #[test]
    fn bit_layout_exclusivity_u64_u64() {
        type S = Shard<u64, u64>;
        assert_eq!(S::ID_SHIFT, 4);
        assert_eq!(S::ID_MASK, 0x03f0);
        assert_eq!(S::HASH_MASK, 0x3c0f);
        assert_eq!(S::SCAN_MASK, LIVE | S::HASH_MASK);

        assert_eq!(LIVE | VISITED | S::ID_MASK | S::HASH_MASK, 0xFFFF);
        assert_eq!(LIVE & VISITED, 0);
        assert_eq!(LIVE & S::ID_MASK, 0);
        assert_eq!(LIVE & S::HASH_MASK, 0);
        assert_eq!(VISITED & S::ID_MASK, 0);
        assert_eq!(VISITED & S::HASH_MASK, 0);
        assert_eq!(S::ID_MASK & S::HASH_MASK, 0);
    }

    #[test]
    fn warm_up_to_steady_transition() {
        let cache: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::with_shards(4, 1);
        assert_eq!(cache.insert(1, 100), None);
        assert_eq!(cache.insert(2, 200), None);
        assert_eq!(cache.insert(3, 300), None);
        assert_eq!(cache.insert(4, 400), None);
        assert_eq!(cache.len(), 4);
        let evicted = cache.insert(5, 500);
        assert!(evicted.is_some());
        assert_eq!(cache.len(), 4);
        assert_eq!(cache.get(&5), Some(500));
    }

    /// V: Clone (non-Copy) でも動くことの確認 (c8 との API 差分を test で固定)。
    #[test]
    fn works_with_string_value() {
        let cache: ConcurrentSieveCache<u64, String> = ConcurrentSieveCache::with_shards(8, 1);
        cache.insert(1, "hello".to_string());
        cache.insert(2, "world".to_string());
        assert_eq!(cache.get(&1), Some("hello".to_string()));
        assert_eq!(cache.get(&2), Some("world".to_string()));
        assert_eq!(cache.get(&3), None);
    }

    #[test]
    fn stats_track_hits_misses_evictions() {
        let cache: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::with_shards(2, 1);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        cache.get(&999);
        cache.insert(3, 30);
        let s = cache.stats();
        assert_eq!(s.hits, 1);
        assert_eq!(s.misses, 1);
        assert_eq!(s.insertions, 3);
        assert_eq!(s.evictions, 1);
    }

    /// 並行 invariants: N thread から Zipf を流して終了後の不変条件のみ検証。
    /// データ競合は Mutex で構造的に排除されるので、c8 のような phantom tag バグは
    /// 原理的に出ない。
    #[test]
    fn concurrent_invariants_under_zipf() {
        use crate::workload::zipf::ZipfGen;
        let cap = 256usize;
        let cache: Arc<ConcurrentSieveCache<u64, u64>> =
            Arc::new(ConcurrentSieveCache::with_shards(cap, 8));

        std::thread::scope(|s| {
            for tid in 0..4u64 {
                let c = Arc::clone(&cache);
                s.spawn(move || {
                    let mut zipf = ZipfGen::new(1.0, 1024, 42 ^ tid);
                    for _ in 0..50_000 {
                        let k = zipf.next().unwrap();
                        if c.get(&k).is_none() {
                            c.insert(k, k);
                        }
                    }
                });
            }
        });

        // I-conc-1: 全体 len <= cap
        let total_len = cache.len();
        assert!(total_len <= cap, "len {total_len} > cap {cap}");

        // I-conc-2 / I-conc-3: 各 shard で LIVE tag 数 == shard.len、id 集合は重複なし
        let mut sum_live = 0;
        for i in 0..8 {
            let sh = cache.lock_shard(i);
            let live = sh.live_count();
            let ids = sh.live_ids();
            assert_eq!(live, ids.len());
            assert_eq!(live, sh.len);
            let mut sorted = ids.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), ids.len(), "shard {i} で id 重複");
            sum_live += live;
        }
        assert_eq!(sum_live, total_len);

        // I-conc-4: get で hit する key の value は key と一致する (= insert 規約)
        for k in 0..1024u64 {
            if let Some(v) = cache.get(&k) {
                assert_eq!(v, k, "key {k} の value が破壊されている");
            }
        }
    }

    /// self-insert → self-get で必ず hit する (visibility test)。
    #[test]
    fn self_insert_self_get_visibility() {
        let cache: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::with_shards(256, 8);
        for k in 0..200u64 {
            cache.insert(k, k * 17);
            assert_eq!(
                cache.get(&k),
                Some(k * 17),
                "直後の self-get で miss: k={k}"
            );
        }
    }
}
