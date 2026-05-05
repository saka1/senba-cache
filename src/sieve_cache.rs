//! `senba::Cache` — j8 系列を SlotSize 抽象で padding 自動化したライブラリ向け
//! SIEVE 実装。
//!
//! 設計詳細は `docs/reports/2026-05-06-senba-sievecache-design.md`。
//!
//! - 公開型: [`Cache`]`<K, V, S = Slot32, const SHARDS = 8>`
//! - [`SlotSize`] は sealed trait、impl は [`Slot16`] / [`Slot32`] (default) / [`Slot64`]
//! - entries arena の **stride を `S::SIZE` 固定** (= 自動 padding)。
//!   `sizeof(Entry<K, V>) <= S::SIZE` を const-eval で要求し、違反は friendly error
//! - j8 の c-hoist trick (`tag & ID_MASK = id × S::SIZE`) は SLOT 単位で同型に成立、
//!   inner SIMD ループ短縮はそのまま流用
//! - `remove` は per-shard rebuild (swap-to-fill-gap) で warm-up 不変条件 I8 を回復、
//!   free_list を持たない構造を維持
//!
//! ## 不変条件 (j8 と同じ I1〜I8)
//!
//! - I4: live tag の集合 = `{ tags[i] : i < tail, tags[i] & LIVE != 0 }`、個数 = `len`
//! - I5: live tag が指す entry_id の集合は重複なく、サイズ = `len`
//! - I6: I5 集合の id についてのみ `entries[id]` の **`entry` フィールド** が init 済み
//! - I7: I5 集合 ⊆ `0..capacity`
//! - I8: live ids = `0..len` (warm-up 中、および remove 後に swap-to-fill-gap で回復)

use crate::hash::Xxh3Build;
use std::hash::{BuildHasher, Hash};
use std::mem::{ManuallyDrop, MaybeUninit};

const EMPTY: u16 = 0;
const LIVE: u16 = 0x8000;
const VISITED: u16 = 0x4000;
/// AVX2 1 chunk = 32 byte = 16 u16 lane。
const LANE: usize = 16;
/// 6-bit entry_id の構造的上限。per_shard はこの値以下でなければならない。
pub const MAX_PER_SHARD: usize = 64;

// ---------------- SlotSize sealed trait + ZST 札 ----------------

mod sealed {
    pub trait Sealed {}
}

/// entries arena 1 slot 分の stride (byte) を型レベルで指定する sealed trait。
///
/// `S::SIZE` は常に 2 の冪。`Storage<E>` は内部で `#[repr(C)] union` を使い、
/// `entry` フィールドを **オフセット 0** に置くことで、`*const Storage<E>` を
/// `*const E` に reinterpret するだけで `E` に到達できることを保証する。
pub trait SlotSize: sealed::Sealed + 'static {
    /// このブラケットの slot stride (byte)。常に 2 の冪。
    const SIZE: usize;
    /// ブラケットごとの記憶セル型。`size_of::<Storage<E>>() == SIZE` を保つように
    /// 各 impl で union を使って定義する。
    type Storage<E>: Sized;
}

/// `Slot16` ブラケット: stride = 16 byte。
/// `(u32, u32)` `(u64, u64)` 等の小型 primitive 主流ケース。
pub struct Slot16;
/// `Slot32` (default) ブラケット: stride = 32 byte。
/// `(String, V_small)` `(Arc<str>, Arc<str>)` 等の string-cache 主流ケース。
pub struct Slot32;
/// `Slot64` ブラケット: stride = 64 byte。
/// `(String, String)` `(K, V_struct_up_to_56B)` の重量ケース。
pub struct Slot64;

impl sealed::Sealed for Slot16 {}
impl sealed::Sealed for Slot32 {}
impl sealed::Sealed for Slot64 {}

#[repr(C)]
pub union Slot16Storage<E> {
    entry: ManuallyDrop<E>,
    _pad: [u64; 2],
}

#[repr(C)]
pub union Slot32Storage<E> {
    entry: ManuallyDrop<E>,
    _pad: [u64; 4],
}

#[repr(C)]
pub union Slot64Storage<E> {
    entry: ManuallyDrop<E>,
    _pad: [u64; 8],
}

impl SlotSize for Slot16 {
    const SIZE: usize = 16;
    type Storage<E> = Slot16Storage<E>;
}
impl SlotSize for Slot32 {
    const SIZE: usize = 32;
    type Storage<E> = Slot32Storage<E>;
}
impl SlotSize for Slot64 {
    const SIZE: usize = 64;
    type Storage<E> = Slot64Storage<E>;
}

// ---------------- Inner ----------------

struct Entry<K, V> {
    key: K,
    value: V,
}

/// 1 shard 分の SIEVE。j8 の `Inner<K, V>` を `S` で parametrize しただけ。
struct Inner<K, V, S: SlotSize> {
    capacity: usize,
    /// 並列配列 #1: tag 列。`order_cap = 2 × capacity` の LANE 揃え (slack 持ち)。
    tags: Vec<u16>,
    /// 並列配列 #2: entries arena。`capacity` (slack なし)。
    /// id (= tag に埋め込んだ 6 bit) で indexing する。
    /// `S::Storage<Entry<K, V>>` の sizeof は `S::SIZE` 固定 (`_STORAGE_SIZE_OK` で保証)。
    entries: Vec<MaybeUninit<S::Storage<Entry<K, V>>>>,
    /// tags への次挿入位置 (`0..=order_cap`)。
    tail: usize,
    /// SIEVE hand cursor (`0..=tail`)、tags 上を巡回。
    hand: usize,
    /// 現在 live な entry 数 (= live tag 数)。
    len: usize,
}

impl<K, V, S: SlotSize> Inner<K, V, S> {
    /// const-eval: `sizeof(Entry<K, V>) <= S::SIZE`。
    const _SIZE_OK: () = assert!(
        std::mem::size_of::<Entry<K, V>>() <= S::SIZE,
        "senba::Cache: sizeof(Entry<K, V>) exceeds the chosen SlotSize. \
         Try a larger SlotSize (e.g. Slot64)."
    );

    /// const-eval: `Storage<Entry>` の sizeof が `S::SIZE` ピッタリであること。
    /// `Entry` の alignment が 8 を超える (= `repr(align(16))` 等) と union sizeof が
    /// SLOT::SIZE を超えて切り上げられ、c-hoist 不変条件
    /// (`tag & ID_MASK = id × S::SIZE`) が破綻する。これを compile-fail で防ぐ。
    const _STORAGE_SIZE_OK: () = assert!(
        std::mem::size_of::<<S as SlotSize>::Storage<Entry<K, V>>>() == S::SIZE,
        "senba::Cache: SlotStorage size differs from SlotSize::SIZE. \
         (likely caused by Entry alignment > 8 byte)"
    );

    /// id (6 bit) を tag のどの bit 位置に置くか。bit pattern が
    /// **`id × S::SIZE` の値と一致する** ように `log2(S::SIZE)` を取る。
    const ID_SHIFT: u32 = (S::SIZE as u32).trailing_zeros();
    /// id 領域を覆う mask。`tag & ID_MASK = id × S::SIZE`
    /// (= entries 内の byte offset) の関係が成立する。
    const ID_MASK: u16 = ((MAX_PER_SHARD - 1) as u16) << Self::ID_SHIFT;
    /// hash 領域を覆う mask (常にちょうど 8 bit 分散)。
    const HASH_MASK: u16 = 0x3FFF & !Self::ID_MASK;
    /// SIMD scan の比較対象。visited と id を mask out して live + hash のみで突合。
    const SCAN_MASK: u16 = LIVE | Self::HASH_MASK;

    /// tag から id (0..MAX_PER_SHARD) を抽出する。スカラー path / drop / evict 用。
    #[inline]
    fn id_of(tag: u16) -> usize {
        ((tag & Self::ID_MASK) >> Self::ID_SHIFT) as usize
    }

    /// `entries[id]` の **`entry` フィールド** への raw pointer。
    /// `#[repr(C)] union { entry: ManuallyDrop<E>, _pad: [u64; N] }` の
    /// 第一フィールドはオフセット 0 なので `Storage<E>` の先頭 = `E` の先頭。
    /// `MaybeUninit<T>` も同 layout を保つ。
    #[inline]
    fn entry_ptr(&self, id: usize) -> *const Entry<K, V> {
        self.entries[id].as_ptr() as *const Entry<K, V>
    }

    #[inline]
    fn entry_ptr_mut(&mut self, id: usize) -> *mut Entry<K, V> {
        self.entries[id].as_mut_ptr() as *mut Entry<K, V>
    }
}

impl<K, V, S: SlotSize> Inner<K, V, S>
where
    K: Hash + Eq,
{
    fn new(capacity: usize) -> Self {
        // const assert を実体化させる (参照しないと const eval が走らないため)。
        let _: () = Self::_SIZE_OK;
        let _: () = Self::_STORAGE_SIZE_OK;

        assert!(capacity > 0, "capacity must be > 0");
        assert!(
            capacity <= MAX_PER_SHARD,
            "per-shard capacity ({capacity}) must be <= {MAX_PER_SHARD} (6-bit ID limit)"
        );
        let raw = capacity.checked_mul(2).expect("capacity * 2 overflow");
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

    /// 64-bit hash の上位 8 bit を tag の hash 部に流し込む (j8 と同型)。
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
            if (t & Self::SCAN_MASK) == needle {
                let id = Self::id_of(t);
                // SAFETY: live tag が指す id は I5/I6 より init 済み。
                let e = unsafe { &*self.entry_ptr(id) };
                if &e.key == key {
                    return Some(i);
                }
            }
        }
        None
    }

    /// AVX2 + BMI1 で `tags[..]` を SCAN_MASK 一致探索。j8 と同型、
    /// c-hoist trick (`tag & ID_MASK = id × S::SIZE`) は SLOT 単位で成立。
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_avx2(&self, key: &K, needle: u16) -> Option<usize> {
        use std::arch::x86_64::*;
        let limit = self.tags.len();
        let tags_ptr = self.tags.as_ptr();
        let tags_byte_ptr = tags_ptr as *const u8;
        // entries も byte ポインタで持つ。Storage<Entry> の sizeof = S::SIZE 固定なので
        // `tag & ID_MASK = id × S::SIZE` がそのまま byte offset。
        // Storage の第一フィールドは entry (offset 0) なので Entry に直接到達。
        let entries_byte_ptr = self.entries.as_ptr() as *const u8;
        let needle_v = _mm256_set1_epi16(needle as i16);
        let mask_v = _mm256_set1_epi16(Self::SCAN_MASK as i16);
        let id_mask_u32 = Self::ID_MASK as u32;

        let mut i = 0usize;
        while i < limit {
            let v = unsafe { _mm256_loadu_si256(tags_ptr.add(i) as *const __m256i) };
            let masked = _mm256_and_si256(v, mask_v);
            let cmp = _mm256_cmpeq_epi16(masked, needle_v);
            let mut mask = _mm256_movemask_epi8(cmp) as u32;

            let chunk_byte_ptr = unsafe { tags_byte_ptr.add(i * 2) };

            while mask != 0 {
                let bit = mask.trailing_zeros() as usize;
                let tag = unsafe { *(chunk_byte_ptr.add(bit) as *const u16) } as u32;
                let id_bytes = (tag & id_mask_u32) as usize;
                // SAFETY: live needle ⟹ tag live ⟹ entries[id] init (I6)、
                // id_bytes = id × S::SIZE で id < capacity ⟹ 境界内。
                // Storage は `#[repr(C)]` で entry がオフセット 0 → Entry へ直接到達可。
                let entry_ptr = unsafe {
                    entries_byte_ptr.add(id_bytes) as *const Entry<K, V>
                };
                let e = unsafe { &*entry_ptr };
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

    fn contains(&self, key: &K, hash: u64) -> bool {
        self.find(key, Self::needle_from_hash(hash)).is_some()
    }

    fn get(&mut self, key: &K, hash: u64) -> Option<&V> {
        let needle = Self::needle_from_hash(hash);
        let pos = self.find(key, needle)?;
        self.tags[pos] |= VISITED;
        let id = Self::id_of(self.tags[pos]);
        // SAFETY: pos は find が tag マッチを確認した位置 (= live)。
        let e = unsafe { &*self.entry_ptr(id) };
        Some(&e.value)
    }

    fn insert(&mut self, key: K, value: V, hash: u64) -> Option<(K, V)> {
        let needle = Self::needle_from_hash(hash);
        if let Some(pos) = self.find(&key, needle) {
            let id = Self::id_of(self.tags[pos]);
            // SAFETY: find が live を確認した。
            let e = unsafe { &mut *self.entry_ptr_mut(id) };
            e.value = value;
            self.tags[pos] |= VISITED;
            return None;
        }

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
        self.tags[pos] = LIVE | (entry_id << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        // SAFETY: entry_id は warm-up なら未使用、steady なら直前 evict で uninit に
        // 戻った slot。Storage の entry フィールドはオフセット 0 なので raw write。
        unsafe {
            std::ptr::write(self.entry_ptr_mut(entry_id as usize), Entry { key, value });
        }
        self.len += 1;

        evicted
    }

    /// SIEVE の victim 探索 + freed entry_id を返す。
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
        let id = Self::id_of(self.tags[pos]) as u16;
        // SAFETY: live ⟹ entries[id] init (I6)。read 後 entries[id] は uninit。
        let entry = unsafe { std::ptr::read(self.entry_ptr(id as usize)) };
        self.tags[pos] = EMPTY;
        self.len -= 1;
        self.hand = pos + 1;
        if self.hand >= self.tail {
            self.hand = 0;
        }
        ((entry.key, entry.value), id)
    }

    /// tags のみ前詰め。entries arena は不変 (= id-based indexing)。
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

    /// `key` を削除して値を返す。slow-path: O(per_shard) 線形 scan + swap。
    ///
    /// **swap-to-fill-gap**: removed_id を `self.len - 1` (= 最大 live id) と
    /// 物理 swap して I8 (live ids = `0..len`) を回復する。これにより free_list を
    /// 持たない構造を維持し、次回 insert で warm-up branch がそのまま機能する。
    fn remove(&mut self, key: &K, hash: u64) -> Option<V> {
        let needle = Self::needle_from_hash(hash);
        let pos = self.find(key, needle)?;
        let removed_id = Self::id_of(self.tags[pos]);

        // (1) entries[removed_id] から Entry を取り出して破壊、tag を EMPTY に。
        // SAFETY: live ⟹ entries[removed_id] init (I6)。read 後 uninit。
        let entry = unsafe { std::ptr::read(self.entry_ptr(removed_id)) };
        self.tags[pos] = EMPTY;
        self.len -= 1;

        // (2) I8 を回復: max_id (= self.len、self.len-=1 後の最大 live id) を
        //     removed_id に swap で詰める。
        let max_id = self.len;
        if removed_id < max_id {
            // max_id を指す live tag を線形探索 (O(tail) ≤ O(2 × capacity) ≤ O(128))。
            let mut found = false;
            for i in 0..self.tail {
                let t = self.tags[i];
                if t & LIVE != 0 && Self::id_of(t) == max_id {
                    // entries[max_id] → entries[removed_id] へ物理 move。
                    // SAFETY: removed_id != max_id (上の if で保証)、両方 init 済。
                    // read 後 max_id は uninit になり、対応 tag を EMPTY ではなく
                    // id field 更新で「max_id を指していた tag が removed_id を指す」へ書き換え。
                    unsafe {
                        let v = std::ptr::read(self.entry_ptr(max_id));
                        std::ptr::write(self.entry_ptr_mut(removed_id), v);
                    }
                    let cleared = t & !Self::ID_MASK;
                    let new_id_field = (removed_id as u16) << Self::ID_SHIFT;
                    self.tags[i] = cleared | new_id_field;
                    found = true;
                    break;
                }
            }
            debug_assert!(found, "live id {max_id} should be referenced by some live tag");
        }
        // hand カーソルは既存の EMPTY skip ロジックで処理されるので追加処理不要。

        Some(entry.value)
    }
}

impl<K, V, S: SlotSize> Drop for Inner<K, V, S> {
    fn drop(&mut self) {
        // tags scan で live tag を列挙し、id 抽出して entries[id] を drop。
        // I5 (id 重複なし) より同じ entries[id] の二重 drop は起きない。
        for i in 0..self.tail {
            let t = self.tags[i];
            if t != EMPTY {
                let id = Self::id_of(t);
                // SAFETY: live ⟹ entries[id] init (I6)。
                unsafe { std::ptr::drop_in_place(self.entry_ptr_mut(id)) };
            }
        }
    }
}

// ---------------- 公開型 Cache ----------------

pub const DEFAULT_SHARDS: usize = 8;

/// publishable な SIEVE cache。`SlotSize` で entry stride を型レベル指定する。
///
/// ```
/// use senba::Cache;
///
/// // default Slot32: Entry<u64, String> (sizeof=32) で exact fit
/// let mut c: Cache<u64, String> = Cache::new(8);
/// c.insert(1, "hello".into());
/// assert_eq!(c.get(&1), Some(&"hello".to_string()));
/// assert_eq!(c.remove(&1), Some("hello".to_string()));
/// assert_eq!(c.get(&1), None);
/// ```
pub struct Cache<K, V, S: SlotSize = Slot32, const SHARDS: usize = DEFAULT_SHARDS> {
    shards: [Inner<K, V, S>; SHARDS],
    hasher: Xxh3Build,
}

impl<K, V, S, const SHARDS: usize> Cache<K, V, S, SHARDS>
where
    K: Hash + Eq,
    S: SlotSize,
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
        let shards: [Inner<K, V, S>; SHARDS] = std::array::from_fn(|i| {
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

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let h = self.hasher.hash_one(key);
        let i = Self::shard_of_hash(h);
        self.shards[i].remove(key, h)
    }

    #[inline]
    fn shard_of_hash(hash: u64) -> usize {
        (hash as usize) & (SHARDS - 1)
    }
}

// ---------------- tests ----------------

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SHARDS: usize = DEFAULT_SHARDS;

    // sizeof(Entry<u64, u64>) = 16 → Slot16 / Slot32 / Slot64 全部適合。
    // sizeof(Entry<i32, i32>) = 8  → Slot16 / Slot32 / Slot64 全部適合 (slack あり)。

    #[test]
    fn cache_initially_empty() {
        let cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.capacity(), TEST_SHARDS * 4);
        assert!(cache.is_empty());
    }

    #[test]
    fn insert_then_get() {
        let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        assert!(cache.insert(1, 10).is_none());
        assert_eq!(cache.get(&1), Some(&10));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn get_missing_returns_none() {
        let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        cache.insert(1, 10);
        assert_eq!(cache.get(&2), None);
    }

    #[test]
    fn contains_key_reflects_insertions() {
        let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        assert!(!cache.contains_key(&1));
        cache.insert(1, 10);
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
    }

    #[test]
    fn insert_existing_key_updates_value() {
        let mut cache: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        cache.insert(1, 10);
        assert!(cache.insert(1, 20).is_none());
        assert_eq!(cache.get(&1), Some(&20));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn evicts_oldest_when_full_and_unvisited() {
        let mut cache: Cache<u64, u64, Slot32, 1> = Cache::new(2);
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
        let mut cache: Cache<u64, u64, Slot32, 1> = Cache::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((2, 20)));
    }

    #[test]
    fn all_visited_clears_bits_then_evicts() {
        let mut cache: Cache<u64, u64, Slot32, 1> = Cache::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        cache.get(&2);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((1, 10)));
    }

    #[test]
    fn total_capacity_is_respected_under_churn() {
        let cap = TEST_SHARDS * 16;
        let mut cache: Cache<u64, u64> = Cache::new(cap);
        for k in 0..10_000u64 {
            cache.insert(k, k);
            assert!(cache.len() <= cap);
        }
        assert_eq!(cache.len(), cap);
    }

    #[test]
    fn churn_keeps_a_full_capacity_set() {
        let cap = TEST_SHARDS * 16;
        let mut cache: Cache<u64, u64> = Cache::new(cap);
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

    /// bit layout の排他性 — Slot32 (default、Entry<u64,u64>=16) で確認。
    /// Inner<u64, u64, Slot32> の ID_SHIFT = 5、ID_MASK = 0x07e0、HASH_MASK = 0x381f。
    #[test]
    fn bit_layout_exclusivity_slot32() {
        type I = Inner<u64, u64, Slot32>;
        assert_eq!(I::ID_SHIFT, 5);
        assert_eq!(I::ID_MASK, 0x07e0);
        assert_eq!(I::HASH_MASK, 0x381f);
        assert_eq!(I::SCAN_MASK, LIVE | I::HASH_MASK);
        assert_eq!(I::SCAN_MASK, 0xb81f);

        assert_eq!(LIVE | VISITED | I::ID_MASK | I::HASH_MASK, 0xFFFF);
        assert_eq!(LIVE & VISITED, 0);
        assert_eq!(LIVE & I::ID_MASK, 0);
        assert_eq!(LIVE & I::HASH_MASK, 0);
        assert_eq!(VISITED & I::ID_MASK, 0);
        assert_eq!(VISITED & I::HASH_MASK, 0);
        assert_eq!(I::ID_MASK & I::HASH_MASK, 0);

        // c-hoist 不変条件: tag に id を埋めると `tag & ID_MASK = id × S::SIZE`。
        for id in 0..MAX_PER_SHARD {
            let tag_id_field = (id as u16) << I::ID_SHIFT;
            assert_eq!((tag_id_field & I::ID_MASK) as usize, id * Slot32::SIZE);
        }
    }

    #[test]
    fn bit_layout_slot16() {
        type I = Inner<u32, u32, Slot16>;
        assert_eq!(I::ID_SHIFT, 4);
        assert_eq!(I::ID_MASK, 0x03f0);
        assert_eq!(I::HASH_MASK, 0x3c0f);
    }

    #[test]
    fn bit_layout_slot64() {
        type I = Inner<u64, u64, Slot64>;
        assert_eq!(I::ID_SHIFT, 6);
        assert_eq!(I::ID_MASK, 0x0fc0);
        assert_eq!(I::HASH_MASK, 0x303f);
    }

    /// hash spread injectivity for 3 brackets。
    #[test]
    fn needle_spread_is_injective_all_slots() {
        for slot_id in 0..3 {
            let mut seen = std::collections::HashSet::new();
            for h in 0..=255u64 {
                let needle = match slot_id {
                    0 => Inner::<u64, u64, Slot16>::needle_from_hash(h << 56),
                    1 => Inner::<u64, u64, Slot32>::needle_from_hash(h << 56),
                    2 => Inner::<u64, u64, Slot64>::needle_from_hash(h << 56),
                    _ => unreachable!(),
                };
                assert!(seen.insert(needle), "slot {slot_id} hash {h} collides");
            }
            assert_eq!(seen.len(), 256);
        }
    }

    #[test]
    fn slot16_small_entry() {
        // sizeof(Entry<u32, u32>) = 8 ≤ 16
        let mut c: Cache<u32, u32, Slot16> = Cache::new(TEST_SHARDS * 4);
        for k in 0..100u32 {
            c.insert(k, k * 7);
        }
        assert_eq!(c.len(), TEST_SHARDS * 4);
    }

    #[test]
    fn slot32_default_string_value() {
        // sizeof(Entry<u64, String>) = 32 (8 + 24)
        let mut c: Cache<u64, String> = Cache::new(TEST_SHARDS * 2);
        for k in 0..40u64 {
            c.insert(k, format!("v{k}"));
        }
        assert_eq!(c.len(), TEST_SHARDS * 2);
    }

    #[test]
    fn slot64_string_string() {
        // sizeof(Entry<String, String>) = 48 ≤ 64
        let cap = TEST_SHARDS * 2;
        let mut c: Cache<String, String, Slot64> = Cache::new(cap);
        for k in 0..200u64 {
            c.insert(format!("k{k}"), format!("v{k}"));
        }
        assert_eq!(c.len(), cap);
        // 直近に挿入した key は残っているはず (per-shard 内で SIEVE が選別)。
        let alive = (0..200u64)
            .filter(|k| c.get(&format!("k{k}")) == Some(&format!("v{k}")))
            .count();
        assert_eq!(alive, cap);
    }

    #[test]
    fn remove_basic() {
        let mut c: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        c.insert(1, 100);
        c.insert(2, 200);
        c.insert(3, 300);
        assert_eq!(c.remove(&2), Some(200));
        assert_eq!(c.get(&2), None);
        assert_eq!(c.get(&1), Some(&100));
        assert_eq!(c.get(&3), Some(&300));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn remove_missing_returns_none() {
        let mut c: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        c.insert(1, 100);
        assert_eq!(c.remove(&999), None);
        assert_eq!(c.len(), 1);
    }

    /// remove 後に I8 (live ids = 0..len) が回復していて次回 insert で
    /// warm-up branch (`entry_id = self.len`) が機能することを確認。
    #[test]
    fn remove_then_insert_reuses_id() {
        let mut c: Cache<u64, u64, Slot32, 1> = Cache::new(4);
        c.insert(1, 100);
        c.insert(2, 200);
        c.insert(3, 300);
        c.insert(4, 400);
        assert_eq!(c.len(), 4);

        // remove で len=3 に。I8 は swap-to-fill-gap で回復。
        assert_eq!(c.remove(&2), Some(200));
        assert_eq!(c.len(), 3);

        // 続けて 5 個目を insert (warm-up branch、evict なしのはず)。
        assert_eq!(c.insert(5, 500), None);
        assert_eq!(c.len(), 4);

        // 1, 3, 4, 5 が live、2 だけ消えている。
        assert_eq!(c.get(&1), Some(&100));
        assert_eq!(c.get(&2), None);
        assert_eq!(c.get(&3), Some(&300));
        assert_eq!(c.get(&4), Some(&400));
        assert_eq!(c.get(&5), Some(&500));
    }

    /// 末尾 id を remove するケース (swap 不要 path)。
    #[test]
    fn remove_max_id_no_swap() {
        let mut c: Cache<u64, u64, Slot32, 1> = Cache::new(4);
        c.insert(1, 100);
        c.insert(2, 200);
        c.insert(3, 300);
        // 最後に入った key 3 は (warm-up なら) id=2。
        assert_eq!(c.remove(&3), Some(300));
        assert_eq!(c.len(), 2);
        assert_eq!(c.get(&1), Some(&100));
        assert_eq!(c.get(&2), Some(&200));
        assert_eq!(c.get(&3), None);
    }

    /// remove → insert を繰り返しても破綻しない。
    #[test]
    fn remove_insert_churn() {
        let mut c: Cache<u64, u64> = Cache::new(TEST_SHARDS * 4);
        for k in 0..100u64 {
            c.insert(k, k * 11);
        }
        // 偶数 key を全部 remove。
        for k in (0..100u64).step_by(2) {
            let _ = c.remove(&k);
        }
        // 奇数 key だけ残っているはず (cap 内に入る数だけ)。
        let alive: usize = (1..100u64)
            .step_by(2)
            .filter(|k| c.get(k) == Some(&(k * 11)))
            .count();
        assert!(alive > 0);
        // 新規 insert もできる。
        for k in 200..220u64 {
            c.insert(k, k);
        }
        assert!(c.len() <= TEST_SHARDS * 4);
    }

    /// sieve_orig (oracle) と insert/get 列が一致 — 1 shard で SIEVE 意味論検証。
    #[test]
    fn matches_sieve_orig_externally_1shard() {
        use crate::sieve_orig::SieveCache as Orig;
        let cap = 64usize;
        let mut a: Orig<u64, u64> = Orig::new(cap);
        let mut b: Cache<u64, u64, Slot32, 1> = Cache::new(cap);
        for k in 0..10_000u64 {
            let key = (k.wrapping_mul(2654435761)) % 256;
            let _ = a.insert(key, key);
            let _ = b.insert(key, key);
        }
        for k in 0..256u64 {
            assert_eq!(
                a.get(&k).copied(),
                b.get(&k).copied(),
                "1-shard で sieve_orig と senba::Cache が key {k} で食い違う"
            );
        }
    }

    /// 3 ブラケット (Slot16/32/64) で sieve_orig と意味論一致 (`Entry<u64,u64>` は全部適合)。
    #[test]
    fn matches_sieve_orig_per_slot() {
        use crate::sieve_orig::SieveCache as Orig;
        let cap = 32usize;
        let mut oracle: Orig<u64, u64> = Orig::new(cap);
        let mut s16: Cache<u64, u64, Slot16, 1> = Cache::new(cap);
        let mut s32: Cache<u64, u64, Slot32, 1> = Cache::new(cap);
        let mut s64: Cache<u64, u64, Slot64, 1> = Cache::new(cap);
        for k in 0..5_000u64 {
            let key = (k.wrapping_mul(2654435761)) % 128;
            oracle.insert(key, key);
            s16.insert(key, key);
            s32.insert(key, key);
            s64.insert(key, key);
        }
        for k in 0..128u64 {
            let g = oracle.get(&k).copied();
            assert_eq!(s16.get(&k).copied(), g, "Slot16 mismatch key={k}");
            assert_eq!(s32.get(&k).copied(), g, "Slot32 mismatch key={k}");
            assert_eq!(s64.get(&k).copied(), g, "Slot64 mismatch key={k}");
        }
    }

    /// remove を含めた sieve_orig との外部一致 (orig は remove API を持つ)。
    #[test]
    fn remove_during_churn_oracle_match() {
        use crate::sieve_orig::SieveCache as Orig;
        let cap = 32usize;
        let mut a: Orig<u64, u64> = Orig::new(cap);
        let mut b: Cache<u64, u64, Slot32, 1> = Cache::new(cap);
        for k in 0..3_000u64 {
            let key = (k.wrapping_mul(2654435761)) % 128;
            // sieve_orig.insert は (K,V) を返す API か LRU 半順序があるか実装依存。
            // ここでは get/remove を厳密一致確認、insert は両側に流す。
            a.insert(key, key);
            b.insert(key, key);
            // 5 step ごとに remove を混ぜる。
            if k % 5 == 0 {
                let rk = (k.wrapping_mul(11400714819323198485)) % 128;
                let ar = a.remove(&rk);
                let br = b.remove(&rk);
                assert_eq!(ar, br, "remove mismatch step={k} key={rk}");
            }
        }
        for k in 0..128u64 {
            assert_eq!(
                a.get(&k).copied(),
                b.get(&k).copied(),
                "oracle mismatch key={k}"
            );
        }
    }

    #[test]
    #[should_panic]
    fn capacity_below_shards_panics() {
        let _: Cache<u64, u64> = Cache::new(TEST_SHARDS - 1);
    }

    #[test]
    #[should_panic]
    fn per_shard_above_max_panics() {
        let _: Cache<u64, u64, Slot32, 1> = Cache::new(65);
    }

    #[test]
    fn drop_runs_for_live_entries_only() {
        // String value で drop の正当性 (二重 drop / leak の不在) を確認。
        let mut cache: Cache<u64, String> = Cache::new(TEST_SHARDS * 2);
        for k in 0..64u64 {
            cache.insert(k, format!("value-{k}"));
        }
        assert_eq!(cache.len(), TEST_SHARDS * 2);
        // remove も drop の経路。
        for k in 0..16u64 {
            let _ = cache.remove(&k);
        }
        // Cache drop で残りも回収される。
    }
}
