#![cfg(all(target_arch = "x86_64", not(miri)))]
//! `sieve_r3`: RwLock-based concurrent SIEVE variant.
//!
//! 動機: `senba::concurrent::Cache` (lib 0.3.0) が c17s skeleton + `Arc<V>` +
//! `crossbeam-epoch::defer_unchecked` + `Mutex<Box<WriterState>>` で全 48 cell
//! median −34% / worst −63% の退行を出した
//! (`docs/reports/2026-05-13-senba-concurrent-vs-c17s.md`)。退行 signature が
//! Arc strong-count cache-line ping-pong だったため、Arc / epoch / seqlock を
//! まとめて RwLock per-shard に統合し、reader hot path の atomic を最小化する
//! baseline を測るための research variant が r3。
//!
//! # c17s からの構造差分 (削除)
//!
//! - ❌ `Entry::version: AtomicU32` (entry-level seqlock) — RwLock が R/W 排他
//! - ❌ `ShardHot::path_c_epoch: AtomicU64` (coarse seqlock) — 同上
//! - ❌ `tags: Box<[AtomicU16]>` (atomic tag) → plain `Box<[u16]>`
//! - ❌ Path A の lock-free CAS update — write-lock 下で plain in-place
//! - ❌ ManuallyDrop seqlock dance / Probe::Racing / MAX_READER_RETRY
//!
//! # 追加
//!
//! - ✅ `parking_lot::RwLock<ShardInner<K, V>>` per-shard
//! - ✅ `remove()` は entries の **swap-with-last** で compact (write-lock 配下なので
//!   reader race なし)。senba::concurrent の `free_ids` + `next_fresh_id` は lock-free
//!   reader と共存させるための補助構造で、r3 では不要 → invariant は `entries[0..len)`
//!   一直線。
//!
//! # Reader hot path (atomic 数)
//!
//! 1. `RwLock::read()` parking_lot uncontended ~3 atomic
//! 2. plain SIMD scan (tags は AtomicU16 ではない)
//! 3. plain `Entry::value.clone()` (writer 排他)
//! 4. `visited.fetch_or(Relaxed)` 1 atomic (hot-key conditional skip 付き)
//! 5. read-lock release ~1 atomic
//!
//! 合計 ~5 atomic。c17s は ~8-10 atomic (path_c_epoch ×2 + entry.version ×2 +
//! tag Acquire ×1 + visited fetch_or + retry budget)、senba::concurrent は
//! 10+ atomic + Arc strong-count RMW × 2 + `epoch::pin`。
//!
//! # K, V trait bounds
//!
//! - `K: Hash + Eq + Send + Sync`
//! - `V: Clone + Send + Sync`
//!
//! `Sync` は read-lock 下で複数 reader が `&K` / `&V` を同時に持つため必須。
//! `'static` は不要 (epoch deferred drop が無いので)。
//!
//! # 実装 scope
//!
//! 本ファイルは **x86_64 + AVX2 + non-miri 専用** (research artifact)。

use parking_lot::RwLock;
use senba::Xxh3Build;
use std::hash::{BuildHasher, Hash};
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU64, Ordering};

/// EMPTY tag (LIVE OFF)。`tags[len..]` 領域および remove 直後の trailing slot に置く。
const EMPTY: u16 = 0;
/// LIVE bit (bit 15)。tag が有効な entry を指していることを示す。
const LIVE: u16 = 0x8000;
/// AVX2 1 chunk = 32 byte = 16 u16 lane。
const LANE: usize = 16;
/// 6-bit entry_id の構造的上限。per_shard はこの値以下でなければならない。
pub const MAX_PER_SHARD: usize = 64;

/// `sizeof(Entry)` から ID_SHIFT を const-eval で算出 (c17s と同形)。
const fn id_shift_from_entry_size(s: usize) -> u32 {
    assert!(
        s.is_power_of_two(),
        "sieve_r3: sizeof(Entry<K,V>) must be a power of two"
    );
    assert!(s <= 256, "sieve_r3: sizeof(Entry<K,V>) must be <= 256");
    s.trailing_zeros()
}

const fn id_mask_from_shift(id_shift: u32) -> u16 {
    ((MAX_PER_SHARD - 1) as u16) << id_shift
}

/// HASH 領域は 0x7FFF (LIVE のみ除外、c17s と同じ) から ID 6 bit を抜いたもの。
const fn hash_mask_from_id_mask(id_mask: u16) -> u16 {
    0x7FFF & !id_mask
}

/// Path A の lock-free seqlock を持たないので version field は不要。`#[repr(C, align(16))]`
/// で u64+u64 = 16 (ID_SHIFT=4)、u64+String = 32 (ID_SHIFT=5) と power-of-2 を保つ。
#[repr(C, align(16))]
struct Entry<K, V> {
    key: K,
    value: V,
}

/// Per-shard mutable state。RwLock 配下に置き、reader は read-lock で plain 参照、
/// writer は write-lock で plain mutate する。`visited` のみ AtomicU64 (read-lock 下で
/// 複数 reader が `fetch_or` を撃つため)。
///
/// **Invariant**: `entries[0..len)` は全 init、`entries[len..cap)` は全 uninit。
/// Path B (warmup) は `entry_id = len` で sequential 払出し、Path C (evict) は
/// `evict_id` を inline 再利用、`remove()` は swap-with-last で compact することで
/// この invariant を保つ。
struct ShardInner<K, V> {
    capacity: u16,
    len: u16,
    hand: u16,
    /// SIMD scan target。長さは `((cap + LANE - 1) & !(LANE-1)).max(LANE)`、tail は EMPTY。
    tags: Box<[u16]>,
    /// length = capacity。`entries[id]` の init/uninit 状態は writer のみが管理。
    entries: Box<[MaybeUninit<Entry<K, V>>]>,
    /// per-pos visited bit。reader が hit 時に `fetch_or(Relaxed)`、writer が eviction で
    /// `fetch_and` / `fetch_or` する。write-lock 下では `Relaxed` で十分。
    visited: AtomicU64,
}

/// 1 shard 分の並行 SIEVE。
#[repr(C, align(64))]
pub struct Shard<K, V> {
    rw: RwLock<ShardInner<K, V>>,
}

// SAFETY: RwLock が R/W 排他を保証する。reader 群は read-lock 下で `&Entry<K,V>` を共有
// するため K, V とも `Sync` 要件 (各 method 側の trait bound で課す)。writer は write-lock
// で exclusive access、entries の init/uninit を維持し V::clone race を起こさない。
unsafe impl<K: Send + Sync, V: Send + Sync> Send for Shard<K, V> {}
unsafe impl<K: Send + Sync, V: Send + Sync> Sync for Shard<K, V> {}

impl<K, V> Shard<K, V> {
    const ENTRY_SIZE: usize = std::mem::size_of::<Entry<K, V>>();
    const ID_SHIFT: u32 = id_shift_from_entry_size(Self::ENTRY_SIZE);
    const ID_MASK: u16 = id_mask_from_shift(Self::ID_SHIFT);
    const HASH_MASK: u16 = hash_mask_from_id_mask(Self::ID_MASK);
    /// reader needle 比較用。LIVE + HASH、ID は除外。
    const SCAN_MASK: u16 = LIVE | Self::HASH_MASK;

    #[inline]
    fn id_of(tag: u16) -> usize {
        ((tag & Self::ID_MASK) >> Self::ID_SHIFT) as usize
    }

    #[inline]
    fn vbit_mask(pos: usize) -> u64 {
        debug_assert!(pos < 64, "vbit_mask: pos {pos} >= 64 (per-shard cap limit)");
        1u64 << pos
    }

    pub fn capacity(&self) -> usize {
        self.rw.read().capacity as usize
    }

    pub fn len(&self) -> usize {
        self.rw.read().len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<K, V> Shard<K, V>
where
    K: Hash + Eq + Send + Sync,
    V: Clone + Send + Sync,
{
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        assert!(
            capacity <= MAX_PER_SHARD,
            "per-shard capacity ({capacity}) must be <= {MAX_PER_SHARD} (6-bit ID limit)"
        );
        assert!(
            std::is_x86_feature_detected!("avx2"),
            "sieve_r3: AVX2 required (research artifact); compile-time gated to x86_64+non-miri but runtime CPU lacks AVX2"
        );
        let order_cap = ((capacity + LANE - 1) & !(LANE - 1)).max(LANE);
        let tags = vec![EMPTY; order_cap].into_boxed_slice();
        let mut entries_vec: Vec<MaybeUninit<Entry<K, V>>> = Vec::with_capacity(capacity);
        entries_vec.resize_with(capacity, MaybeUninit::uninit);
        let inner = ShardInner {
            capacity: capacity as u16,
            len: 0,
            hand: 0,
            tags,
            entries: entries_vec.into_boxed_slice(),
            visited: AtomicU64::new(0),
        };
        Self {
            rw: RwLock::new(inner),
        }
    }

    /// hash → tag bit spread (c17s と同型)。HASH は 9 bit、ID 切れ目を跨いで spread。
    #[inline]
    fn needle_from_hash(hash: u64) -> u16 {
        let h9 = ((hash >> 55) as u16) & 0x01FF;
        let s = Self::ID_SHIFT;
        let spread = if s >= 9 {
            h9
        } else {
            let low_mask: u16 = ((1u32 << s) - 1) as u16;
            let low = h9 & low_mask;
            let high = (h9 & !low_mask) << 6;
            low | high
        };
        LIVE | spread
    }

    pub fn contains(&self, key: &K, hash: u64) -> bool {
        self.get_by_hash(key, hash).is_some()
    }

    /// reader: read-lock 下で SIMD scan して plain clone。MAX_READER_RETRY は不要
    /// (RwLock が writer を排他するので transient state を踏まない)。
    pub fn get_by_hash(&self, key: &K, hash: u64) -> Option<V> {
        let needle = Self::needle_from_hash(hash);
        let g = self.rw.read();
        // SAFETY: AVX2 は Shard::new の assert で検証済み。read-lock 下なので writer 排他、
        //         tags / entries は immutable view、`entries[id]` は LIVE tag が指す init slot。
        unsafe { Self::find_get(&g, key, needle) }
    }

    /// AVX2 SIMD scan + plain entry clone。
    ///
    /// SAFETY: caller は AVX2 が available なことを保証する (Shard::new で assert)。
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_get(inner: &ShardInner<K, V>, key: &K, needle: u16) -> Option<V> {
        use std::arch::x86_64::*;

        let tags_ptr = inner.tags.as_ptr();
        let entries_base = inner.entries.as_ptr();
        let needle_v = _mm256_set1_epi16(needle as i16);
        let mask_v = _mm256_set1_epi16(Self::SCAN_MASK as i16);
        let limit = inner.tags.len();

        let mut i = 0usize;
        while i < limit {
            // SAFETY: tags_ptr.add(i) は in-bounds (i < limit、tags の長さは order_cap)。
            //         `_mm256_loadu_si256` は unaligned read OK。read-lock 下で writer 排他。
            let v = unsafe { _mm256_loadu_si256(tags_ptr.add(i) as *const __m256i) };
            let masked = _mm256_and_si256(v, mask_v);
            let cmp = _mm256_cmpeq_epi16(masked, needle_v);
            let mut mask = _mm256_movemask_epi8(cmp) as u32;

            while mask != 0 {
                let bit = mask.trailing_zeros() as usize;
                let lane = bit >> 1;
                let pos = i + lane;
                // SAFETY: pos < limit (= order_cap)。tags[pos] は read-lock 下で writer 排他。
                let tag = unsafe { *tags_ptr.add(pos) };
                let id = ((tag & Self::ID_MASK) >> Self::ID_SHIFT) as usize;
                // SAFETY: LIVE tag が指す entries[id] は writer が write-lock 下で init 済み。
                //         read-lock 下で writer 排他、Entry の内容は不変。
                let entry_ref: &Entry<K, V> =
                    unsafe { &*(entries_base.add(id) as *const Entry<K, V>) };
                if entry_ref.key == *key {
                    let v = entry_ref.value.clone();
                    // hot-key conditional skip: bit が既に立っていれば fetch_or を撃たない
                    // (MESI ping-pong 回避、c11s 由来の最適化)。
                    let mask = Self::vbit_mask(pos);
                    if inner.visited.load(Ordering::Relaxed) & mask == 0 {
                        inner.visited.fetch_or(mask, Ordering::Relaxed);
                    }
                    return Some(v);
                }
                mask = _blsr_u32(mask);
                mask = _blsr_u32(mask);
            }
            i += LANE;
        }
        None
    }

    /// writer (insert)。write-lock を取って path-a (in-place update) / path-b (warmup install)
    /// / path-c (evict+install) を分岐する。RwLock 配下では Path A も seqlock 不要の plain mutate。
    pub fn insert(&self, key: K, value: V, hash: u64) -> Option<(K, V)> {
        let needle = Self::needle_from_hash(hash);
        let mut g = self.rw.write();
        // (a) 既存 key を update
        if let Some((pos, id)) = Self::writer_find(&g, &key, needle) {
            Self::writer_update_in_place(&mut g, pos, id, key, value);
            return None;
        }
        let len = g.len as usize;
        let cap = g.capacity as usize;
        // (b) Path B: warmup install (len < cap)
        if len < cap {
            Self::writer_warmup_install(&mut g, len, key, value, needle);
            return None;
        }
        // (c) Path C: evict + shift + install
        Some(Self::writer_evict_and_install(&mut g, key, value, needle))
    }

    /// remove: write-lock 配下で tags shift + entries の swap-with-last compact。
    /// `entries[0..len)` invariant を保つため、`id != last_id` のとき `entries[last_id]`
    /// を `entries[id]` に移して該当 tag の id field を rewrite する。RwLock 排他下なので
    /// reader race なし、senba::concurrent の `free_ids` + epoch defer が r3 では不要。
    pub fn remove(&self, key: &K, hash: u64) -> Option<V> {
        let needle = Self::needle_from_hash(hash);
        let mut g = self.rw.write();
        let (pos, id) = Self::writer_find(&g, key, needle)?;
        let len = g.len as usize;
        // (1) tags shift down: tags[pos+1..len] → tags[pos..len-1]
        for i in pos..(len - 1) {
            g.tags[i] = g.tags[i + 1];
            let s_mask = Self::vbit_mask(i + 1);
            let d_mask = Self::vbit_mask(i);
            let was_visited = g.visited.load(Ordering::Relaxed) & s_mask != 0;
            g.visited.fetch_and(!s_mask, Ordering::Relaxed);
            if was_visited {
                g.visited.fetch_or(d_mask, Ordering::Relaxed);
            } else {
                g.visited.fetch_and(!d_mask, Ordering::Relaxed);
            }
        }
        g.tags[len - 1] = EMPTY;
        g.visited
            .fetch_and(!Self::vbit_mask(len - 1), Ordering::Relaxed);

        // (2) entries[id] を取り出し
        // SAFETY: writer_find で LIVE が指していた slot なので init 済み。
        let entry = unsafe { std::ptr::read(g.entries.as_ptr().add(id) as *const Entry<K, V>) };

        // (3) swap-with-last: id != last_id なら entries[last_id] を entries[id] に詰めて
        //     対応する tag の id field を rewrite する。len ≤ 64 で線形 tag scan が cold path。
        let last_id = len - 1;
        if id != last_id {
            let new_len = len - 1;
            let mut fixed = false;
            for j in 0..new_len {
                let t = g.tags[j];
                if (t & LIVE) != 0 && Self::id_of(t) == last_id {
                    // SAFETY: entries[last_id] は (1) で touched なし、まだ init 済み。
                    //         id (= 削除済み slot) は (2) で読み出された uninit。
                    let last_entry = unsafe {
                        std::ptr::read(g.entries.as_ptr().add(last_id) as *const Entry<K, V>)
                    };
                    g.entries[id] = MaybeUninit::new(last_entry);
                    let new_tag = (t & !Self::ID_MASK) | ((id as u16) << Self::ID_SHIFT);
                    g.tags[j] = new_tag;
                    fixed = true;
                    break;
                }
            }
            debug_assert!(
                fixed,
                "remove: no live tag pointing to last_id={last_id}; invariant violated"
            );
        }

        g.len = (len - 1) as u16;
        if g.hand as usize >= g.len as usize {
            g.hand = 0;
        }
        drop(entry.key);
        Some(entry.value)
    }

    /// writer 内部 find: write-lock 配下なので spin / seqlock 不要、plain scan。
    fn writer_find(inner: &ShardInner<K, V>, key: &K, needle: u16) -> Option<(usize, usize)> {
        let len = inner.len as usize;
        for pos in 0..len {
            let t = inner.tags[pos];
            if (t & LIVE) == 0 {
                continue;
            }
            if (t & Self::SCAN_MASK) != needle {
                continue;
            }
            let id = Self::id_of(t);
            // SAFETY: write-lock 下、LIVE tag が指す entries[id] は init 済み。
            let entry = unsafe { inner.entries[id].assume_init_ref() };
            if entry.key == *key {
                return Some((pos, id));
            }
        }
        None
    }

    /// writer Mutex 配下の既存 key 更新。引数 `key` は重複として drop。
    fn writer_update_in_place(
        inner: &mut ShardInner<K, V>,
        pos: usize,
        id: usize,
        key: K,
        value: V,
    ) {
        // SAFETY: write-lock 下、entries[id] は writer_find が LIVE で確認済みの init slot。
        let entry = unsafe { inner.entries[id].assume_init_mut() };
        let old_value = std::mem::replace(&mut entry.value, value);
        drop(key);
        drop(old_value);
        // visited SET (sieve_orig の `freq=1` と一致)
        inner
            .visited
            .fetch_or(Self::vbit_mask(pos), Ordering::Relaxed);
    }

    /// Path B: warmup install (len < capacity)。`entries[0..len)` 一直線 invariant のため
    /// 新規 entry の id は常に `len` (c17s と同じ sequential 払出し)。
    fn writer_warmup_install(
        inner: &mut ShardInner<K, V>,
        len: usize,
        key: K,
        value: V,
        needle: u16,
    ) {
        let entry_id = len as u16;
        inner.entries[len] = MaybeUninit::new(Entry { key, value });
        // 新 install は visited=0 (sieve_orig も新 entry は freq=0)
        inner
            .visited
            .fetch_and(!Self::vbit_mask(len), Ordering::Relaxed);
        let new_tag = LIVE | (entry_id << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        inner.tags[len] = new_tag;
        inner.len = (len + 1) as u16;
    }

    /// Path C: 定常 evict + shift + install。c17s と同型の SIEVE 状態機械を write-lock 下で
    /// plain mutation として実行。evict_id は inline 再利用 (free_ids には返さない)。
    fn writer_evict_and_install(
        inner: &mut ShardInner<K, V>,
        key: K,
        value: V,
        needle: u16,
    ) -> (K, V) {
        let cap = inner.capacity as usize;
        debug_assert_eq!(inner.len as usize, cap);
        if inner.hand as usize >= cap {
            inner.hand = 0;
        }
        let hand = inner.hand as usize;
        let evict_pos = Self::scan_evict(inner, hand, cap)
            .or_else(|| Self::scan_evict(inner, 0, hand))
            .unwrap_or(hand);
        let evict_tag = inner.tags[evict_pos];
        debug_assert!(evict_tag & LIVE != 0, "evict_tag must be LIVE");
        let evict_id = Self::id_of(evict_tag);

        // SAFETY: LIVE tag が指す init slot を読み出し、後で同じ id に上書き install する。
        let evicted =
            unsafe { std::ptr::read(inner.entries.as_ptr().add(evict_id) as *const Entry<K, V>) };
        // 新 entry を同じ id slot に install (id 再利用)
        inner.entries[evict_id] = MaybeUninit::new(Entry { key, value });

        // shift tags[evict_pos+1..cap] を tags[evict_pos..cap-1] に下げる
        for i in evict_pos..(cap - 1) {
            inner.tags[i] = inner.tags[i + 1];
            let s_mask = Self::vbit_mask(i + 1);
            let d_mask = Self::vbit_mask(i);
            let was_visited = inner.visited.load(Ordering::Relaxed) & s_mask != 0;
            inner.visited.fetch_and(!s_mask, Ordering::Relaxed);
            if was_visited {
                inner.visited.fetch_or(d_mask, Ordering::Relaxed);
            } else {
                inner.visited.fetch_and(!d_mask, Ordering::Relaxed);
            }
        }
        // 新 tag を tags[cap-1] (SIEVE order の "head") に書く
        let new_tag = LIVE | ((evict_id as u16) << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        inner.tags[cap - 1] = new_tag;
        inner
            .visited
            .fetch_and(!Self::vbit_mask(cap - 1), Ordering::Relaxed);
        // hand 進め: senba::Cache の `pos < last ? pos : 0`
        inner.hand = if evict_pos < cap - 1 {
            evict_pos as u16
        } else {
            0
        };

        (evicted.key, evicted.value)
    }

    /// hand 巡回: visited を見て立っていれば剥がす、立っていなければ evict 候補。
    /// write-lock 下なので EMPTY transient は存在しない (Path C 自身が走る瞬間のみ)。
    fn scan_evict(inner: &mut ShardInner<K, V>, lo: usize, hi: usize) -> Option<usize> {
        for i in lo..hi {
            let t = inner.tags[i];
            debug_assert!(
                t & LIVE != 0,
                "scan_evict: tags[{i}] not LIVE under write-lock (t = {t:#x})"
            );
            let mask = Self::vbit_mask(i);
            if inner.visited.load(Ordering::Relaxed) & mask != 0 {
                inner.visited.fetch_and(!mask, Ordering::Relaxed);
            } else {
                return Some(i);
            }
        }
        None
    }

    #[cfg(test)]
    pub(crate) fn live_count(&self) -> usize {
        let g = self.rw.read();
        let mut n = 0;
        for &t in g.tags.iter() {
            if t & LIVE != 0 {
                n += 1;
            }
        }
        n
    }

    #[cfg(test)]
    pub(crate) fn live_ids(&self) -> Vec<usize> {
        let g = self.rw.read();
        let mut ids = Vec::new();
        let len = g.len as usize;
        for i in 0..len {
            let t = g.tags[i];
            if t & LIVE != 0 {
                ids.push(Self::id_of(t));
            }
        }
        ids
    }
}

impl<K, V> Drop for Shard<K, V> {
    fn drop(&mut self) {
        let g = self.rw.get_mut();
        let len = g.len as usize;
        for i in 0..len {
            let t = g.tags[i];
            if t & LIVE != 0 {
                let id = Self::id_of(t);
                // SAFETY: LIVE ⇒ entries[id] init 済み。
                unsafe {
                    g.entries[id].assume_init_drop();
                }
            }
        }
    }
}

// ---------------- 外側 wrapper ----------------

pub const DEFAULT_SHARDS: usize = 8;

pub struct ConcurrentSieveR3<K, V, const SHARDS: usize = DEFAULT_SHARDS> {
    shards: Box<[Shard<K, V>]>,
    hasher: Xxh3Build,
}

impl<K, V, const SHARDS: usize> ConcurrentSieveR3<K, V, SHARDS>
where
    K: Hash + Eq + Send + Sync,
    V: Clone + Send + Sync,
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
        let mut shards_vec: Vec<Shard<K, V>> = Vec::with_capacity(SHARDS);
        for i in 0..SHARDS {
            let cap_i = base + if i < extra { 1 } else { 0 };
            shards_vec.push(Shard::new(cap_i));
        }
        Self {
            shards: shards_vec.into_boxed_slice(),
            hasher: Xxh3Build,
        }
    }

    pub fn capacity(&self) -> usize {
        self.shards.iter().map(|s| s.capacity()).sum()
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.is_empty())
    }

    pub fn contains_key(&self, key: &K) -> bool {
        let h = self.hasher.hash_one(key);
        self.shards[Self::shard_of_hash(h)].contains(key, h)
    }

    pub fn get(&self, key: &K) -> Option<V> {
        let h = self.hasher.hash_one(key);
        self.shards[Self::shard_of_hash(h)].get_by_hash(key, h)
    }

    pub fn insert(&self, key: K, value: V) -> Option<(K, V)> {
        let h = self.hasher.hash_one(&key);
        let i = Self::shard_of_hash(h);
        self.shards[i].insert(key, value, h)
    }

    pub fn remove(&self, key: &K) -> Option<V> {
        let h = self.hasher.hash_one(key);
        let i = Self::shard_of_hash(h);
        self.shards[i].remove(key, h)
    }

    #[inline]
    fn shard_of_hash(hash: u64) -> usize {
        (hash as usize) & (SHARDS - 1)
    }

    #[cfg(test)]
    pub(crate) fn shard(&self, idx: usize) -> &Shard<K, V> {
        &self.shards[idx]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    impl crate::experimental::ConcurrentCacheImpl<u64, u64> for ConcurrentSieveR3<u64, u64> {
        fn with_capacity(capacity: usize) -> Self {
            Self::new(capacity)
        }
        fn capacity(&self) -> usize {
            self.capacity()
        }
        fn len(&self) -> usize {
            self.len()
        }
        fn contains_key(&self, key: &u64) -> bool {
            self.contains_key(key)
        }
        fn get(&self, key: &u64) -> Option<u64> {
            self.get(key)
        }
        fn insert(&self, key: u64, value: u64) -> Option<(u64, u64)> {
            self.insert(key, value)
        }
    }

    crate::concurrent_suite!(ConcurrentSieveR3<u64, u64>);

    #[test]
    fn evicts_oldest_when_full_and_unvisited() {
        let cache: ConcurrentSieveR3<i32, i32, 1> = ConcurrentSieveR3::new(2);
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
        let cache: ConcurrentSieveR3<i32, i32, 1> = ConcurrentSieveR3::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((2, 20)));
    }

    #[test]
    fn all_visited_clears_bits_then_evicts() {
        let cache: ConcurrentSieveR3<i32, i32, 1> = ConcurrentSieveR3::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        cache.get(&2);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((1, 10)));
    }

    #[test]
    fn distinct_keys_full_per_shard_all_hit() {
        let n: u64 = 64;
        let cache: ConcurrentSieveR3<u64, u64, 1> = ConcurrentSieveR3::new(n as usize);
        for k in 0..n {
            cache.insert(k, k * 7);
        }
        for k in 0..n {
            assert_eq!(cache.get(&k), Some(k * 7), "miss for key {k}");
        }
    }

    /// Path A (既存 key update) は eviction を起こさず id 配置不変。
    #[test]
    fn update_preserves_id_and_tag() {
        let cache: ConcurrentSieveR3<u64, u64, 1> = ConcurrentSieveR3::new(4);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.insert(3, 30);
        cache.insert(4, 40);
        let sh = cache.shard(0);
        let ids_before = sh.live_ids();
        let tags_before: Vec<u16> = {
            let g = sh.rw.read();
            (0..4).map(|i| g.tags[i]).collect()
        };
        cache.insert(2, 222);
        let ids_after = sh.live_ids();
        let tags_after: Vec<u16> = {
            let g = sh.rw.read();
            (0..4).map(|i| g.tags[i]).collect()
        };
        assert_eq!(ids_before, ids_after);
        assert_eq!(tags_before, tags_after);
        assert_eq!(cache.get(&2), Some(222));
    }

    /// 既存キー update が visited を 1 に SET (sieve_orig の `freq=1` と一致)。
    #[test]
    fn update_existing_key_sets_visited_like_oracle() {
        let cache: ConcurrentSieveR3<i32, i32, 1> = ConcurrentSieveR3::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.insert(1, 11);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((2, 20)));
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }

    /// reader hit が tag を変更しない (visited は別に立つ)。
    #[test]
    fn reader_hit_does_not_modify_tag() {
        let cache: ConcurrentSieveR3<u64, u64, 1> = ConcurrentSieveR3::new(4);
        cache.insert(1, 100);
        let sh = cache.shard(0);
        let tag_before = sh.rw.read().tags[0];
        assert_eq!(cache.get(&1), Some(100));
        let tag_after = sh.rw.read().tags[0];
        assert_eq!(tag_before, tag_after);
        let mask = Shard::<u64, u64>::vbit_mask(0);
        assert!(sh.rw.read().visited.load(Ordering::Acquire) & mask != 0);
    }

    /// Path C eviction で id は inline 再利用、新 entry は tags[cap-1] に install される。
    #[test]
    fn evict_reuses_id_at_tail_position() {
        let cache: ConcurrentSieveR3<u64, u64, 1> = ConcurrentSieveR3::new(4);
        for k in 0..4u64 {
            cache.insert(k, k * 10);
        }
        let sh = cache.shard(0);
        let ids_before = sh.live_ids();
        assert_eq!(ids_before, vec![0, 1, 2, 3]);
        let evicted = cache.insert(99, 9900);
        assert!(evicted.is_some());
        let last_tag = sh.rw.read().tags[3];
        let last_id = Shard::<u64, u64>::id_of(last_tag);
        assert_eq!(last_id, 0, "Path C で id 再利用していない");
    }

    /// remove: shift + swap-with-last で entries が compact される (entries[0..len) invariant)。
    /// 削除後 ids が `[0, len)` に収まり、新規 insert は `entry_id = len` で sequential 払出し。
    #[test]
    fn remove_compacts_entries() {
        let cache: ConcurrentSieveR3<u64, u64, 1> = ConcurrentSieveR3::new(4);
        for k in 0..4u64 {
            cache.insert(k, k * 10);
        }
        let sh = cache.shard(0);
        // 削除前: entries[0..4] = (0,1,2,3) で id == pos
        let ids_before = sh.live_ids();
        assert_eq!(ids_before, vec![0, 1, 2, 3]);

        // id=2 を削除。swap-with-last で entries[3] が entries[2] に移り、
        // 対応する tag (元 pos=3 だったが shift で pos=2 に来ている) の id が 3→2 に書き換わる。
        let removed = cache.remove(&2);
        assert_eq!(removed, Some(20));
        assert_eq!(sh.live_count(), 3);
        assert!(!cache.contains_key(&2));

        // invariant: 残った 3 entry の ids は [0, 3) 内
        let ids_after = sh.live_ids();
        assert_eq!(ids_after.len(), 3);
        let mut sorted = ids_after.clone();
        sorted.sort();
        assert_eq!(sorted, vec![0, 1, 2], "entries[0..3) compact 不成立");

        // key=3 はまだ取れる (swap-with-last で entries[2] に移っただけ)
        assert_eq!(cache.get(&3), Some(30));
        assert_eq!(cache.get(&0), Some(0));
        assert_eq!(cache.get(&1), Some(10));

        // 新規 insert は entry_id = len = 3 で entries[3] を使う
        cache.insert(100, 1000);
        assert_eq!(sh.live_count(), 4);
        assert_eq!(cache.get(&100), Some(1000));
    }

    /// 末尾 id を削除する場合 swap-with-last はスキップされ entries[last_id] が uninit になる。
    #[test]
    fn remove_last_id_skips_swap() {
        let cache: ConcurrentSieveR3<u64, u64, 1> = ConcurrentSieveR3::new(4);
        for k in 0..4u64 {
            cache.insert(k, k * 10);
        }
        let sh = cache.shard(0);
        // 一番最後に install された entry (id=3, key=3) を削除
        let removed = cache.remove(&3);
        assert_eq!(removed, Some(30));
        assert_eq!(sh.live_count(), 3);
        let ids_after = sh.live_ids();
        let mut sorted = ids_after.clone();
        sorted.sort();
        assert_eq!(sorted, vec![0, 1, 2]);
        assert_eq!(cache.get(&0), Some(0));
        assert_eq!(cache.get(&1), Some(10));
        assert_eq!(cache.get(&2), Some(20));
    }

    /// 並行不変条件 (c16s/c17s と同型)。
    #[test]
    fn concurrent_invariants_under_zipf() {
        use crate::workload::zipf::ZipfGen;
        let cap = 256usize;
        let cache: Arc<ConcurrentSieveR3<u64, u64, 8>> = Arc::new(ConcurrentSieveR3::new(cap));

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

        let total_len = cache.len();
        assert!(total_len <= cap, "len {total_len} > cap {cap}");

        let mut sum_live = 0;
        for i in 0..8 {
            let sh = cache.shard(i);
            let live = sh.live_count();
            let ids = sh.live_ids();
            assert_eq!(live, ids.len());
            let mut sorted = ids.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), ids.len(), "shard {i} で id 重複");
            sum_live += live;
        }
        assert_eq!(sum_live, total_len);

        for k in 0..1024u64 {
            if let Some(v) = cache.get(&k) {
                assert_eq!(v, k);
            }
        }
    }

    /// sieve_orig (oracle) と 1-shard 外部一致。
    #[test]
    fn matches_sieve_orig_externally_1shard() {
        use crate::experimental::sieve_orig::SieveCache as Orig;
        let cap = 64usize;
        let mut a: Orig<u64, u64> = Orig::new(cap);
        let b: ConcurrentSieveR3<u64, u64, 1> = ConcurrentSieveR3::new(cap);
        for k in 0..10_000u64 {
            let key = (k.wrapping_mul(2654435761)) % 256;
            let _ = a.insert(key, key);
            let _ = b.insert(key, key);
        }
        for k in 0..256u64 {
            assert_eq!(
                a.get(&k).copied(),
                b.get(&k),
                "1-shard で sieve_orig と r3 が key {k} で食い違う"
            );
        }
    }

    /// r3 の bit layout: VERSION なし、Entry sizeof=16 (u64,u64)、ID_SHIFT=4。
    #[test]
    fn bit_layout_u64_u64() {
        type S = Shard<u64, u64>;
        assert_eq!(std::mem::size_of::<Entry<u64, u64>>(), 16);
        assert_eq!(S::ID_SHIFT, 4);
        // ID_MASK = 63 << 4 = 0x03F0
        assert_eq!(S::ID_MASK, 0x03F0);
        // HASH_MASK = 0x7FFF & !0x03F0 = 0x7C0F、9 bit
        assert_eq!(S::HASH_MASK, 0x7C0F);
        assert_eq!(S::HASH_MASK.count_ones(), 9);
        assert_eq!(S::SCAN_MASK, LIVE | S::HASH_MASK);
        // LIVE | ID | HASH の 3 区画で 0xFFFF を埋める。
        assert_eq!(LIVE | S::ID_MASK | S::HASH_MASK, 0xFFFF);
        assert_eq!(LIVE & S::ID_MASK, 0);
        assert_eq!(LIVE & S::HASH_MASK, 0);
        assert_eq!(S::ID_MASK & S::HASH_MASK, 0);
    }
}
