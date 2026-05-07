//! `sieve_c8`: `sieve_j8` の並行版 (read lock-free + write per-shard Mutex).
//!
//! # 設計の要約
//!
//! - **read path lock-free**: `get` / `contains_key` / `len` 等は `&self`、
//!   shard 内では `tags: Box<[AtomicU16]>` への atomic load + `fetch_or` のみ。
//!   Mutex は取らない。
//! - **write path per-shard Mutex**: `insert` は `parking_lot::Mutex<WriterState>`
//!   を取って既存 j8 の eviction ロジックを直列実行する。
//! - **seqlock-via-tag**: j8 の tag は `LIVE | VISITED | id (6 bit) | hash (8 bit)`
//!   を 16 bit に詰めた値で、**slot に何が起きても tag が変わる**ように設計されている。
//!   c8 はこれを seqlock の sequence number 兼 locator として再利用する。
//!   reader は tag を 1 回読んで候補にし、entries[id] を copy してから tag を
//!   再 load して **t1 == t2 && LIVE が立っている** 場合だけ結果を採用する。
//!
//! # API 上の制約 (初版)
//!
//! ## `K, V: Copy`
//!
//! reader が Mutex を取らずに entries[id] を raw pointer 経由で copy する設計のため、
//! writer が同 slot を evict / 上書き中に reader が memory を読み取る race が
//! 構造上発生する。`Copy` 制約はこの race の被害を「値が不定 (= torn) になりうるが、
//! Drop / Clone が走らないので UB が伝播しない」レベルに抑えるためのもの:
//!
//! - `String` / `Vec<u8>` のような owned 型は内部 (ptr, len, cap) が torn になると、
//!   後続の Drop が無効ポインタを free して UB に至る。
//! - `Copy` 型 (= POD) は drop なし、内部不変条件に依存しない bit-pattern 表現なので、
//!   torn 値を読み取っても **その値を使わずに破棄する** (= seqlock の re-validate で
//!   不一致を検出して捨てる) 限り無害。
//!
//! 一般化 (`V: Clone` 等) は roadmap で:
//!
//! - **`c8a` (案)**: `Arc<V>` 内部 wrap。reader は Arc clone (= 参照 count++) のみで
//!   生 V には触らない。memory 利得 (j8 の inline 20 B/cap) が崩れる。
//! - **`c8e` (案)**: `crossbeam_epoch` で writer の drop を遅延、reader は raw 参照保持。
//!   実装複雑度が一段上がる。
//!
//! ## `Cache` trait を実装しない
//!
//! `Cache::get` のシグネチャは `&mut self -> Option<&V>` で、
//! (1) `&mut self` が並行性を阻害、(2) `&V` の生存期間中 entry が動かない保証が
//! 必要 (writer がこの id を別 entry に再割当てすると use-after-free)。
//! c8 では (1) を `&self` にし、(2) を `Option<V>` (= `V: Copy` で copy out) に
//! することで両方を回避する。よって既存 `Cache` trait は実装しない。
//!
//! # 形式的な健全性ギャップ
//!
//! reader の `entries[id]` raw read は writer の同 slot 書き換えと race するので、
//! Rust の抽象機械上は **data race UB** に該当する。本実装は次の組み合わせで
//! 「実機 x86_64 で観測可能な範囲では well-defined」というレベルの正しさを得ている:
//!
//! 1. `K: Copy + V: Copy` で torn read が後続 Drop / Clone に伝播しない。
//! 2. 16-bit `AtomicU16` の Acquire load / Release store で seqlock fence を作り、
//!    候補 1 件ごとに前後の tag が一致することを確認する。
//! 3. writer は **tag 無効化 (Release store EMPTY)** を `entries` 書き換えに
//!    先行させ、**`entries` 書き換え** を **新 tag 公開 (Release store LIVE | …)**
//!    に先行させる。
//!
//! miri は (1) を考慮しないので並行 test は `#[cfg(not(miri))]` で抑制する。
//! 単一スレッド経路 (1 shard 同期テスト群) は miri で検証する。
//!
//! # writer / reader プロトコル
//!
//! ## reader (`get`)
//!
//! ```text
//! tail = self.tail.load(Acquire)
//! for i in 0..tail {
//!     t1 = tags[i].load(Acquire)
//!     if (t1 & SCAN_MASK) != needle: continue
//!     id = id_of(t1)
//!     (key_read, val_read) = read_volatile(entries[id])  // racy だが Copy なので非伝播
//!     t2 = tags[i].load(Acquire)
//!     if t2 != t1 or !(t2 & LIVE): continue              // seqlock 検証
//!     if key_read == *key:
//!         tags[i].fetch_or(VISITED, Relaxed)
//!         return Some(val_read)
//! }
//! return None
//! ```
//!
//! ## writer evict at pos (Mutex 配下)
//!
//! ```text
//! t = tags[pos].load(Relaxed)               // 排他下なので Relaxed
//! id = id_of(t)
//! tags[pos].store(EMPTY, Release)           // ★ tag 無効化を先に publish
//! entry = entries[id].assume_init_read()    // 旧 entry 取り出し (drop 用)
//! drop(entry)
//! ```
//!
//! ## writer install at pos (Mutex 配下)
//!
//! ```text
//! entries[id].write(Entry { key, value })   // ★ entries 書き込みを先に
//! new_tag = LIVE | (id << ID_SHIFT) | (hash & HASH_MASK)
//! tags[pos].store(new_tag, Release)         // ★ 新 tag 公開を最後に
//! ```
//!
//! ## writer existing-key update (Mutex 配下)
//!
//! ```text
//! old = tags[pos].load(Relaxed)
//! id = id_of(old)
//! tags[pos].store(EMPTY, Release)              // 一度無効化
//! entries[id].write(Entry { key, value })      // 新値書き込み
//! tags[pos].store(old | VISITED, Release)      // 元 tag に VISITED を立てて公開
//! ```
//!
//! 「一度 EMPTY を経由」する理由は、reader が `t1 = old → read entries → t2 = old`
//! の経路で torn value を採用するのを防ぐため。EMPTY 経由なら reader は必ず
//! どちらかで `t1 != t2` を検出して破棄する。
//!
//! # j8 からの主な差分
//!
//! - `Vec<u16>` → `Box<[AtomicU16]>` (resize しないので Box で十分)
//! - `Vec<MaybeUninit<Entry>>` → `UnsafeCell<Box<[MaybeUninit<Entry>]>>`
//!   (writer 排他下のみアクセス、reader は raw pointer)
//! - `tail: usize` → `AtomicUsize` (reader が見る)
//! - `len: usize` → `AtomicUsize` (`len()` を `&self` で呼ぶ)
//! - `hand: usize` は writer のみ参照 → `Mutex<WriterState>` 配下
//! - `find_scalar` / `find_avx2`: 候補 1 件ごとに seqlock dance + Copy 読み

use crate::Xxh3Build;
use parking_lot::Mutex;
use std::cell::UnsafeCell;
use std::hash::{BuildHasher, Hash};
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering, fence};

const EMPTY: u16 = 0;
const LIVE: u16 = 0x8000;
const VISITED: u16 = 0x4000;
/// AVX2 1 chunk = 32 byte = 16 u16 lane。
const LANE: usize = 16;
/// 6-bit entry_id の構造的上限。per_shard はこの値以下でなければならない。
pub const MAX_PER_SHARD: usize = 64;

/// `sizeof(Entry)` から ID_SHIFT (= log2(sizeof)) を const-eval で算出。j8 と同形。
const fn id_shift_from_entry_size(s: usize) -> u32 {
    assert!(
        s.is_power_of_two(),
        "sieve_c8: sizeof(Entry<K,V>) must be a power of two"
    );
    assert!(s <= 256, "sieve_c8: sizeof(Entry<K,V>) must be <= 256");
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

type EntriesArena<K, V> = UnsafeCell<Box<[MaybeUninit<Entry<K, V>>]>>;

/// 1 shard 分の並行 SIEVE。
pub(crate) struct Shard<K, V> {
    capacity: usize,
    /// tag 列。reader が atomic load、writer が Mutex 配下で atomic store。
    /// `Box<[AtomicU16]>` は resize しない (固定長)。
    tags: Box<[AtomicU16]>,
    /// entries arena。`writer.lock()` を取った間のみ書き込んでよい。
    /// reader は **raw pointer 経由で copy 読み + tag re-validate** で seqlock を成立させる。
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

// SAFETY: Shard 内部の UnsafeCell<entries> は writer Mutex 配下でのみ書き込まれ、
// reader は seqlock-via-tag プロトコルでアクセスする。共有参照を別スレッドに
// 渡せる (Sync) かつ所有権を別スレッドに渡せる (Send) のは K, V がともに
// Send + Sync のときのみ。
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
}

impl<K, V> Shard<K, V>
where
    K: Hash + Eq + Copy,
    V: Copy,
{
    fn new(capacity: usize) -> Self {
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
        let mut entries_vec: Vec<MaybeUninit<Entry<K, V>>> = Vec::with_capacity(capacity);
        entries_vec.resize_with(capacity, MaybeUninit::uninit);

        Self {
            capacity,
            tags: tags_vec.into_boxed_slice(),
            entries: UnsafeCell::new(entries_vec.into_boxed_slice()),
            tail: AtomicUsize::new(0),
            len: AtomicUsize::new(0),
            writer: Mutex::new(WriterState { hand: 0 }),
        }
    }

    /// j8 と同形の hash → tag bit spread。const fn ではないが Self::ID_SHIFT に依存する。
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

    /// reader 用: 候補 1 件ごとに seqlock dance を回して値を返す。
    /// hit したら VISITED bit を `fetch_or(Relaxed)` で立てる。
    fn find_get(&self, key: &K, needle: u16) -> Option<V> {
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        {
            if std::is_x86_feature_detected!("avx2") {
                // SAFETY: avx2 は runtime detect 済み、bmi1 は AVX2 capable CPU の前提。
                return unsafe { self.find_get_avx2(key, needle) };
            }
        }
        self.find_get_scalar(key, needle)
    }

    /// reader 用 scalar path。AVX2 が無い環境および miri 用フォールバック。
    fn find_get_scalar(&self, key: &K, needle: u16) -> Option<V> {
        let tail = self.tail.load(Ordering::Acquire);
        let entries_base = self.entries_ptr();
        for i in 0..tail {
            let t1 = self.tags[i].load(Ordering::Acquire);
            if (t1 & Self::SCAN_MASK) != needle {
                continue;
            }
            let id = Self::id_of(t1);
            // SAFETY: id < MAX_PER_SHARD <= capacity = entries.len()。
            // writer は同 slot を mutate し得るが、後続の re-validate で torn read を弾く。
            // K, V: Copy のため torn ビット列の使い回し / drop は発生しない。
            let entry =
                unsafe { std::ptr::read_volatile(entries_base.add(id) as *const Entry<K, V>) };
            let t2 = self.tags[i].load(Ordering::Acquire);
            if t2 != t1 || (t2 & LIVE) == 0 {
                continue;
            }
            if entry.key == *key {
                self.tags[i].fetch_or(VISITED, Ordering::Relaxed);
                return Some(entry.value);
            }
        }
        None
    }

    #[cfg(all(target_arch = "x86_64", not(miri)))]
    #[target_feature(enable = "avx2,bmi1")]
    /// reader 用 AVX2 path。j8 の `find_avx2` を seqlock dance 付きに置換したもの。
    ///
    /// SIMD scan は **best-effort filter** として機能する: SIMD load は形式的には
    /// `tags.as_ptr() as *const u16` 経由の non-atomic 読みなので Rust 抽象機械では
    /// 候補との race が UB。実機 x86_64 では aligned u16 load は HW アトミック、
    /// SIMD load は 16 個の HW atomic load が同一 cache line から取られたのと
    /// 等価な ASM になる。SIMD で取りこぼしがあっても (= 並行 insert 中の tag を
    /// false negative で missed)、cache 一時 miss にしかならず concurrent cache の
    /// relaxed-semantics として許容範囲。候補が見つかった場合は **scalar 用 seqlock
    /// dance を流用** (= candidate ごと Acquire 再 load + Copy 読み + tag 再検証)
    /// して memory ordering を担保する。
    unsafe fn find_get_avx2(&self, key: &K, needle: u16) -> Option<V> {
        use std::arch::x86_64::*;

        let tail = self.tail.load(Ordering::Acquire);
        // tags は AtomicU16 だが repr(transparent) over UnsafeCell<u16> なのでメモリ
        // レイアウトは Vec<u16> と同一。SIMD load は *const u16 経由で行う。
        let tags_ptr = self.tags.as_ptr() as *const u16;
        let needle_v = _mm256_set1_epi16(needle as i16);
        let mask_v = _mm256_set1_epi16(Self::SCAN_MASK as i16);

        // tail を超えた領域も order_cap までは EMPTY なので false-match しない。
        // limit は 16-lane chunk 単位で切り上げ済みの tags.len()。
        let limit = self.tags.len();

        let mut i = 0usize;
        while i < limit {
            let v = unsafe { _mm256_loadu_si256(tags_ptr.add(i) as *const __m256i) };
            let masked = _mm256_and_si256(v, mask_v);
            let cmp = _mm256_cmpeq_epi16(masked, needle_v);
            let mut mask = _mm256_movemask_epi8(cmp) as u32;

            while mask != 0 {
                // bit は 0..32 の偶数 (vpmovmskb 出力の epi16 一致は連続 2 byte の MSB ペア)
                let bit = mask.trailing_zeros() as usize;
                let lane = bit >> 1;
                let pos = i + lane;
                if pos < tail
                    && let Some(val) = self.try_candidate(pos, key, needle)
                {
                    return Some(val);
                }
                // BLSR ×2 で 1 候補ペアを落とす (tzcnt 結果に独立)。
                mask = _blsr_u32(mask);
                mask = _blsr_u32(mask);
            }
            i += LANE;
        }
        None
    }

    /// 1 候補に対する seqlock dance。スカラー / AVX2 path 共通。
    #[inline]
    fn try_candidate(&self, pos: usize, key: &K, needle: u16) -> Option<V> {
        let t1 = self.tags[pos].load(Ordering::Acquire);
        if (t1 & Self::SCAN_MASK) != needle {
            return None;
        }
        let id = Self::id_of(t1);
        let entries_base = self.entries_ptr();
        // SAFETY: 上記 find_get_scalar と同じ。Copy 制約でtorn read 非伝播。
        let entry = unsafe { std::ptr::read_volatile(entries_base.add(id) as *const Entry<K, V>) };
        let t2 = self.tags[pos].load(Ordering::Acquire);
        if t2 != t1 || (t2 & LIVE) == 0 {
            return None;
        }
        if entry.key == *key {
            self.tags[pos].fetch_or(VISITED, Ordering::Relaxed);
            return Some(entry.value);
        }
        None
    }

    /// `entries` Box の先頭 raw pointer。reader / writer 共用ヘルパ。
    /// SAFETY 注: 返り値の deref は呼び出し側の責任。
    #[inline]
    fn entries_ptr(&self) -> *const MaybeUninit<Entry<K, V>> {
        // SAFETY: UnsafeCell::get で *mut を取って dereference して slice を取得、
        // その先頭 pointer を返す。slice は alive 期間中 valid。
        unsafe { (*self.entries.get()).as_ptr() }
    }

    /// reader 用 contains。値を copy しないだけで find_get と同じ seqlock dance。
    fn contains(&self, key: &K, hash: u64) -> bool {
        self.find_get(key, Self::needle_from_hash(hash)).is_some()
    }

    /// writer (insert) 用。Mutex を取って既存 j8 の eviction ロジックを実行する。
    fn insert(&self, key: K, value: V, hash: u64) -> Option<(K, V)> {
        let needle = Self::needle_from_hash(hash);
        let mut state = self.writer.lock();

        // 1. 既存キー探索 (Mutex 配下なので race を心配しなくてよいが、
        //    reader と並行 ⇒ store は Release で発行する)。
        if let Some(pos) = self.writer_find(&key, needle) {
            self.writer_update_in_place(pos, key, value);
            return None;
        }

        // 2. evict / warm-up で entry_id を確保。
        let len = self.len.load(Ordering::Relaxed);
        let (evicted, entry_id): (Option<(K, V)>, u16) = if len < self.capacity {
            (None, len as u16)
        } else {
            let (kv, freed_id) = self.writer_evict_one(&mut state);
            (Some(kv), freed_id)
        };

        // 3. tail が order_cap に到達していたら compact 発火。
        let tail = self.tail.load(Ordering::Relaxed);
        if tail == self.tags.len() {
            self.writer_compact(&mut state);
        }

        // 4. 新規 install。
        let pos = self.tail.load(Ordering::Relaxed);
        let new_tag = LIVE | (entry_id << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        // entries への書き込みを先に完了させる。
        let entries_mut = self.entries.get();
        // SAFETY: writer Mutex 配下なので排他、entry_id は未使用 slot
        // (warm-up なら未触、steady なら直前 evict で uninit に戻った slot)。
        unsafe {
            (*entries_mut)[entry_id as usize].write(Entry { key, value });
        }
        // entries 書き込みを new_tag publish より strict に先行させる。
        // tag store の Release だけでも Acquire load した reader からは
        // ordered に見えるが、念のため明示的に compiler/CPU fence を入れる。
        fence(Ordering::Release);
        self.tags[pos].store(new_tag, Ordering::Release);
        self.tail.store(pos + 1, Ordering::Release);
        // evict 経由なら len は -1 されているので +1 で capacity 復帰、
        // warm-up 経由なら +1 で len+1。どちらの経路でも fetch_add(1) が正解。
        self.len.fetch_add(1, Ordering::Release);

        evicted
    }

    /// writer 内部 find: Mutex 配下で tags を Relaxed で読みつつ key を比較する。
    /// reader 側 seqlock は不要 (writer が排他、entries は writer 自身しか mutate しない)。
    fn writer_find(&self, key: &K, needle: u16) -> Option<usize> {
        let tail = self.tail.load(Ordering::Relaxed);
        let entries_base = self.entries_ptr();
        for i in 0..tail {
            let t = self.tags[i].load(Ordering::Relaxed);
            if (t & Self::SCAN_MASK) != needle {
                continue;
            }
            let id = Self::id_of(t);
            // SAFETY: t が SCAN_MASK 一致 ⇒ LIVE bit 立 ⇒ entries[id] init 済み。
            // writer 排他下なので raw read で安全。
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

    /// 既存キー更新: 一度 EMPTY に落としてから値を書き換え、最後に新 tag を Release 公開。
    /// reader が torn value を採用しないよう「t1 != t2」を強制するための手順。
    fn writer_update_in_place(&self, pos: usize, key: K, value: V) {
        let old = self.tags[pos].load(Ordering::Relaxed);
        let id = Self::id_of(old);
        // 1. tag 無効化
        self.tags[pos].store(EMPTY, Ordering::Release);
        // 2. entries 上書き (古い値は drop される — Copy なので drop は trivial)
        let entries_mut = self.entries.get();
        unsafe {
            // assume_init_drop は Copy では no-op、念のため明示的に書き換える。
            (*entries_mut)[id].write(Entry { key, value });
        }
        fence(Ordering::Release);
        // 3. 新 tag (元の hash + VISITED) を公開
        let new_tag = (old & !VISITED) | VISITED; // = old | VISITED、ただし old の hash 等を保つ
        // ※ old & !VISITED は old から visited を一旦落とした値、それに VISITED を再付与
        //   している。実質 old | VISITED と同じ bit 列だが、意図 (= visited を立てた tag を
        //   復帰公開) を明示するために 2 段で書いている。最適化で消える。
        self.tags[pos].store(new_tag, Ordering::Release);
    }

    /// j8 と同形の victim 探索 + freed entry_id を返す。Mutex 配下で呼ぶ。
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

    /// SIEVE の hand 巡回 (visited を剥がしながら未 visited を探す)。
    ///
    /// ※ reader の `fetch_or(VISITED)` が evict 直後の tag (LIVE 落とし済み) に
    /// 発火すると `0x4000` (VISITED のみ・LIVE 無し) という「phantom non-empty」
    /// が残りうる。writer の判定は **LIVE bit の有無** で行い、phantom は
    /// 通りすがりに EMPTY に正規化する。
    fn writer_scan_evict(&self, lo: usize, hi: usize) -> Option<usize> {
        for i in lo..hi {
            let t = self.tags[i].load(Ordering::Relaxed);
            if t & LIVE == 0 {
                if t != EMPTY {
                    // phantom (VISITED のみ等) を EMPTY に正規化
                    self.tags[i].store(EMPTY, Ordering::Release);
                }
                continue;
            }
            if t & VISITED != 0 {
                // visited を剥がす。reader からは tag が変わるので seqlock 検出 → 採用しない。
                self.tags[i].store(t & !VISITED, Ordering::Release);
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
            if t != EMPTY {
                self.tags[i].store(EMPTY, Ordering::Release);
            }
        }
        None
    }

    fn writer_do_evict(&self, state: &mut WriterState, pos: usize, tail: usize) -> ((K, V), u16) {
        let t = self.tags[pos].load(Ordering::Relaxed);
        debug_assert!(t != EMPTY);
        let id = Self::id_of(t) as u16;
        // 1. tag 無効化を先に publish
        self.tags[pos].store(EMPTY, Ordering::Release);
        fence(Ordering::Release);
        // 2. 旧 entry を取り出す (assume_init_read は Copy なので bitwise copy)
        let entries_mut = self.entries.get();
        let entry = unsafe { (*entries_mut)[id as usize].assume_init_read() };
        // 3. len 更新と hand 進め
        let prev_len = self.len.load(Ordering::Relaxed);
        self.len.store(prev_len - 1, Ordering::Release);
        let mut next_hand = pos + 1;
        if next_hand >= tail {
            next_hand = 0;
        }
        state.hand = next_hand;
        ((entry.key, entry.value), id)
    }

    /// j8 の compact と同形 (tags の前詰め、entries arena は不変)。
    /// Mutex 配下で実行、tag store はすべて Release。
    fn writer_compact(&self, state: &mut WriterState) {
        let old_tail = self.tail.load(Ordering::Relaxed);
        let old_hand = state.hand.min(old_tail);
        let mut new_hand: Option<usize> = None;
        let mut write = 0usize;

        for old_pos in 0..old_tail {
            let t = self.tags[old_pos].load(Ordering::Relaxed);
            // LIVE bit 無しは「phantom (VISITED のみ等)」も含めて EMPTY 扱い。
            if t & LIVE == 0 {
                if t != EMPTY {
                    self.tags[old_pos].store(EMPTY, Ordering::Release);
                }
                continue;
            }
            if new_hand.is_none() && old_pos >= old_hand {
                new_hand = Some(write);
            }
            if write != old_pos {
                // tag 無効化を新位置に書き、その後旧位置を EMPTY 化、最後に新位置を本値に。
                // reader からは「t1 = old, read entries[id], t2 = EMPTY/別値」で必ず弾かれる。
                self.tags[write].store(EMPTY, Ordering::Release);
                self.tags[old_pos].store(EMPTY, Ordering::Release);
                self.tags[write].store(t, Ordering::Release);
            }
            write += 1;
        }
        for i in write..old_tail {
            self.tags[i].store(EMPTY, Ordering::Release);
        }

        let len = self.len.load(Ordering::Relaxed);
        self.tail.store(write, Ordering::Release);
        state.hand = if len == 0 { 0 } else { new_hand.unwrap_or(0) };
        debug_assert_eq!(len, write);
    }

    /// テスト用: shard 内の LIVE tag 数を直接数える。
    /// phantom (VISITED only, LIVE 無し) は除外する。
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

    /// テスト用: shard 内の全 LIVE tag が指す id 集合を返す (重複なしを別途検証)。
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
        // K, V: Copy なので drop は trivial だが、entries の MaybeUninit
        // 規約上は init 領域を明示的に drop する設計が正しい。
        // ここでは &mut self なので排他、atomic 順序の心配は不要。
        let tail = self.tail.load(Ordering::Relaxed);
        let entries_mut = self.entries.get();
        for i in 0..tail {
            let t = self.tags[i].load(Ordering::Relaxed);
            if t & LIVE != 0 {
                let id = Self::id_of(t);
                // SAFETY: LIVE ⇒ entries[id] init 済み (writer 不変条件)
                unsafe {
                    (*entries_mut)[id].assume_init_drop();
                }
            }
        }
    }
}

// ---------------- 外側 (set-associative wrapper) ----------------

pub const DEFAULT_SHARDS: usize = 8;

/// `sieve_j8` の並行版 set-associative SIEVE。
///
/// `&self` で `get` / `insert` / `contains_key` / `len` を呼べる。
/// `K, V: Copy` 制約は冒頭 module doc 参照。
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

    /// `&self` 経由の lock-free read。値を copy out して返す。
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

    /// テスト用: 指定 shard へのアクセサ。
    #[cfg(test)]
    pub(crate) fn shard(&self, idx: usize) -> &Shard<K, V> {
        &self.shards[idx]
    }
}

#[cfg(test)]
mod tests {
    //! j8 のテスト群を mirror + 並行 invariants test。

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

    /// MAX_PER_SHARD まで詰めて全 hit を確認。
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
    /// シングルスレッドなので race なし → bit-exact 期待可能。
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
                "1-shard で sieve_orig と c8 が key {k} で食い違う"
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
                "1-shard で j8 と c8 が key {k} で食い違う"
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

    /// 並行 invariants: N thread から Zipf 流して終了後の不変条件のみ検証。
    /// eviction 列の bit-exact 一致は意図的に放棄する。
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

        // I-conc-1: 全体 len <= cap
        let total_len = cache.len();
        assert!(total_len <= cap, "len {total_len} > cap {cap}");

        // I-conc-2 / I-conc-3: 各 shard で LIVE tag 数 == shard.len、id 集合は重複なし
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
