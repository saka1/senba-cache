//! `sieve_c10s`: c8 から **VISITED bit を tag 列の外に分離** した shard 並行 SIEVE。
//!
//! # c8 との差分 (本 variant の核心)
//!
//! c8 は `tags: Box<[AtomicU16]>` の 1 bit に VISITED を同居させていた。
//! reader hit 時の `tags[pos].fetch_or(VISITED, Relaxed)` がその 32 byte
//! AVX2 chunk (= cache line 1 本に乗る 16 lane) を MESI Modified に遷移させ、
//! **同じ chunk を AVX2 scan で読んでいる他 reader 全員に invalidate** を撒いていた。
//! `docs/reports/2026-05-08-single-shard-baseline.md` の adversarial-hot
//! read-only で c8 が 1T 73.9 → 16T 31.1 Mops と plateau した主要因。
//!
//! c10s は **VISITED を別 `Box<[AtomicU64]>`** に bit-packed (pos 単位) で持つ。
//! reader hit は visited 配列の word に対して `fetch_or` するだけで tags 列には
//! 一切書き込まない。結果:
//!
//! - tags 列 = reader からは Acquire **load 専用** → MESI Shared 維持 → AVX2 scan
//!   の cache line が invalidate を被らない
//! - visited 列 = 別ヒープ Box で物理的に別 cache line に乗る → tags scan 路を汚染しない
//!
//! visited 列内部の ping-pong (= 同 hot key への複数 reader の `fetch_or` が
//! 同 word 1 本に集中) は依然残るので、adversarial-hot は完全には解消しない。
//! 報告 §5 attack 順位の (1) を単独で評価することが本 variant の目的。
//!
//! # Tag bit layout
//!
//! c8 と c10s で 1 bit ずれる:
//!
//! - c8:    `LIVE (1) | VISITED (1) | id (6) | hash (8)` = 16 bit
//! - c10s:  `LIVE (1) | id (6) | hash (9)` = 16 bit (旧 VISITED bit は hash に転用)
//!
//! HASH_MASK が 8b → 9b に拡張されることで AVX2 scan の false-positive
//! (= tag マッチしたが key 違いの seqlock candidate) が半減する副次利得を得る。
//!
//! # writer 経路の差分
//!
//! - `writer_scan_evict` / `writer_first_live`: c8 では VISITED only / LIVE 無しの
//!   "phantom tag" が発生しうるので二次正規化 (`tags[i].store(EMPTY)`) を入れていたが、
//!   c10s では reader が tag を変更しないので phantom 概念自体が消える。コード簡素化。
//! - `writer_evict_one` / `writer_install` / `writer_update_in_place` / `writer_compact`:
//!   各 pos の visited bit を `fetch_and(!mask, Relaxed)` で必ずクリアする処理を追加。
//! - `writer_update_in_place`: 既存 pos の更新後 visited を **1 に SET** する
//!   (sieve_orig oracle が `node.freq = 1` するのと一致、c8 の `old | VISITED` と同じ
//!   挙動)。visited の SET は EMPTY 窓の外で発火させ、reader の seqlock-fail miss を
//!   最小化する。
//!
//! # 継承する性質
//!
//! - `K, V: Copy` 制約と torn-read 非伝播 (= reader が seqlock 検査で torn 値を破棄)
//! - seqlock-via-tag (`t1 == t2 && (t2 & LIVE) != 0`)
//! - `UnsafeCell<Box<[MaybeUninit<Entry>]>>` の writer 排他下 mutate
//! - `K, V: Copy` のため Drop / Clone が走らない (c8 と同レベルの soundness gap)
//! - miri 並行テストの抑制 (`#[cfg(not(miri))]`)、単一スレッド経路は miri pass

use parking_lot::Mutex;
use senba::Xxh3Build;
use std::cell::UnsafeCell;
use std::hash::{BuildHasher, Hash};
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering, fence};

const EMPTY: u16 = 0;
const LIVE: u16 = 0x8000;
/// AVX2 1 chunk = 32 byte = 16 u16 lane。
const LANE: usize = 16;
/// 6-bit entry_id の構造的上限。per_shard はこの値以下でなければならない。
pub const MAX_PER_SHARD: usize = 64;

/// `sizeof(Entry)` から ID_SHIFT (= log2(sizeof)) を const-eval で算出。
const fn id_shift_from_entry_size(s: usize) -> u32 {
    assert!(
        s.is_power_of_two(),
        "sieve_c10s: sizeof(Entry<K,V>) must be a power of two"
    );
    assert!(s <= 256, "sieve_c10s: sizeof(Entry<K,V>) must be <= 256");
    s.trailing_zeros()
}

const fn id_mask_from_shift(id_shift: u32) -> u16 {
    ((MAX_PER_SHARD - 1) as u16) << id_shift
}

/// c8 の `0x3FFF & !id_mask` (= 14 bit) から 1 bit 拡張して `0x7FFF & !id_mask` (= 15 bit)。
/// 旧 VISITED bit (0x4000) を hash 領域に組み入れる。
const fn hash_mask_from_id_mask(id_mask: u16) -> u16 {
    0x7FFF & !id_mask
}

struct Entry<K, V> {
    key: K,
    value: V,
}

type EntriesArena<K, V> = UnsafeCell<Box<[MaybeUninit<Entry<K, V>>]>>;

/// 1 shard 分の並行 SIEVE。
pub struct Shard<K, V> {
    capacity: usize,
    /// tag 列。reader が atomic load (write 無し)、writer のみが atomic store。
    /// VISITED bit は持たない (= visited 列に分離)。
    tags: Box<[AtomicU16]>,
    /// VISITED bit を pos 単位で bit-packed した独立配列。
    /// reader hit 時の `fetch_or` はこちらに発火し、tags 列を汚染しない。
    /// `Box::new` 経由で別ヒープ確保 → 物理的に別 cache line に乗る (期待)。
    visited: Box<[AtomicU64]>,
    /// entries arena。`writer.lock()` を取った間のみ書き込んでよい。
    entries: EntriesArena<K, V>,
    /// reader からも見える書き込み境界。`0..tail` が scan 範囲。
    tail: AtomicUsize,
    /// live entry 数。
    len: AtomicUsize,
    /// writer 排他状態。`hand` は writer のみが触る。
    writer: Mutex<WriterState>,
}

struct WriterState {
    hand: usize,
}

// SAFETY: c8 と同じ。UnsafeCell<entries> は writer Mutex 配下でのみ書き込まれ、
// reader は seqlock-via-tag プロトコルでアクセスする。
unsafe impl<K: Send, V: Send> Send for Shard<K, V> {}
unsafe impl<K: Send + Sync, V: Send + Sync> Sync for Shard<K, V> {}

impl<K, V> Shard<K, V> {
    const ENTRY_SIZE: usize = std::mem::size_of::<Entry<K, V>>();
    const ID_SHIFT: u32 = id_shift_from_entry_size(Self::ENTRY_SIZE);
    const ID_MASK: u16 = id_mask_from_shift(Self::ID_SHIFT);
    const HASH_MASK: u16 = hash_mask_from_id_mask(Self::ID_MASK);
    const SCAN_MASK: u16 = LIVE | Self::HASH_MASK;

    #[inline]
    fn id_of(tag: u16) -> usize {
        ((tag & Self::ID_MASK) >> Self::ID_SHIFT) as usize
    }

    /// pos に対応する visited word index と bit mask。
    #[inline]
    fn vbit(pos: usize) -> (usize, u64) {
        (pos >> 6, 1u64 << (pos & 63))
    }

    /// reader-safe な capacity 取得。
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// reader-safe な live entry 数の取得。
    pub fn len(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<K, V> Shard<K, V>
where
    K: Hash + Eq + Copy,
    V: Copy,
{
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        assert!(
            capacity <= MAX_PER_SHARD,
            "per-shard capacity ({capacity}) must be <= {MAX_PER_SHARD} (6-bit ID limit)"
        );
        let raw = capacity.checked_mul(2).expect("capacity * 2 overflow");
        let order_cap = ((raw + LANE - 1) & !(LANE - 1)).max(LANE);

        let mut tags_vec: Vec<AtomicU16> = Vec::with_capacity(order_cap);
        for _ in 0..order_cap {
            tags_vec.push(AtomicU16::new(EMPTY));
        }

        // visited は order_cap bit ぶんを 64 bit ずつ詰める。
        // order_cap <= 128 (cap=64 * 2) のとき word 数 = 2、それでも別ヒープ確保で
        // 物理的に tags と異なる cache line に乗ることを期待する。
        let visited_words = order_cap.div_ceil(64);
        let mut vis_vec: Vec<AtomicU64> = Vec::with_capacity(visited_words);
        for _ in 0..visited_words {
            vis_vec.push(AtomicU64::new(0));
        }

        let mut entries_vec: Vec<MaybeUninit<Entry<K, V>>> = Vec::with_capacity(capacity);
        entries_vec.resize_with(capacity, MaybeUninit::uninit);

        Self {
            capacity,
            tags: tags_vec.into_boxed_slice(),
            visited: vis_vec.into_boxed_slice(),
            entries: UnsafeCell::new(entries_vec.into_boxed_slice()),
            tail: AtomicUsize::new(0),
            len: AtomicUsize::new(0),
            writer: Mutex::new(WriterState { hand: 0 }),
        }
    }

    /// hash → tag bit spread。c8 の 8 bit から 9 bit に拡張。
    #[inline]
    fn needle_from_hash(hash: u64) -> u16 {
        // 高位 9 bit を取り出す。
        let h9 = ((hash >> 55) as u16) & 0x01FF;
        let s = Self::ID_SHIFT;
        // hash 9 bit を ID_MASK の左右に分割して詰める。ID 幅 = 6 bit なので
        // 「s 未満は低位、s..s+6 は ID 領域、s+6 以上は高位」の構造を保つ。
        let spread = if s >= 9 {
            // hash bits 全てが ID の下に収まる ((entry size が大きい K,V のとき))
            h9
        } else {
            let low_mask: u16 = ((1u32 << s) - 1) as u16;
            let low = h9 & low_mask;
            let high = (h9 & !low_mask) << 6;
            low | high
        };
        LIVE | spread
    }

    /// reader 用: 候補 1 件ごとに seqlock dance を回して値を返す。
    /// hit したら **visited 配列の bit** を `fetch_or(Relaxed)` で立てる。
    fn find_get(&self, key: &K, needle: u16) -> Option<V> {
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        {
            if std::is_x86_feature_detected!("avx2") {
                // SAFETY: avx2 runtime detect 済み、bmi1 は AVX2 capable CPU の前提。
                return unsafe { self.find_get_avx2(key, needle) };
            }
        }
        self.find_get_scalar(key, needle)
    }

    fn find_get_scalar(&self, key: &K, needle: u16) -> Option<V> {
        let tail = self.tail.load(Ordering::Acquire);
        for i in 0..tail {
            if let Some(v) = self.try_candidate(i, key, needle) {
                return Some(v);
            }
        }
        None
    }

    #[cfg(all(target_arch = "x86_64", not(miri)))]
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_get_avx2(&self, key: &K, needle: u16) -> Option<V> {
        use std::arch::x86_64::*;

        let tail = self.tail.load(Ordering::Acquire);
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
                if pos < tail
                    && let Some(val) = self.try_candidate(pos, key, needle)
                {
                    return Some(val);
                }
                mask = _blsr_u32(mask);
                mask = _blsr_u32(mask);
            }
            i += LANE;
        }
        None
    }

    /// 1 候補に対する seqlock dance。スカラー / AVX2 path 共通。
    /// hit 時の VISITED 立ては **visited 配列** に対して行う (c8 との唯一の意味的差分)。
    #[inline]
    fn try_candidate(&self, pos: usize, key: &K, needle: u16) -> Option<V> {
        let t1 = self.tags[pos].load(Ordering::Acquire);
        if (t1 & Self::SCAN_MASK) != needle {
            return None;
        }
        let id = Self::id_of(t1);
        let entries_base = self.entries_ptr();
        // SAFETY: c8 と同じ。Copy 制約で torn read 非伝播。
        let entry = unsafe { std::ptr::read_volatile(entries_base.add(id) as *const Entry<K, V>) };
        let t2 = self.tags[pos].load(Ordering::Acquire);
        if t2 != t1 || (t2 & LIVE) == 0 {
            return None;
        }
        if entry.key == *key {
            let (w, b) = Self::vbit(pos);
            self.visited[w].fetch_or(b, Ordering::Relaxed);
            return Some(entry.value);
        }
        None
    }

    /// `entries` Box の先頭 raw pointer。
    #[inline]
    fn entries_ptr(&self) -> *const MaybeUninit<Entry<K, V>> {
        // SAFETY: UnsafeCell::get 経由で slice の先頭 pointer を返す。
        unsafe { (*self.entries.get()).as_ptr() }
    }

    pub fn contains(&self, key: &K, hash: u64) -> bool {
        self.find_get(key, Self::needle_from_hash(hash)).is_some()
    }

    pub fn get_by_hash(&self, key: &K, hash: u64) -> Option<V> {
        self.find_get(key, Self::needle_from_hash(hash))
    }

    /// writer (insert)。
    pub fn insert(&self, key: K, value: V, hash: u64) -> Option<(K, V)> {
        let needle = Self::needle_from_hash(hash);
        let mut state = self.writer.lock();

        if let Some(pos) = self.writer_find(&key, needle) {
            self.writer_update_in_place(pos, key, value);
            return None;
        }

        let len = self.len.load(Ordering::Relaxed);
        let (evicted, entry_id): (Option<(K, V)>, u16) = if len < self.capacity {
            (None, len as u16)
        } else {
            let (kv, freed_id) = self.writer_evict_one(&mut state);
            (Some(kv), freed_id)
        };

        let tail = self.tail.load(Ordering::Relaxed);
        if tail == self.tags.len() {
            self.writer_compact(&mut state);
        }

        let pos = self.tail.load(Ordering::Relaxed);
        let new_tag = LIVE | (entry_id << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        let entries_mut = self.entries.get();
        // SAFETY: writer 排他下、entry_id は未使用 slot。
        unsafe {
            (*entries_mut)[entry_id as usize].write(Entry { key, value });
        }
        // 新 install 時は visited bit を 0 で開始。
        let (w, b) = Self::vbit(pos);
        self.visited[w].fetch_and(!b, Ordering::Relaxed);
        fence(Ordering::Release);
        self.tags[pos].store(new_tag, Ordering::Release);
        self.tail.store(pos + 1, Ordering::Release);
        self.len.fetch_add(1, Ordering::Release);

        evicted
    }

    /// writer 内部 find: Mutex 配下で tags を Relaxed 読み + key 比較。
    fn writer_find(&self, key: &K, needle: u16) -> Option<usize> {
        let tail = self.tail.load(Ordering::Relaxed);
        let entries_base = self.entries_ptr();
        for i in 0..tail {
            let t = self.tags[i].load(Ordering::Relaxed);
            if (t & Self::SCAN_MASK) != needle {
                continue;
            }
            let id = Self::id_of(t);
            // SAFETY: t が SCAN_MASK 一致 ⇒ LIVE 立 ⇒ entries[id] init 済み。
            let e: &Entry<K, V> = unsafe {
                let slot_ptr = entries_base.add(id);
                (*slot_ptr).assume_init_ref()
            };
            if e.key == *key {
                return Some(i);
            }
        }
        None
    }

    /// 既存キー更新: c8 と同じく一度 EMPTY を経由してから旧 tag を Release 公開。
    /// visited は更新後 1 に SET (sieve_orig の `freq = 1` と c8 の `old | VISITED` に一致)。
    /// visited の SET は **EMPTY 窓の外** に置き、reader の seqlock-fail miss を最小化する。
    fn writer_update_in_place(&self, pos: usize, key: K, value: V) {
        let old = self.tags[pos].load(Ordering::Relaxed);
        let id = Self::id_of(old);
        // 1. tag 無効化 (EMPTY 窓 開始)
        self.tags[pos].store(EMPTY, Ordering::Release);
        // 2. entries 上書き
        let entries_mut = self.entries.get();
        // SAFETY: writer 排他下、id は LIVE tag が指していた有効 slot。
        unsafe {
            (*entries_mut)[id].write(Entry { key, value });
        }
        fence(Ordering::Release);
        // 3. 旧 tag を再公開 (EMPTY 窓 終了)。
        //    c10s では LIVE | id | hash の 3 区画のみで VISITED bit が無いので
        //    そのまま old を書き戻す (= 同 hash 部分)。
        self.tags[pos].store(old, Ordering::Release);
        // 4. visited を SET (窓外)。reader の fetch_or と race してもどちらも 1 を立てるので無害。
        let (w, b) = Self::vbit(pos);
        self.visited[w].fetch_or(b, Ordering::Relaxed);
    }

    /// SIEVE hand 巡回。visited bit は別 array にあるので tags は LIVE 判定のみ。
    fn writer_evict_one(&self, state: &mut WriterState) -> ((K, V), u16) {
        debug_assert!(self.len.load(Ordering::Relaxed) > 0);
        let tail = self.tail.load(Ordering::Relaxed);
        if state.hand >= tail {
            state.hand = 0;
        }

        let pos = self
            .writer_scan_evict(state.hand, tail)
            .or_else(|| self.writer_scan_evict(0, state.hand))
            .or_else(|| self.writer_first_live(state.hand, tail))
            .or_else(|| self.writer_first_live(0, state.hand))
            .expect("len > 0 implies at least one live slot");
        self.writer_do_evict(state, pos, tail)
    }

    /// hand 巡回: visited を見て立っていれば剥がす、立っていなければ evict 候補。
    fn writer_scan_evict(&self, lo: usize, hi: usize) -> Option<usize> {
        for i in lo..hi {
            let t = self.tags[i].load(Ordering::Relaxed);
            if t & LIVE == 0 {
                continue;
            }
            let (w, b) = Self::vbit(i);
            if self.visited[w].load(Ordering::Relaxed) & b != 0 {
                self.visited[w].fetch_and(!b, Ordering::Relaxed);
            } else {
                return Some(i);
            }
        }
        None
    }

    fn writer_first_live(&self, lo: usize, hi: usize) -> Option<usize> {
        for i in lo..hi {
            let t = self.tags[i].load(Ordering::Relaxed);
            if t & LIVE != 0 {
                return Some(i);
            }
        }
        None
    }

    fn writer_do_evict(&self, state: &mut WriterState, pos: usize, tail: usize) -> ((K, V), u16) {
        let t = self.tags[pos].load(Ordering::Relaxed);
        debug_assert!(t & LIVE != 0);
        let id = Self::id_of(t) as u16;
        // 1. tag 無効化を先に publish
        self.tags[pos].store(EMPTY, Ordering::Release);
        // 2. visited bit クリア (entry が消えるので立っていればそれを払う)
        let (w, b) = Self::vbit(pos);
        self.visited[w].fetch_and(!b, Ordering::Relaxed);
        fence(Ordering::Release);
        // 3. 旧 entry を取り出す
        let entries_mut = self.entries.get();
        // SAFETY: LIVE tag が指していた有効 slot。
        let entry = unsafe { (*entries_mut)[id as usize].assume_init_read() };
        // 4. len 更新と hand 進め
        let prev_len = self.len.load(Ordering::Relaxed);
        self.len.store(prev_len - 1, Ordering::Release);
        let mut next_hand = pos + 1;
        if next_hand >= tail {
            next_hand = 0;
        }
        state.hand = next_hand;
        ((entry.key, entry.value), id)
    }

    /// tags の前詰めに合わせて visited bit も remap。
    fn writer_compact(&self, state: &mut WriterState) {
        let old_tail = self.tail.load(Ordering::Relaxed);
        let old_hand = state.hand.min(old_tail);
        let mut new_hand: Option<usize> = None;
        let mut write = 0usize;

        for old_pos in 0..old_tail {
            let t = self.tags[old_pos].load(Ordering::Relaxed);
            if t & LIVE == 0 {
                continue;
            }
            if new_hand.is_none() && old_pos >= old_hand {
                new_hand = Some(write);
            }
            // 元の visited bit を読み取り、まずは write 位置に展開する準備。
            let (ow, ob) = Self::vbit(old_pos);
            let was_visited = self.visited[ow].load(Ordering::Relaxed) & ob != 0;
            if write != old_pos {
                // tag move: reader が中間状態で torn read を採用しないよう一旦 EMPTY を経由。
                self.tags[write].store(EMPTY, Ordering::Release);
                self.tags[old_pos].store(EMPTY, Ordering::Release);
                self.tags[write].store(t, Ordering::Release);
                // 旧位置の visited を落として新位置に再現。
                self.visited[ow].fetch_and(!ob, Ordering::Relaxed);
            }
            let (nw, nb) = Self::vbit(write);
            if was_visited {
                self.visited[nw].fetch_or(nb, Ordering::Relaxed);
            } else {
                self.visited[nw].fetch_and(!nb, Ordering::Relaxed);
            }
            write += 1;
        }
        for i in write..old_tail {
            self.tags[i].store(EMPTY, Ordering::Release);
            // tail 後方の visited は冗長クリア (どうせ install 時に 0 で始まる)。
            let (w, b) = Self::vbit(i);
            self.visited[w].fetch_and(!b, Ordering::Relaxed);
        }

        let len = self.len.load(Ordering::Relaxed);
        self.tail.store(write, Ordering::Release);
        state.hand = if len == 0 { 0 } else { new_hand.unwrap_or(0) };
        debug_assert_eq!(len, write);
    }

    #[cfg(test)]
    pub(crate) fn live_count(&self) -> usize {
        let tail = self.tail.load(Ordering::Acquire);
        let mut n = 0;
        for i in 0..tail {
            if self.tags[i].load(Ordering::Acquire) & LIVE != 0 {
                n += 1;
            }
        }
        n
    }

    #[cfg(test)]
    pub(crate) fn live_ids(&self) -> Vec<usize> {
        let tail = self.tail.load(Ordering::Acquire);
        let mut ids = Vec::new();
        for i in 0..tail {
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
        let tail = self.tail.load(Ordering::Relaxed);
        let entries_mut = self.entries.get();
        for i in 0..tail {
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
    K: Hash + Eq + Copy,
    V: Copy,
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
            .map(|s| s.len.load(Ordering::Acquire))
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.shards
            .iter()
            .all(|s| s.len.load(Ordering::Acquire) == 0)
    }

    pub fn contains_key(&self, key: &K) -> bool {
        let h = self.hasher.hash_one(key);
        self.shards[Self::shard_of_hash(h)].contains(key, h)
    }

    pub fn get(&self, key: &K) -> Option<V> {
        let h = self.hasher.hash_one(key);
        let s = &self.shards[Self::shard_of_hash(h)];
        s.find_get(key, Shard::<K, V>::needle_from_hash(h))
    }

    pub fn insert(&self, key: K, value: V) -> Option<(K, V)> {
        let h = self.hasher.hash_one(key);
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
    //! c8 のテスト群を mirror + visited 分離不変条件 test。

    use super::*;
    use std::sync::Arc;

    const TEST_SHARDS: usize = DEFAULT_SHARDS;

    #[test]
    fn cache_initially_empty() {
        let cache: ConcurrentSieveCache<i32, i32> = ConcurrentSieveCache::new(TEST_SHARDS * 4);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.capacity(), TEST_SHARDS * 4);
        assert!(cache.is_empty());
    }

    #[test]
    fn insert_then_get() {
        let cache: ConcurrentSieveCache<i32, i32> = ConcurrentSieveCache::new(TEST_SHARDS * 4);
        assert!(cache.insert(1, 10).is_none());
        assert_eq!(cache.get(&1), Some(10));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn get_missing_returns_none() {
        let cache: ConcurrentSieveCache<i32, i32> = ConcurrentSieveCache::new(TEST_SHARDS * 4);
        cache.insert(1, 10);
        assert_eq!(cache.get(&2), None);
    }

    #[test]
    fn contains_key_reflects_insertions() {
        let cache: ConcurrentSieveCache<i32, i32> = ConcurrentSieveCache::new(TEST_SHARDS * 4);
        assert!(!cache.contains_key(&1));
        cache.insert(1, 10);
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
    }

    #[test]
    fn insert_existing_key_updates_value() {
        let cache: ConcurrentSieveCache<i32, i32> = ConcurrentSieveCache::new(TEST_SHARDS * 4);
        cache.insert(1, 10);
        assert!(cache.insert(1, 20).is_none());
        assert_eq!(cache.get(&1), Some(20));
        assert_eq!(cache.len(), 1);
    }

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
    fn total_capacity_is_respected_under_churn() {
        let cap = TEST_SHARDS * 16;
        let cache: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::new(cap);
        for k in 0..10_000u64 {
            cache.insert(k, k);
            assert!(cache.len() <= cap);
        }
        assert_eq!(cache.len(), cap);
    }

    #[test]
    fn churn_keeps_a_full_capacity_set() {
        let cap = TEST_SHARDS * 16;
        let cache: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::new(cap);
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
        let _: ConcurrentSieveCache<u64, u64> = ConcurrentSieveCache::new(TEST_SHARDS - 1);
    }

    #[test]
    #[should_panic]
    fn non_power_of_two_shards_panics() {
        let _: ConcurrentSieveCache<u64, u64, 3> = ConcurrentSieveCache::new(9);
    }

    #[test]
    #[should_panic]
    fn per_shard_above_max_panics() {
        let _: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(65);
    }

    #[test]
    fn per_shard_at_max_works() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(64);
        for k in 0..200u64 {
            cache.insert(k, k * 11);
        }
        assert_eq!(cache.len(), 64);
    }

    #[test]
    fn works_with_non_default_shards() {
        let cache_2: ConcurrentSieveCache<u64, u64, 2> = ConcurrentSieveCache::new(64);
        let cache_16: ConcurrentSieveCache<u64, u64, 16> = ConcurrentSieveCache::new(64);
        for k in 0..1000u64 {
            cache_2.insert(k, k);
            cache_16.insert(k, k);
        }
        assert!(cache_2.len() <= 64);
        assert!(cache_16.len() <= 64);
        assert_eq!(cache_2.capacity(), 64);
        assert_eq!(cache_16.capacity(), 64);
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
                "1-shard で sieve_orig と c10s が key {k} で食い違う"
            );
        }
    }

    /// j8 (single-thread oracle) と 1 shard 同期で外部一致。
    #[test]
    fn matches_j8_externally_1shard() {
        use crate::experimental::sieve_j8::SieveCache as J8;
        let cap = 64usize;
        let mut a: J8<u64, u64, 1> = J8::new(cap);
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
                "1-shard で j8 と c10s が key {k} で食い違う"
            );
        }
    }

    #[test]
    fn bit_layout_exclusivity_u64_u64() {
        type S = Shard<u64, u64>;
        // Entry<u64,u64> は 16 byte ⇒ ID_SHIFT = 4
        assert_eq!(S::ID_SHIFT, 4);
        assert_eq!(S::ID_MASK, 0x03f0);
        // hash mask は LIVE と ID を除いた 15 bit から ID 4-9 を抜いた 9 bit。
        // 0x7FFF & !0x03f0 = 0x7c0f
        assert_eq!(S::HASH_MASK, 0x7c0f);
        assert_eq!(S::SCAN_MASK, LIVE | S::HASH_MASK);

        // LIVE | ID | HASH の 3 区画で 0x7FFF を埋め切る。
        assert_eq!(LIVE | S::ID_MASK | S::HASH_MASK, 0xFFFF);
        assert_eq!(LIVE & S::ID_MASK, 0);
        assert_eq!(LIVE & S::HASH_MASK, 0);
        assert_eq!(S::ID_MASK & S::HASH_MASK, 0);
        // hash mask の有意 bit 数は 9。
        assert_eq!(S::HASH_MASK.count_ones(), 9);
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
    fn compact_preserves_id_mapping() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        for k in 0..40u64 {
            cache.insert(k, k * 13);
        }
        let alive: u64 = (0..40u64)
            .filter(|&k| cache.get(&k) == Some(k * 13))
            .count() as u64;
        assert_eq!(alive, 4);
    }

    /// 既存キーへの insert (= update) は visited を 1 に SET する。
    /// sieve_orig が `node.freq = 1` する仕様と一致しなければならない。
    /// この test が **失敗するなら writer_update_in_place の visited 処理が間違っている**。
    #[test]
    fn update_existing_key_sets_visited_like_oracle() {
        let cache: ConcurrentSieveCache<i32, i32, 1> = ConcurrentSieveCache::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        // (1) を update (= 既存キーへの insert) → visited=1 にする (oracle と同じ)
        cache.insert(1, 11);
        // (3) を新規 insert すると、visited=1 の (1) は survive、visited=0 の (2) が evict される
        let evicted = cache.insert(3, 30);
        assert_eq!(
            evicted,
            Some((2, 20)),
            "update が visited を SET しないと (1) が evict されてしまう"
        );
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }

    /// visited 分離が機能している不変条件:
    /// reader hit 後、tags[pos] の値は変化しない (以前は VISITED bit が立った)。
    #[test]
    fn reader_hit_does_not_modify_tag() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        cache.insert(1, 100);
        let sh = cache.shard(0);
        let tag_before = sh.tags[0].load(Ordering::Acquire);
        // hit 1 回
        assert_eq!(cache.get(&1), Some(100));
        let tag_after = sh.tags[0].load(Ordering::Acquire);
        assert_eq!(
            tag_before, tag_after,
            "reader hit が tag を変更している (visited 分離が崩れている)"
        );
        // visited 配列側には bit が立っているはず
        let (w, b) = Shard::<u64, u64>::vbit(0);
        assert!(
            sh.visited[w].load(Ordering::Acquire) & b != 0,
            "visited bit が立っていない"
        );
    }

    #[cfg(not(miri))]
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
            assert_eq!(live, sh.len.load(Ordering::Acquire));
            let mut sorted = ids.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), ids.len(), "shard {i} で id 重複");
            sum_live += live;
        }
        assert_eq!(sum_live, total_len);

        for k in 0..1024u64 {
            if let Some(v) = cache.get(&k) {
                assert_eq!(v, k, "key {k} の value が破壊されている");
            }
        }
    }

    #[test]
    fn self_insert_self_get_visibility() {
        let cache: ConcurrentSieveCache<u64, u64, 8> = ConcurrentSieveCache::new(256);
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
