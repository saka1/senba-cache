#![cfg(all(target_arch = "x86_64", not(miri)))]
//! `sieve_c18s`: c17s から `Entry::version` を別配列 `versions: [AtomicU32; 64]` に
//! 逃がした variant (G2-α-2)。
//!
//! c17s からの **構造差分**:
//! 1. `Entry<K, V>` を Slot16 (`{ key, value }` 16B, `repr(C, align(16))`) に戻す。
//!    version field は廃止。entries arena per-shard cap=64 = 1024B = 16 cache line
//!    (c17s の 32 line / 2KB の半分)。
//! 2. 新 `ReaderState` block を導入: `{ path_c_epoch: AtomicU64, _pad0: [u8; 56],
//!    versions: [AtomicU32; 64] }`、5 cache line。reader-only cluster として
//!    writer-hot な ShardHot (Mutex / visited / len) と完全分離する。
//! 3. `path_c_epoch` を ShardHot から ReaderState に移動。c17s では Path A の visited
//!    fetch_or で path_c_epoch line が常時 invalidate される構造だったが、c18s では
//!    line 0 を path_c_epoch 専用化し、versions[*] の Path A W (line 1〜4) からも
//!    完全分離する (`_pad0: [u8; 56]` で line 0 を path_c_epoch に専有させる)。
//! 4. ID_SHIFT が 5 → 4 になる (Entry sizeof 32→16)。HASH bit 数は 9 で c17s と同等
//!    (bit 位置は移動: c17s は 0x781F、c18s は 0x7C0F)。
//! 5. Path A / Path C entries 上書き時の version 操作先が `entries[id].version` から
//!    `reader_state.versions[id]` に変わる。CAS even→odd → write entries[id] →
//!    store even+2 という idiom 自体は不変。
//!
//! 設計の一次資料: `docs/reports/2026-05-12-c18s-design.md`。
//! 動機 (c17s gate (b) fail の構造除去): `docs/reports/2026-05-11-c17s-results.md` §7.1
//! と `docs/improvement-ideas.md` §D.1 / §8.1 #1。
//!
//! # tag layout (16 bit、c17s と同 bit 数 / 別 bit 位置)
//!
//! ```text
//!   bit 15:        LIVE
//!   bits 4..10:    ID (6 bit)        ← ID_SHIFT = 4 (c17s は 5)
//!   bits 0..4 と 10..15: HASH (9 bit) ← c17s と同 bit 数、bit 位置だけ移動
//! ```
//!
//! `SCAN_MASK = LIVE | HASH_MASK` は ID を除外。`needle_from_hash` の spread は
//! ID_SHIFT に依存して動的に組まれるので変更不要。
//!
//! # `V: !Copy` (e.g. `String`) でも健全
//!
//! reader が `versions[id].load` (v1) → ptr::read entries[id] → `versions[id].load`
//! (v2) という seqlock dance は c17s と同型。半上書き Entry は手元に来ないので
//! `String` 等の非 Copy V も c17s と同様に sound (実測根拠は `cseries-string-baseline`)。
//!
//! # Path A 同時実行排他
//!
//! tag CAS が無いので、`reader_state.versions[id].compare_exchange(v_even, v_even + 1)`
//! が排他機構。失敗した writer は MAX_RETRY=1 で抜けて Path B/C (Mutex) に escalate
//! する (c14s/c16s/c17s と同じ retry policy)。
//!
//! # Path C false-miss と path_c_epoch
//!
//! c17s と同様、Path C の shift loop は `tags[i] = EMPTY → tags[i] = next_tag` の
//! 2 段 store を踏む。reader の SIMD scan が EMPTY transient を踏むと「該当 chunk に
//! candidate なし」と判定し racing flag が立たない (= retry 不発) → false-miss 化する。
//! c18s では `reader_state.path_c_epoch: AtomicU64` を Path C 完了時に bump し、
//! reader は scan 前後で epoch を snapshot して変化があれば retry する coarse-grained
//! seqlock を持つ (c17s と同型、配置のみ ShardHot から ReaderState に移動)。
//!
//! # K, V trait bounds
//!
//! - `K: Hash + Eq` (Clone 不要)
//! - `V: Clone`
//!
//! ## V::Clone soundness の限界 (research artifact 限定)
//!
//! c17s と同じ。V::clone を呼んでいる **mid-flight** に並行 Path A が old V を drop
//! すると UB の可能性。V = Copy (u64 等) なら sound、heap-owning V の本番用途では
//! Arc<V> / Epoch GC が要。bench は V = u64 を使うので sound。
//!
//! # 実装 scope
//!
//! 本ファイルは **x86_64 + AVX2 + non-miri 専用** (research artifact)。AVX2 が必須
//! なので `Shard::new` で runtime detect。scalar fallback は持たない。

use parking_lot::Mutex;
use senba::Xxh3Build;
use std::cell::UnsafeCell;
use std::hash::{BuildHasher, Hash};
use std::hint;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering, fence};

/// EMPTY tag (LIVE OFF)。Path C の shift transient と pad lane に使う。
const EMPTY: u16 = 0;
/// LIVE bit (bit 15)。tag が有効な entry を指していることを示す。
const LIVE: u16 = 0x8000;
/// AVX2 1 chunk = 32 byte = 16 u16 lane。
const LANE: usize = 16;
/// 6-bit entry_id の構造的上限。per_shard はこの値以下でなければならない。
pub const MAX_PER_SHARD: usize = 64;

/// `sizeof(Entry)` から ID_SHIFT (= log2(sizeof)) を const-eval で算出。
const fn id_shift_from_entry_size(s: usize) -> u32 {
    assert!(
        s.is_power_of_two(),
        "sieve_c18s: sizeof(Entry<K,V>) must be a power of two"
    );
    assert!(s <= 256, "sieve_c18s: sizeof(Entry<K,V>) must be <= 256");
    s.trailing_zeros()
}

const fn id_mask_from_shift(id_shift: u32) -> u16 {
    ((MAX_PER_SHARD - 1) as u16) << id_shift
}

/// HASH 領域は 0x7FFF (LIVE のみ除外) から ID 6 bit を抜いたもの。c17s と同じ 9 bit。
const fn hash_mask_from_id_mask(id_mask: u16) -> u16 {
    0x7FFF & !id_mask
}

/// Slot16 Entry。version field は廃止 (ReaderState::versions に移動)。
#[repr(C, align(16))]
struct Entry<K, V> {
    key: K,
    value: V,
}

/// reader scan 1 slot の結果。
enum Probe<V> {
    Found(V),
    Miss,
    Racing,
}

type EntriesArena<K, V> = UnsafeCell<Box<[MaybeUninit<Entry<K, V>>]>>;

/// reader-only cluster。**writer-hot な ShardHot から MESI 干渉を分離する**目的で 1 block
/// に co-locate。
///
/// - `path_c_epoch` は line 0 専用化 (`_pad0: [u8; 56]` で他 field を排除)。Path C 以外
///   この line に W しない。reader (毎 call 2 load) と Path A は触らない。
/// - `versions: [AtomicU32; 64]` は line 1〜4 (16 versions/line)。Path A の W は id 単位で
///   散布。
///
/// 計 5 cache line。reader は `path_c_epoch.load → versions[id].load` で同 prefetch 区間を
/// touch する spatial locality も狙う。
#[repr(C, align(64))]
struct ReaderState {
    path_c_epoch: AtomicU64,
    _pad0: [u8; 56],
    versions: [AtomicU32; 64],
}

const _: () = {
    assert!(std::mem::size_of::<ReaderState>() == 64 + 256);
    assert!(std::mem::align_of::<ReaderState>() == 64);
};

/// writer-hot cluster。c17s から `path_c_epoch` を取り除いて writer 専用化。
/// trailing padding は `repr(C, align(64))` が自動付与 (sizeof=64 維持)。
#[repr(C, align(64))]
struct ShardHot {
    /// Path B/C 排他。Path A (lock-free version CAS) は Mutex を取らない。
    writer: Mutex<WriterState>,
    /// 1 shard 全 visited (cap ≤ 64)。reader fetch_or / writer fetch_and 両方が触る。
    visited: AtomicU64,
    /// live entry 数。reader scan は `tags[0..len]` を見る。
    len: AtomicUsize,
}

const _: () = {
    assert!(std::mem::size_of::<ShardHot>() == 64);
    assert!(std::mem::align_of::<ShardHot>() == 64);
};

/// 1 shard 分の並行 SIEVE。
pub struct Shard<K, V> {
    capacity: usize,
    /// tag 列。AtomicU16 で原子操作。R + Path C のみ W (eviction 時の shift loop)。
    tags: Box<[AtomicU16]>,
    /// reader-only cluster (path_c_epoch + versions)。Box で heap 上に 64-byte aligned 配置。
    reader_state: Box<ReaderState>,
    /// entries arena。Slot16 化で c17s 比 footprint 半減 (per-shard 1024B = 16 line)。
    entries: EntriesArena<K, V>,
    /// writer-hot cluster (Mutex / visited / len)。
    hot: ShardHot,
}

struct WriterState {
    hand: usize,
}

// SAFETY: c17s と同じ。entries[id] への書き込みは reader_state.versions[id] CAS で
// 所有権を確保した writer または Mutex 配下の writer のみが行い、reader は version
// seqlock + ManuallyDrop で torn read / use-after-free を弾く。V: Clone soundness の
// 限界は module doc 参照。
unsafe impl<K: Send, V: Send> Send for Shard<K, V> {}
unsafe impl<K: Send + Sync, V: Send + Sync> Sync for Shard<K, V> {}

impl<K, V> Shard<K, V> {
    const ENTRY_SIZE: usize = std::mem::size_of::<Entry<K, V>>();
    const ID_SHIFT: u32 = id_shift_from_entry_size(Self::ENTRY_SIZE);
    const ID_MASK: u16 = id_mask_from_shift(Self::ID_SHIFT);
    const HASH_MASK: u16 = hash_mask_from_id_mask(Self::ID_MASK);
    /// reader needle 比較用。LIVE + HASH (9 bit)、ID は除外。
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
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.hot.len.load(Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<K, V> Shard<K, V>
where
    K: Hash + Eq,
    V: Clone,
{
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        assert!(
            capacity <= MAX_PER_SHARD,
            "per-shard capacity ({capacity}) must be <= {MAX_PER_SHARD} (6-bit ID limit)"
        );
        assert!(
            std::is_x86_feature_detected!("avx2"),
            "sieve_c18s: AVX2 required (research artifact); compile-time gated to x86_64+non-miri but runtime CPU lacks AVX2"
        );
        let order_cap = ((capacity + LANE - 1) & !(LANE - 1)).max(LANE);

        let mut tags_vec: Vec<AtomicU16> = Vec::with_capacity(order_cap);
        for _ in 0..order_cap {
            tags_vec.push(AtomicU16::new(EMPTY));
        }

        let mut entries_vec: Vec<MaybeUninit<Entry<K, V>>> = Vec::with_capacity(capacity);
        entries_vec.resize_with(capacity, MaybeUninit::uninit);

        let reader_state = Box::new(ReaderState {
            path_c_epoch: AtomicU64::new(0),
            _pad0: [0u8; 56],
            versions: std::array::from_fn(|_| AtomicU32::new(0)),
        });

        Self {
            capacity,
            tags: tags_vec.into_boxed_slice(),
            reader_state,
            entries: UnsafeCell::new(entries_vec.into_boxed_slice()),
            hot: ShardHot {
                writer: Mutex::new(WriterState { hand: 0 }),
                visited: AtomicU64::new(0),
                len: AtomicUsize::new(0),
            },
        }
    }

    /// hash → tag bit spread。HASH 9 bit を ID 切れ目を跨いで spread。c17s と同型、
    /// `s = ID_SHIFT` の値だけが変わる (c18s は s=4)。
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

    /// reader 用 AVX2 scan。EMPTY-lane SIMD 検出は削除済 (c17s から継承)。Path C 由来の
    /// EMPTY transient は `path_c_epoch` で coarse 検出。
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_get(&self, key: &K, needle: u16) -> (Option<V>, bool) {
        use std::arch::x86_64::*;

        let len = self.hot.len.load(Ordering::Acquire);
        let tags_ptr = self.tags.as_ptr() as *const u16;
        let needle_v = _mm256_set1_epi16(needle as i16);
        let mask_v = _mm256_set1_epi16(Self::SCAN_MASK as i16);

        let limit = self.tags.len();

        let mut i = 0usize;
        let mut racing = false;
        while i < limit {
            let v = unsafe { _mm256_loadu_si256(tags_ptr.add(i) as *const __m256i) };
            let masked = _mm256_and_si256(v, mask_v);
            let cmp = _mm256_cmpeq_epi16(masked, needle_v);
            let mut mask = _mm256_movemask_epi8(cmp) as u32;

            while mask != 0 {
                let bit = mask.trailing_zeros() as usize;
                let lane = bit >> 1;
                let pos = i + lane;
                if pos < len {
                    match self.try_candidate(pos, key, needle) {
                        Probe::Found(val) => return (Some(val), false),
                        Probe::Racing => racing = true,
                        Probe::Miss => {}
                    }
                }
                mask = _blsr_u32(mask);
                mask = _blsr_u32(mask);
            }
            i += LANE;
        }
        (None, racing)
    }

    /// reader entry-level seqlock dance。tier 1 (versions[id] v1==v2) で Path A / Path C
    /// entries 上書きを捕える。tier 2 (tag re-load) は冗長として削除済 (c17s v2 を継承)。
    #[inline]
    fn try_candidate(&self, pos: usize, key: &K, needle: u16) -> Probe<V> {
        let t1 = self.tags[pos].load(Ordering::Acquire);
        if (t1 & Self::SCAN_MASK) != needle {
            return Probe::Miss;
        }
        let id = Self::id_of(t1);
        let entries_base = self.entries_ptr();
        // SAFETY: id < cap (= len) で entries[id] は init 済み (LIVE tag が指していた)。
        let entry_ptr = unsafe { entries_base.add(id) as *const Entry<K, V> };

        // tier 1 (versions[id] 偶奇 + 一致)
        let version_ref = &self.reader_state.versions[id];
        let v1 = version_ref.load(Ordering::Acquire);
        if v1 & 1 != 0 {
            return Probe::Racing;
        }
        // SAFETY: ManuallyDrop で local の Drop を抑制。
        let buf: ManuallyDrop<Entry<K, V>> =
            unsafe { ManuallyDrop::new(std::ptr::read(entry_ptr)) };
        let v2 = version_ref.load(Ordering::Acquire);
        if v1 != v2 {
            return Probe::Racing;
        }
        // Validated: buf is a consistent snapshot. Safe to call K::eq + V::clone.
        if buf.key == *key {
            let v = buf.value.clone();
            // visited bit conditional set (c11s 由来、hot key の MESI ping-pong 回避)。
            let mask = Self::vbit_mask(pos);
            if self.hot.visited.load(Ordering::Relaxed) & mask == 0 {
                self.hot.visited.fetch_or(mask, Ordering::Relaxed);
            }
            return Probe::Found(v);
        }
        Probe::Miss
    }

    #[inline]
    fn entries_ptr(&self) -> *const MaybeUninit<Entry<K, V>> {
        unsafe { (*self.entries.get()).as_ptr() }
    }

    pub fn contains(&self, key: &K, hash: u64) -> bool {
        self.get_by_hash(key, hash).is_some()
    }

    /// c17s と同型: `path_c_epoch` snapshot による coarse retry + `try_candidate` 由来の
    /// `racing` flag による fine retry の OR で MAX_READER_RETRY 回まで再試行する。
    pub fn get_by_hash(&self, key: &K, hash: u64) -> Option<V> {
        const MAX_READER_RETRY: usize = 4;
        let needle = Self::needle_from_hash(hash);
        for attempt in 0..MAX_READER_RETRY {
            let epoch_before = self.reader_state.path_c_epoch.load(Ordering::Acquire);
            // SAFETY: AVX2 は Shard::new の assert で検証済み。
            let (v, racing) = unsafe { self.find_get(key, needle) };
            if let Some(v) = v {
                return Some(v);
            }
            let epoch_after = self.reader_state.path_c_epoch.load(Ordering::Acquire);
            if !racing && epoch_before == epoch_after {
                return None;
            }
            if attempt + 1 < MAX_READER_RETRY {
                hint::spin_loop();
            }
        }
        None
    }

    /// writer (insert)。Path A (lock-free) を MAX_RETRY 回まで試み、失敗したら
    /// Path B/C (writer Mutex) に escalate。
    pub fn insert(&self, key: K, value: V, hash: u64) -> Option<(K, V)> {
        let needle = Self::needle_from_hash(hash);
        match self.try_path_a(&key, needle, value) {
            Ok(()) => {
                drop(key);
                None
            }
            Err(value) => self.path_bc(key, value, needle),
        }
    }

    /// Path A: lock-free CAS update for existing key (`reader_state.versions[id]` 偶奇 flip)。
    fn try_path_a(&self, key: &K, needle: u16, value: V) -> Result<(), V> {
        const MAX_RETRY: usize = 1;
        let mut value_holder = ManuallyDrop::new(value);
        for _ in 0..MAX_RETRY {
            // SAFETY: AVX2 は Shard::new の assert で検証済み。
            let found = unsafe { self.find_lockfree_for_path_a(key, needle) };
            let (pos, id, v_snap) = match found {
                Some(x) => x,
                None => {
                    let v = unsafe { ManuallyDrop::take(&mut value_holder) };
                    return Err(v);
                }
            };
            // SAFETY: entries[id] は LIVE tag が指していた init 済み slot。
            let entries_mut = self.entries.get();
            let entry_ptr = unsafe { (*entries_mut).as_mut_ptr().add(id) as *mut Entry<K, V> };
            let version_ref = &self.reader_state.versions[id];
            // version CAS: even (= v_snap) → odd (= v_snap + 1)。
            match version_ref.compare_exchange(
                v_snap,
                v_snap.wrapping_add(1),
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Ok(_) => {}
                Err(_) => continue,
            }
            // CAS 成功: entries[id].value への排他書き込み権獲得 (key は同じなので触らない)。
            let new_value = unsafe { ManuallyDrop::take(&mut value_holder) };
            // SAFETY: version 奇数で reader は bail out、別 writer も CAS で弾かれる。
            let old_value: V = unsafe { std::ptr::read(&(*entry_ptr).value) };
            unsafe {
                std::ptr::write(&mut (*entry_ptr).value, new_value);
            }
            // version を even (= v_snap + 2) に store (Release で先行 store を publish)。
            version_ref.store(v_snap.wrapping_add(2), Ordering::Release);
            // visited SET (sieve_orig の `freq=1` と一致)。
            let mask = Self::vbit_mask(pos);
            self.hot.visited.fetch_or(mask, Ordering::Relaxed);
            drop(old_value);
            let _ = id;
            return Ok(());
        }
        let v = unsafe { ManuallyDrop::take(&mut value_holder) };
        Err(v)
    }

    /// Path A 用 find: pos / id / version snapshot を返す。
    /// SAFETY: AVX2 は `Shard::new` で検証済み。
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_lockfree_for_path_a(&self, key: &K, needle: u16) -> Option<(usize, usize, u32)> {
        use std::arch::x86_64::*;

        let entries_base = self.entries_ptr();
        let len = self.hot.len.load(Ordering::Acquire);
        let tags_ptr = self.tags.as_ptr() as *const u16;
        let needle_v = _mm256_set1_epi16(needle as i16);
        let mask_v = _mm256_set1_epi16(Self::SCAN_MASK as i16);

        let limit = self.tags.len();
        let mut i = 0usize;
        while i < limit {
            let v = unsafe { _mm256_loadu_si256(tags_ptr.add(i) as *const __m256i) };
            let masked = _mm256_and_si256(v, mask_v);
            let cmp = _mm256_cmpeq_epi16(masked, needle_v);
            let mut mask = _mm256_movemask_epi8(cmp) as u32;

            while mask != 0 {
                let bit = mask.trailing_zeros() as usize;
                let lane = bit >> 1;
                let pos = i + lane;
                if pos < len {
                    let t1 = self.tags[pos].load(Ordering::Acquire);
                    if (t1 & Self::SCAN_MASK) == needle
                        && let Some(found) = self.try_path_a_candidate(pos, t1, key, entries_base)
                    {
                        return Some(found);
                    }
                }
                mask = _blsr_u32(mask);
                mask = _blsr_u32(mask);
            }
            i += LANE;
        }
        None
    }

    /// SIMD scan が拾った candidate に versions[id] + K::eq を行う。
    #[inline]
    fn try_path_a_candidate(
        &self,
        pos: usize,
        t1: u16,
        key: &K,
        entries_base: *const MaybeUninit<Entry<K, V>>,
    ) -> Option<(usize, usize, u32)> {
        let id = Self::id_of(t1);
        let entry_ptr = unsafe { entries_base.add(id) as *const Entry<K, V> };
        let version_ref = &self.reader_state.versions[id];
        let v1 = version_ref.load(Ordering::Acquire);
        if v1 & 1 != 0 {
            return None;
        }
        let buf: ManuallyDrop<Entry<K, V>> =
            unsafe { ManuallyDrop::new(std::ptr::read(entry_ptr)) };
        let v2 = version_ref.load(Ordering::Acquire);
        if v1 != v2 {
            return None;
        }
        if buf.key == *key {
            return Some((pos, id, v1));
        }
        None
    }

    /// Path B (warmup install) と Path C (evict + shift + install) を writer Mutex 配下で実行。
    fn path_bc(&self, key: K, value: V, needle: u16) -> Option<(K, V)> {
        let mut state = self.hot.writer.lock();

        if let Some((pos, id)) = self.writer_find(&key, needle) {
            self.writer_update_in_place(pos, id, key, value);
            return None;
        }

        let len = self.hot.len.load(Ordering::Relaxed);
        if len < self.capacity {
            self.writer_warmup_install(len, key, value, needle);
            return None;
        }

        Some(self.writer_evict_and_install(&mut state, key, value, needle))
    }

    /// writer 内部 find: tags + versions + key 比較。Mutex 配下だが Path A と並行する
    /// ので EMPTY sentinel と versions[id] 奇数を spin-wait。
    fn writer_find(&self, key: &K, needle: u16) -> Option<(usize, usize)> {
        let entries_base = self.entries_ptr();
        let len = self.hot.len.load(Ordering::Relaxed);
        for pos in 0..len {
            loop {
                let t = self.tags[pos].load(Ordering::Acquire);
                if t == EMPTY {
                    hint::spin_loop();
                    continue;
                }
                if (t & LIVE) == 0 {
                    break;
                }
                if (t & Self::SCAN_MASK) != needle {
                    break;
                }
                let id = Self::id_of(t);
                let entry_ptr = unsafe { entries_base.add(id) as *const Entry<K, V> };
                let version_ref = &self.reader_state.versions[id];
                let mut v;
                loop {
                    v = version_ref.load(Ordering::Acquire);
                    if v & 1 == 0 {
                        break;
                    }
                    hint::spin_loop();
                }
                let buf: ManuallyDrop<Entry<K, V>> =
                    unsafe { ManuallyDrop::new(std::ptr::read(entry_ptr)) };
                let v2 = version_ref.load(Ordering::Acquire);
                if v != v2 {
                    continue;
                }
                let t2 = self.tags[pos].load(Ordering::Acquire);
                if t != t2 || (t2 & LIVE) == 0 {
                    continue;
                }
                if buf.key == *key {
                    return Some((pos, id));
                }
                break;
            }
        }
        None
    }

    /// writer Mutex 配下の既存 key 更新 (Path A 失敗後の escalate path)。
    /// Path A 同様 tag は触らない、`reader_state.versions[id]` で reader 通知。
    fn writer_update_in_place(&self, pos: usize, id: usize, key: K, value: V) {
        let entries_mut = self.entries.get();
        let entry_ptr = unsafe { (*entries_mut).as_mut_ptr().add(id) as *mut Entry<K, V> };
        let version_ref = &self.reader_state.versions[id];
        // 別 Path A の進行を spin-wait + claim CAS で排他。
        let v_claimed = loop {
            let v = version_ref.load(Ordering::Acquire);
            if v & 1 == 0
                && version_ref
                    .compare_exchange(v, v.wrapping_add(1), Ordering::Acquire, Ordering::Acquire)
                    .is_ok()
            {
                break v.wrapping_add(1);
            }
            hint::spin_loop();
        };
        // SAFETY: version 奇数で reader は bail、別 writer も Path A は CAS で弾かれる。
        unsafe {
            let old_value: V = std::ptr::read(&(*entry_ptr).value);
            std::ptr::write(&mut (*entry_ptr).value, value);
            drop(old_value);
        }
        drop(key);
        version_ref.store(v_claimed.wrapping_add(1), Ordering::Release);
        let mask = Self::vbit_mask(pos);
        self.hot.visited.fetch_or(mask, Ordering::Relaxed);
    }

    /// Path B: warmup install (len < capacity)。
    fn writer_warmup_install(&self, len: usize, key: K, value: V, needle: u16) {
        let entry_id = len as u16;
        let entries_mut = self.entries.get();
        // 新 entry に対応する version を 0 にリセット (前 lifecycle で奇数のまま終わらないが
        // sanity として)。Release で entries 書き込み前に publish。
        self.reader_state.versions[len].store(0, Ordering::Release);
        // SAFETY: writer Mutex 排他下、entries[len] は uninit slot。
        unsafe {
            let slot_ptr = (*entries_mut).as_mut_ptr().add(len) as *mut Entry<K, V>;
            std::ptr::write(slot_ptr, Entry { key, value });
        }
        let mask = Self::vbit_mask(len);
        self.hot.visited.fetch_and(!mask, Ordering::Relaxed);
        let new_tag = LIVE | (entry_id << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        fence(Ordering::Release);
        self.tags[len].store(new_tag, Ordering::Release);
        self.hot.len.store(len + 1, Ordering::Release);
    }

    /// Path C: 定常 evict + shift + install。c17s と同型 (shift loop / evict_id 再利用 /
    /// path_c_epoch bump) で、version 操作先のみ `reader_state.versions[evict_id]` に変更。
    fn writer_evict_and_install(
        &self,
        state: &mut WriterState,
        key: K,
        value: V,
        needle: u16,
    ) -> (K, V) {
        let cap = self.capacity;
        debug_assert_eq!(self.hot.len.load(Ordering::Relaxed), cap);
        if state.hand >= cap {
            state.hand = 0;
        }
        let evict_pos = self
            .scan_evict(state.hand, cap)
            .or_else(|| self.scan_evict(0, state.hand))
            .unwrap_or(state.hand);
        let evict_tag = self.read_live_tag_with_spin(evict_pos);
        let evict_id = Self::id_of(evict_tag);

        // entries[evict_id] を排他確保。Path A が並行進行中なら spin-wait + CAS。
        let entries_mut = self.entries.get();
        let evict_entry_ptr =
            unsafe { (*entries_mut).as_mut_ptr().add(evict_id) as *mut Entry<K, V> };
        let evict_version_ref = &self.reader_state.versions[evict_id];
        let v_claimed = loop {
            let v = evict_version_ref.load(Ordering::Acquire);
            if v & 1 == 0
                && evict_version_ref
                    .compare_exchange(v, v.wrapping_add(1), Ordering::Acquire, Ordering::Acquire)
                    .is_ok()
            {
                break v.wrapping_add(1);
            }
            hint::spin_loop();
        };

        // 旧 entry の (key, value) を取り出し
        // SAFETY: version 奇数で排他、Path A は CAS で弾かれる。
        let evicted_key: K = unsafe { std::ptr::read(&(*evict_entry_ptr).key) };
        let evicted_value: V = unsafe { std::ptr::read(&(*evict_entry_ptr).value) };

        // shift: tags[evict_pos+1..cap] を tags[evict_pos..cap-1] に下げる
        for i in evict_pos..(cap - 1) {
            let next_tag = self.read_live_tag_with_spin(i + 1);
            let s_mask = Self::vbit_mask(i + 1);
            let d_mask = Self::vbit_mask(i);
            let was_visited = self.hot.visited.load(Ordering::Relaxed) & s_mask != 0;
            self.hot.visited.fetch_and(!s_mask, Ordering::Relaxed);
            if was_visited {
                self.hot.visited.fetch_or(d_mask, Ordering::Relaxed);
            } else {
                self.hot.visited.fetch_and(!d_mask, Ordering::Relaxed);
            }
            self.tags[i].store(EMPTY, Ordering::Release);
            fence(Ordering::Release);
            self.tags[i].store(next_tag, Ordering::Release);
        }
        self.tags[cap - 1].store(EMPTY, Ordering::Release);

        // 新 entry を entries[evict_id] に install
        // SAFETY: version 奇数で排他確保済み、tag は LIVE が無いので reader は当たらない。
        unsafe {
            std::ptr::write(&mut (*evict_entry_ptr).key, key);
            std::ptr::write(&mut (*evict_entry_ptr).value, value);
        }
        evict_version_ref.store(v_claimed.wrapping_add(1), Ordering::Release);

        let mask = Self::vbit_mask(cap - 1);
        self.hot.visited.fetch_and(!mask, Ordering::Relaxed);
        let new_tag = LIVE | ((evict_id as u16) << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        fence(Ordering::Release);
        self.tags[cap - 1].store(new_tag, Ordering::Release);

        state.hand = if evict_pos < cap - 1 { evict_pos } else { 0 };

        // path_c_epoch bump で reader の coarse seqlock を fire。
        self.reader_state
            .path_c_epoch
            .fetch_add(1, Ordering::Release);

        (evicted_key, evicted_value)
    }

    /// hand 巡回: visited を見て立っていれば剥がす、立っていなければ evict 候補。
    fn scan_evict(&self, lo: usize, hi: usize) -> Option<usize> {
        for i in lo..hi {
            let t = loop {
                let t = self.tags[i].load(Ordering::Acquire);
                if t == EMPTY {
                    hint::spin_loop();
                    continue;
                }
                break t;
            };
            debug_assert!(
                t & LIVE != 0,
                "scan_evict: tags[{i}] was unexpectedly EMPTY/dead after spin (t = {t:#x})"
            );
            let mask = Self::vbit_mask(i);
            if self.hot.visited.load(Ordering::Relaxed) & mask != 0 {
                self.hot.visited.fetch_and(!mask, Ordering::Relaxed);
            } else {
                return Some(i);
            }
        }
        None
    }

    /// pos の LIVE tag を spin-wait しながら取得 (Path C 自身の shift transient EMPTY を吸収)。
    fn read_live_tag_with_spin(&self, pos: usize) -> u16 {
        loop {
            let t = self.tags[pos].load(Ordering::Acquire);
            if t == EMPTY {
                hint::spin_loop();
                continue;
            }
            return t;
        }
    }

    #[cfg(test)]
    pub(crate) fn live_count(&self) -> usize {
        let len = self.hot.len.load(Ordering::Acquire);
        let mut n = 0;
        for i in 0..len {
            let t = self.tags[i].load(Ordering::Acquire);
            if t & LIVE != 0 {
                n += 1;
            }
        }
        n
    }

    #[cfg(test)]
    pub(crate) fn live_ids(&self) -> Vec<usize> {
        let len = self.hot.len.load(Ordering::Acquire);
        let mut ids = Vec::new();
        for i in 0..len {
            let t = self.tags[i].load(Ordering::Acquire);
            if t & LIVE != 0 {
                ids.push(Self::id_of(t));
            }
        }
        ids
    }
}

impl<K, V> Drop for Shard<K, V> {
    fn drop(&mut self) {
        let len = self.hot.len.load(Ordering::Relaxed);
        let entries_mut = self.entries.get();
        for i in 0..len {
            let t = self.tags[i].load(Ordering::Relaxed);
            if t & LIVE != 0 {
                let id = Self::id_of(t);
                // SAFETY: LIVE ⇒ entries[id] init 済み。
                unsafe {
                    (*entries_mut)[id].assume_init_drop();
                }
            }
        }
    }
}

// ---------------- 外側 wrapper ----------------

pub const DEFAULT_SHARDS: usize = 8;

pub struct ConcurrentSieveCache<K, V, const SHARDS: usize = DEFAULT_SHARDS> {
    shards: [Shard<K, V>; SHARDS],
    hasher: Xxh3Build,
}

impl<K, V, const SHARDS: usize> ConcurrentSieveCache<K, V, SHARDS>
where
    K: Hash + Eq,
    V: Clone,
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
        let shards: [Shard<K, V>; SHARDS] = std::array::from_fn(|i| {
            let cap_i = base + if i < extra { 1 } else { 0 };
            Shard::new(cap_i)
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
        self.shards
            .iter()
            .map(|s| s.hot.len.load(Ordering::Acquire))
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.shards
            .iter()
            .all(|s| s.hot.len.load(Ordering::Acquire) == 0)
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

    impl crate::experimental::ConcurrentCacheImpl<u64, u64> for ConcurrentSieveCache<u64, u64> {
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

    crate::concurrent_suite!(ConcurrentSieveCache<u64, u64>);

    #[test]
    fn evicts_oldest_when_full_and_unvisited() {
        let cache: ConcurrentSieveCache<i32, i32, 1> = ConcurrentSieveCache::new(2);
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
        let cache: ConcurrentSieveCache<i32, i32, 1> = ConcurrentSieveCache::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((2, 20)));
    }

    #[test]
    fn all_visited_clears_bits_then_evicts() {
        let cache: ConcurrentSieveCache<i32, i32, 1> = ConcurrentSieveCache::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        cache.get(&2);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((1, 10)));
    }

    #[test]
    fn warm_up_to_steady_transition() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
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

    #[test]
    fn distinct_keys_full_per_shard_all_hit() {
        let n: u64 = 64;
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(n as usize);
        for k in 0..n {
            cache.insert(k, k * 7);
        }
        for k in 0..n {
            assert_eq!(cache.get(&k), Some(k * 7), "miss for key {k}");
        }
    }

    /// Path A 経路で update したとき id 配置不変 (= tag 不変、entries[id].value だけ書き換え)。
    #[test]
    fn update_via_path_a_preserves_id_and_tag() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.insert(3, 30);
        cache.insert(4, 40);
        let sh = cache.shard(0);
        let ids_before: Vec<usize> = sh.live_ids();
        let tags_before: Vec<u16> = (0..4).map(|i| sh.tags[i].load(Ordering::Acquire)).collect();
        cache.insert(2, 222);
        let ids_after: Vec<usize> = sh.live_ids();
        let tags_after: Vec<u16> = (0..4).map(|i| sh.tags[i].load(Ordering::Acquire)).collect();
        assert_eq!(ids_before, ids_after);
        assert_eq!(
            tags_before, tags_after,
            "Path A update が tag を変更している (c18s も c17s 同様 tag 不変が core property)"
        );
        assert_eq!(cache.get(&2), Some(222));
    }

    /// Path A は `reader_state.versions[id]` を 2 増やす (偶数→奇数→偶数)。
    #[test]
    fn path_a_increments_entry_version_by_two() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        cache.insert(1, 10);
        let sh = cache.shard(0);
        // entry_id 0 が key=1 (warmup install slot)
        let v0 = sh.reader_state.versions[0].load(Ordering::Acquire);
        cache.insert(1, 100);
        let v1 = sh.reader_state.versions[0].load(Ordering::Acquire);
        cache.insert(1, 1000);
        let v2 = sh.reader_state.versions[0].load(Ordering::Acquire);
        assert_eq!(v1, v0.wrapping_add(2));
        assert_eq!(v2, v0.wrapping_add(4));
        assert_eq!(v1 & 1, 0);
        assert_eq!(v2 & 1, 0);
        assert_eq!(cache.get(&1), Some(1000));
    }

    /// 既存キー update が visited を 1 に SET。
    #[test]
    fn update_existing_key_sets_visited_like_oracle() {
        let cache: ConcurrentSieveCache<i32, i32, 1> = ConcurrentSieveCache::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.insert(1, 11);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((2, 20)));
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }

    /// reader hit が tag を変更しない。
    #[test]
    fn reader_hit_does_not_modify_tag() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        cache.insert(1, 100);
        let sh = cache.shard(0);
        let tag_before = sh.tags[0].load(Ordering::Acquire);
        assert_eq!(cache.get(&1), Some(100));
        let tag_after = sh.tags[0].load(Ordering::Acquire);
        assert_eq!(tag_before, tag_after);
        let mask = Shard::<u64, u64>::vbit_mask(0);
        assert!(sh.hot.visited.load(Ordering::Acquire) & mask != 0);
    }

    /// Path C で eviction が起きたあと、新 entry の id は evicted entry の id を再利用、
    /// tags 配列上の position は cap-1 (末尾) に install される。`path_c_epoch` も bump。
    #[test]
    fn evict_reuses_id_at_tail_position_and_bumps_epoch() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        for k in 0..4u64 {
            cache.insert(k, k * 10);
        }
        let sh = cache.shard(0);
        let epoch_before = sh.reader_state.path_c_epoch.load(Ordering::Acquire);
        let ids_before: Vec<usize> = sh.live_ids();
        assert_eq!(sh.live_count(), 4);
        assert_eq!(ids_before, vec![0, 1, 2, 3]);
        let evicted = cache.insert(99, 9900);
        assert!(evicted.is_some());
        assert_eq!(sh.live_count(), 4);
        let last_tag = sh.tags[3].load(Ordering::Acquire);
        let last_id = Shard::<u64, u64>::id_of(last_tag);
        assert_eq!(last_id, 0, "Path C で id 再利用していない");
        let epoch_after = sh.reader_state.path_c_epoch.load(Ordering::Acquire);
        assert!(
            epoch_after > epoch_before,
            "Path C で path_c_epoch が bump されていない"
        );
    }

    /// 並行不変条件 (c17s と同型)。
    #[test]
    fn concurrent_invariants_under_zipf() {
        use crate::workload::zipf::ZipfGen;
        let cap = 256usize;
        let cache: Arc<ConcurrentSieveCache<u64, u64, 8>> =
            Arc::new(ConcurrentSieveCache::new(cap));

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
            assert_eq!(live, sh.hot.len.load(Ordering::Acquire));
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

    /// Path A は eviction を起こさない。
    #[test]
    fn path_a_does_not_evict() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        for k in 0..4u64 {
            assert_eq!(cache.insert(k, k), None);
        }
        for _ in 0..100 {
            for k in 0..4u64 {
                assert_eq!(cache.insert(k, k * 1000), None);
            }
        }
        for k in 0..4u64 {
            assert_eq!(cache.get(&k), Some(k * 1000));
        }
    }

    /// sieve_orig (oracle) と外部一致: 1 shard 同期で SIEVE 意味論完全一致。
    #[test]
    fn matches_sieve_orig_externally_1shard() {
        use crate::experimental::sieve_orig::SieveCache as Orig;
        let cap = 64usize;
        let mut a: Orig<u64, u64> = Orig::new(cap);
        let b: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(cap);
        for k in 0..10_000u64 {
            let key = (k.wrapping_mul(2654435761)) % 256;
            let _ = a.insert(key, key);
            let _ = b.insert(key, key);
        }
        for k in 0..256u64 {
            assert_eq!(
                a.get(&k).copied(),
                b.get(&k),
                "1-shard で sieve_orig と c18s が key {k} で食い違う"
            );
        }
    }

    /// c18s の bit layout: Slot16 (Entry sizeof=16, ID_SHIFT=4)、HASH 9 bit (c17s と同等、
    /// bit 位置のみ移動: ID_MASK=0x03F0, HASH_MASK=0x7C0F)。
    #[test]
    fn bit_layout_exclusivity_u64_u64() {
        type S = Shard<u64, u64>;
        // Entry<u64,u64> = u64 + u64 = 16、align(16) で sizeof = 16 ⇒ ID_SHIFT = 4
        assert_eq!(std::mem::size_of::<Entry<u64, u64>>(), 16);
        assert_eq!(S::ID_SHIFT, 4);
        // ID_MASK = 63 << 4 = 0x03F0
        assert_eq!(S::ID_MASK, 0x03F0);
        // HASH_MASK = 0x7FFF & !0x03F0 = 0x7C0F、9 bit (4 low + 5 high)
        assert_eq!(S::HASH_MASK, 0x7C0F);
        assert_eq!(S::HASH_MASK.count_ones(), 9);
        assert_eq!(S::SCAN_MASK, LIVE | S::HASH_MASK);

        // LIVE | ID | HASH の 3 区画で 0xFFFF を埋め切る (c18s も VERSION 不在)。
        assert_eq!(LIVE | S::ID_MASK | S::HASH_MASK, 0xFFFF);
        assert_eq!(LIVE & S::ID_MASK, 0);
        assert_eq!(LIVE & S::HASH_MASK, 0);
        assert_eq!(S::ID_MASK & S::HASH_MASK, 0);
    }

    /// `ShardHot` の sizeof / alignment 契約 (path_c_epoch を取り除いても 64B 維持)。
    #[test]
    fn shard_hot_layout_contract() {
        assert_eq!(std::mem::size_of::<ShardHot>(), 64);
        assert_eq!(std::mem::align_of::<ShardHot>(), 64);
    }

    /// `ReaderState` の sizeof / alignment 契約: path_c_epoch line 0 専有 + versions 4 line。
    #[test]
    fn reader_state_layout_contract() {
        assert_eq!(std::mem::size_of::<ReaderState>(), 64 + 256);
        assert_eq!(std::mem::align_of::<ReaderState>(), 64);
        // path_c_epoch は offset 0 (line 0 の先頭)
        let dummy = ReaderState {
            path_c_epoch: AtomicU64::new(0),
            _pad0: [0u8; 56],
            versions: std::array::from_fn(|_| AtomicU32::new(0)),
        };
        let base = &dummy as *const _ as usize;
        let epoch_off = (&dummy.path_c_epoch as *const _ as usize) - base;
        let versions_off = (&dummy.versions as *const _ as usize) - base;
        assert_eq!(epoch_off, 0);
        assert_eq!(versions_off, 64);
    }
}
