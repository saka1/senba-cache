//! `sieve_c16s`: c14s の writer hot 4 line を 1 cache line に co-locate。
//!
//! c14s からの **唯一の構造差分は per-shard layout**: `Mutex<WriterState>` /
//! `AtomicU64 visited` / `AtomicUsize len` を `#[repr(C, align(64))] ShardHot`
//! に集約し、writer が 1 op で取る hot field を 1 cache line に詰める。
//! per-shard cap ≤ 64 (6-bit ID 上限) なので visited は `AtomicU64` 1 word で
//! 全 pos を表現でき、`vbit(pos) → (word_idx, mask)` は `vbit_mask(pos) → mask`
//! に縮退する。Path A / Path B/C のロジックは c14s と同型。
//!
//! # ⚠ 健全性は `V: Copy` 限定 (c14s 同様)
//!
//! reader の seqlock-via-tag は c14s と同じ構造 (tag を v1/v2、間に
//! `ptr::read<ManuallyDrop<Entry>>` を挟む)。`ptr::read` の **前** に writer
//! 進行を検知して escape する仕組みがないため、`V: !Copy` で writer と並走
//! すると半上書きされた entry header (e.g. `String` の (ptr, len, cap)) が
//! reader の ManuallyDrop drop 経路で `free(壊れた ptr)` に到達して abort
//! する。`V: Copy` (Drop なし) でのみ健全。
//!
//! 詳細・再現条件・root cause は `sieve_c14s` の module doc と
//! `docs/reports/2026-05-11-cseries-string-baseline.md` §5 を参照。
//! library 化候補は entry-level seqlock の `sieve_c17s` が引き継ぐ。
//!
//! 設計の一次資料: `docs/reports/2026-05-10-c16s-design.md`。
//! 動機 (3-line picture / Mops 改善 ROI 上位案) は
//! `docs/reports/2026-05-10-c14s-vtune-write-contention.md` §8.1。
//!
//! ----- 以下 c14s 由来の module doc -----
//!
//! `sieve_c14s`: c14s の 3 点 tuning。SIEVE 等価性 / API 表面 / shard 構造は不変。
//!
//! 1. **find_lockfree AVX2 化** — Path A の scan を c11s reader と同形の SIMD
//!    (16-lane × 4 chunk) にして uniform write の overhead を縮める
//! 2. **MAX_RETRY = 1** — Path A の CAS 失敗時に即 Mutex escalate (retry loop 廃止)
//! 3. **reader bounded retry (MAX=4)** — `get` 公開 API で false-miss を吸収
//!
//! 設計の一次資料: `docs/reports/2026-05-08-c14s-design.md`。
//!
//! # 動機 (c13s からの位置取り)
//!
//! c13s sweep は read-heavy zipf 16T で ≥ c11s + adversarial-hot HR drop を発見:
//!   - uniform read-heavy: c11s 比 -67% (try_path_a の scalar 64 scan が pure overhead)
//!   - adversarial-hot HR: -0.26 (reader seqlock の VERSION flip false-miss)
//!
//! どちらも構造的問題ではなく実装 tunable。c14s でこの 2 点を解消する。
//!
//! 観察: hot key への write は **Path A (update existing key)** であって Path C
//! (evict + install) ではない。Path A は eviction を起こさず、tag の HASH/ID/LIVE
//! 部位は最終的に元値に戻る (V を差し替えて visited を SET するだけ)。よって Path A
//! は SIEVE state machine に何の変化も起こさず、並行に走らせても eviction 順序は
//! 保持される。
//!
//! c14s は c13s と同様 **Path A だけ lock-free**、**Path B/C は writer Mutex 配下で
//! senba::Cache 流 shift-on-evict**。3 点の tuning で c13s の残課題を潰す。
//!
//! # senba::Cache lineage (= c8/c11s/c12s の j8 lineage と異なる)
//!
//! 継承する load-bearing piece:
//! - **shift-on-evict** (eager): Path C で `tags[pos+1..len]` を 1 slot 詰める。
//!   新 entry は必ず tail (= len-1) に install されるので SIEVE 順序が崩れない。
//! - **single `len` field**: c11s/c12s の `tail` + `len` ペアでなく `len` のみ。
//!   steady state では `tags[0..capacity]` 全 LIVE で reader scan 範囲も一致。
//! - **c-hoist trick** (`tag & ID_MASK == id × sizeof(Entry)`): SIMD lane から
//!   entries pointer を直接計算する trick は senba と同形 (c11s/c12s と共通)。
//! - **AVX2 find** (BLSR×2 + chunk hoist): c11s から流用。
//!
//! # Concurrent layer (= c11s から継承)
//!
//! - tags を `Box<[AtomicU16]>`、reader は Acquire load、writer は CAS / Release store
//! - visited を `Box<[AtomicU64]>` に bit-pack (c10s 由来、tag 列の cache-line 汚染回避)
//! - reader hit で **conditional `fetch_or`** (c11s 由来、hot key の MESI ping-pong 回避)
//! - seqlock-via-tag (`t1 == t2 && (t2 & LIVE) != 0`)
//!
//! # Lock-free Path A (= c12s 由来、ただし install-at-evicted-pos は捨てる)
//!
//! 1. `find_lockfree` で pos と現 tag `t` を取得
//! 2. `tags[pos].compare_exchange(t, EMPTY, Acquire, Acquire)` で slot 所有権獲得
//! 3. CAS 成功 = `entries[id]` への排他書き込み権獲得
//!    - `ptr::read` で旧 Entry を取り出し
//!    - `ptr::write` で `Entry { key: old.key, value: new_value }` を install
//!    - 旧 V は drop (旧 K は新 Entry に move されたので drop されない)
//! 4. `tags[pos].store(t ^ VERSION, Release)` で tag を flipped VERSION で復帰
//! 5. visited[pos] を SET (sieve_orig の `freq=1` 一致)
//!
//! CAS 失敗 = 並行 writer (別の Path A or Path B/C) が tag を変更 → c14s は **即** writer
//! Mutex に escalate (MAX_RETRY = 1; c13s の MAX_RETRY = 4 retry loop は廃止)。
//!
//! # VERSION bit (0x4000) — soundness key
//!
//! Path A は CAS sentinel (= EMPTY) を経由して同 hash/id の tag を復帰させるため、
//! reader の `t1 == t2` 検査が **同一 16 bit 値** を観測すると Path A cycle を見逃す。
//! VERSION bit を Path A ごとに flip することで、復帰後の tag は必ず元と異なる
//! 16 bit 値となり、reader が cycle を確実に検出できる。
//!
//! tag layout (16 bit):
//! ```text
//!   bit 15: LIVE          (= 0x8000)
//!   bit 14: VERSION       (= 0x4000)  ← Path A ごとに flip
//!   bits ID_SHIFT..+6: ID (6 bit)
//!   remaining: HASH       (8 bit、c8 と同じ)
//! ```
//!
//! `SCAN_MASK = LIVE | HASH_MASK` は VERSION + ID を **除外** するので、find は
//! VERSION 値に関わらず同 key を発見する。一方 seqlock の `t1 == t2` は **全 16 bit
//! 一致** を要求するので Path A cycle を検出する。
//!
//! # K, V trait bounds
//!
//! - `K: Hash + Eq` (Clone 不要): Path A は引数 key を drop して `entries[id]` の旧 K
//!   を流用、find は &K で eq 比較
//! - `V: Clone`: reader が seqlock validate 後の local snapshot から `clone()` で値を取り出す。
//!   非 Copy な V (String 等) も sound に扱える (ただし下記 caveat あり)。
//!
//! # V: Clone soundness の限界 (research artifact 限定)
//!
//! reader が seqlock を pass し V::clone を呼んでいる **mid-flight** に並行 Path A が
//! 走り old V を drop すると、V::clone が freed heap を読む可能性がある (UB)。
//! seqlock dance は cycle 完了の検出に使うので、cycle 開始前に reader が pass してから
//! clone 完了までの間の race は捕えられない。
//!
//! - V = u64 / 整数型のような Copy type では問題なし (drop が no-op、heap 無し)
//! - V = String / Vec のような heap-owning type は research 用途なら fine、production
//!   用途では senba::concurrent::Cache (将来) で Arc<V> / Epoch GC を組み合わせる必要
//!
//! c14s は research artifact (= bench で V = u64 を使う) としては sound、API 表面は
//! V: Clone を許容する形にしてある。
//!
//! # miri
//!
//! 単一スレッド経路 (warmup, evict_one_st, basic update) は miri pass。並行
//! テストは `#[cfg(not(miri))]` で抑制 (c8/c11s/c12s と同じ)。

use parking_lot::Mutex;
use senba::Xxh3Build;
use std::cell::UnsafeCell;
use std::hash::{BuildHasher, Hash};
use std::hint;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering, fence};

/// EMPTY tag (LIVE OFF、VERSION OFF、すべての bit が 0)。
/// Path A の CAS sentinel と、tags[capacity..order_cap] の永久 pad、初期状態に使う。
const EMPTY: u16 = 0;
/// LIVE bit (bit 15)。tag が有効な entry を指していることを示す。
const LIVE: u16 = 0x8000;
/// VERSION bit (bit 14)。Path A の cycle を reader に伝えるため、Path A ごとに flip する。
/// SCAN_MASK には含めない (= find は VERSION 値に関わらず一致)。
const VERSION: u16 = 0x4000;
/// AVX2 1 chunk = 32 byte = 16 u16 lane。
const LANE: usize = 16;
/// 6-bit entry_id の構造的上限。per_shard はこの値以下でなければならない。
pub const MAX_PER_SHARD: usize = 64;

/// `sizeof(Entry)` から ID_SHIFT (= log2(sizeof)) を const-eval で算出。
const fn id_shift_from_entry_size(s: usize) -> u32 {
    assert!(
        s.is_power_of_two(),
        "sieve_c14s: sizeof(Entry<K,V>) must be a power of two"
    );
    assert!(s <= 256, "sieve_c14s: sizeof(Entry<K,V>) must be <= 256");
    s.trailing_zeros()
}

const fn id_mask_from_shift(id_shift: u32) -> u16 {
    ((MAX_PER_SHARD - 1) as u16) << id_shift
}

/// HASH 領域は 0x3FFF (LIVE と VERSION を除外) から ID 6 bit を抜いたもの。
/// c8 と同じ 8 bit、c11s の 9 bit より 1 bit 少ない (VERSION bit を切り出した分)。
const fn hash_mask_from_id_mask(id_mask: u16) -> u16 {
    0x3FFF & !id_mask
}

struct Entry<K, V> {
    key: K,
    value: V,
}

/// reader scan 1 slot の結果。
///
/// `Miss` は "tag が needle と一致しない / 一致したが key が異なる" という settled な
/// 不一致。 `Racing` は "tag が一致したが seqlock validate に落ちた" — Path A の cycle
/// を踏んだ可能性があり、caller (get_by_hash) はこの観測時のみ retry する。
enum Probe<V> {
    Found(V),
    Miss,
    Racing,
}

type EntriesArena<K, V> = UnsafeCell<Box<[MaybeUninit<Entry<K, V>>]>>;

/// writer が 1 op で取る hot field を 1 cache line に co-locate (c16s 設計)。
///
/// per-shard cap ≤ 64 なので visited は `AtomicU64` 1 word で全 pos を表現可能。
/// `parking_lot::Mutex<T>` の repr は `#[repr(C)] lock_api::Mutex { raw, data }`
/// で raw word が offset 0 — `ShardHot` を `#[repr(C, align(64))]` で揃えるので
/// Mutex word も `ShardHot` の cache line 内 offset 0 に来る。
///
/// 別 core が shard ownership を奪う際、writer 4 line (Mutex word, hand,
/// visited, len) の coherence transfer が **1 transfer** で済む。
#[repr(C, align(64))]
struct ShardHot {
    /// Path B/C 排他。Path A (lock-free CAS update) は Mutex を取らない。
    writer: Mutex<WriterState>,
    /// 1 shard 全 visited (cap ≤ 64)。reader fetch_or / writer fetch_and 両方が触る。
    /// c14s では別 `Box<[AtomicU64]>` だったのを inline 化、word index 計算が消える。
    visited: AtomicU64,
    /// live entry 数。reader scan は `tags[0..len]` を見る。
    /// Path B (warmup) で `+1`、Path C (evict + shift + install) では不変 (= capacity)。
    len: AtomicUsize,
}

const _: () = {
    assert!(std::mem::size_of::<ShardHot>() == 64);
    assert!(std::mem::align_of::<ShardHot>() == 64);
};

/// 1 shard 分の並行 SIEVE。
pub struct Shard<K, V> {
    capacity: usize,
    /// tag 列。AtomicU16 で原子操作。
    /// 長さは `((cap + LANE - 1) & !(LANE-1)).max(LANE)`、`tags[capacity..order_cap]`
    /// は永久 EMPTY pad (senba::Cache lineage と同じく、scan 終端を SIMD lane に揃える)。
    tags: Box<[AtomicU16]>,
    /// entries arena。Path A は tag CAS で所有権を確保した上で、Path B/C は writer
    /// Mutex 配下で書き込む。
    entries: EntriesArena<K, V>,
    /// 1 cache line に集約された writer hot state (Mutex / visited / len)。
    /// c14s 比でこの 3 field が独立 line に散っていたのを co-locate (c16s 設計)。
    hot: ShardHot,
}

struct WriterState {
    hand: usize,
}

// SAFETY: c11s と同じ。entries[id] への書き込みは tag CAS で所有権を確保した writer
// または Mutex 配下の writer のみが行い、reader は seqlock-via-tag + ManuallyDrop で
// torn read / use-after-free を弾く (V: Clone soundness 限界は module doc 参照)。
unsafe impl<K: Send, V: Send> Send for Shard<K, V> {}
unsafe impl<K: Send + Sync, V: Send + Sync> Sync for Shard<K, V> {}

impl<K, V> Shard<K, V> {
    const ENTRY_SIZE: usize = std::mem::size_of::<Entry<K, V>>();
    const ID_SHIFT: u32 = id_shift_from_entry_size(Self::ENTRY_SIZE);
    const ID_MASK: u16 = id_mask_from_shift(Self::ID_SHIFT);
    const HASH_MASK: u16 = hash_mask_from_id_mask(Self::ID_MASK);
    /// reader needle 比較用。VERSION + ID を除外、LIVE + HASH のみを比較。
    const SCAN_MASK: u16 = LIVE | Self::HASH_MASK;

    #[inline]
    fn id_of(tag: u16) -> usize {
        ((tag & Self::ID_MASK) >> Self::ID_SHIFT) as usize
    }

    /// pos に対応する visited bit mask。c14s では `(word_idx, mask)` を返していたが、
    /// c16s は `ShardHot::visited` が単一 `AtomicU64` なので word_idx は常に 0、
    /// mask のみを返す。pos < 64 (= per-shard cap ≤ 64) を debug でガード。
    #[inline]
    fn vbit_mask(pos: usize) -> u64 {
        debug_assert!(pos < 64, "vbit_mask: pos {pos} >= 64 (per-shard cap limit)");
        1u64 << pos
    }

    /// reader-safe な capacity 取得。
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// reader-safe な live entry 数の取得。
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
        let order_cap = ((capacity + LANE - 1) & !(LANE - 1)).max(LANE);

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
            hot: ShardHot {
                writer: Mutex::new(WriterState { hand: 0 }),
                visited: AtomicU64::new(0),
                len: AtomicUsize::new(0),
            },
        }
    }

    /// hash → tag bit spread。8 bit hash を ID_MASK の左右に詰める (c8 と同じ shape)。
    #[inline]
    fn needle_from_hash(hash: u64) -> u16 {
        // 高位 8 bit を取り出す。
        let h8 = ((hash >> 56) as u16) & 0x00FF;
        let s = Self::ID_SHIFT;
        let spread = if s >= 8 {
            h8
        } else {
            let low_mask: u16 = ((1u32 << s) - 1) as u16;
            let low = h8 & low_mask;
            let high = (h8 & !low_mask) << 6;
            low | high
        };
        LIVE | spread
    }

    /// reader 用: AVX2 ⇒ scalar の dispatch。
    ///
    /// Returns `(value, racing)`:
    /// - `value: Option<V>` — 見つかった V (Some) または scan 完了で発見できず (None)
    /// - `racing: bool` — Path A の cycle を観測した可能性。true の場合 caller は retry。
    ///   false なら "true miss" 確定で retry 不要。
    fn find_get(&self, key: &K, needle: u16) -> (Option<V>, bool) {
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        {
            if std::is_x86_feature_detected!("avx2") {
                // SAFETY: avx2 runtime detect 済み、bmi1 は AVX2 capable CPU の前提。
                return unsafe { self.find_get_avx2(key, needle) };
            }
        }
        self.find_get_scalar(key, needle)
    }

    fn find_get_scalar(&self, key: &K, needle: u16) -> (Option<V>, bool) {
        let len = self.hot.len.load(Ordering::Acquire);
        let mut racing = false;
        for i in 0..len {
            let t = self.tags[i].load(Ordering::Acquire);
            if (t & Self::SCAN_MASK) == needle {
                match self.try_candidate(i, key, needle) {
                    Probe::Found(v) => return (Some(v), false),
                    Probe::Racing => racing = true,
                    Probe::Miss => {}
                }
            } else if (t & LIVE) == 0 {
                // i < len で LIVE=0 ⇒ Path A が CAS-EMPTY 中。retry 候補。
                racing = true;
            }
        }
        (None, racing)
    }

    #[cfg(all(target_arch = "x86_64", not(miri)))]
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_get_avx2(&self, key: &K, needle: u16) -> (Option<V>, bool) {
        use std::arch::x86_64::*;

        let len = self.hot.len.load(Ordering::Acquire);
        let tags_ptr = self.tags.as_ptr() as *const u16;
        let needle_v = _mm256_set1_epi16(needle as i16);
        let mask_v = _mm256_set1_epi16(Self::SCAN_MASK as i16);
        let zero_v = _mm256_setzero_si256();

        let limit = self.tags.len();

        let mut i = 0usize;
        let mut racing = false;
        while i < limit {
            let v = unsafe { _mm256_loadu_si256(tags_ptr.add(i) as *const __m256i) };
            let masked = _mm256_and_si256(v, mask_v);
            let cmp = _mm256_cmpeq_epi16(masked, needle_v);
            let mut mask = _mm256_movemask_epi8(cmp) as u32;

            // Path A in-flight 検出: [i, min(i+LANE, len)) に LIVE=0 lane が居れば retry。
            // SCAN_MASK は LIVE を含むので masked == 0 ⇔ LIVE=0 (steady state では pad のみ)。
            if i < len {
                let empty_cmp = _mm256_cmpeq_epi16(masked, zero_v);
                let mut empty_mask = _mm256_movemask_epi8(empty_cmp) as u32;
                let live_lanes = (len - i).min(LANE);
                if live_lanes < LANE {
                    // pad 領域 (lane >= live_lanes) を mask の対象から外す。
                    // 1 lane = 2 bit (epi8 movemask on epi16)。
                    let keep_bits = (1u32 << (live_lanes * 2)) - 1;
                    empty_mask &= keep_bits;
                }
                if empty_mask != 0 {
                    racing = true;
                }
            }

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

    /// reader seqlock-clone dance。
    ///
    /// `ManuallyDrop` で local snapshot を確保 → seqlock validate → K::eq + V::clone。
    ///
    /// VERSION bit があるので Path A の cycle (CAS-EMPTY → write → store-back) 後の
    /// tag は元と必ず異なる 16 bit 値となり、`t1 != t2` で検出可能。
    /// (V: Clone soundness の clone-mid-flight race は module doc の caveat 参照。)
    #[inline]
    fn try_candidate(&self, pos: usize, key: &K, needle: u16) -> Probe<V> {
        let t1 = self.tags[pos].load(Ordering::Acquire);
        if (t1 & Self::SCAN_MASK) != needle {
            return Probe::Miss;
        }
        let id = Self::id_of(t1);
        let entries_base = self.entries_ptr();
        // SAFETY: ManuallyDrop で local の Drop を抑制。entries[id] が引き続き K, V の
        // 真の所有者であり、local は bitwise copy なので drop すると double-free。
        let buf: ManuallyDrop<Entry<K, V>> = unsafe {
            ManuallyDrop::new(std::ptr::read(entries_base.add(id) as *const Entry<K, V>))
        };
        let t2 = self.tags[pos].load(Ordering::Acquire);
        // 全 16 bit equality (VERSION 含む) で seqlock validate。
        // - Path A の cycle 後は VERSION が flip しているので t1 != t2 → retry
        // - Path C の shift 後は tag 自体が動いているので同様 retry
        if t1 != t2 || (t2 & LIVE) == 0 {
            // buf bytes は torn かもしれない。ManuallyDrop なので drop しないが、
            // 中身の K::eq / V::clone は **絶対に呼ばない** (ここで return)。
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

    /// `entries` Box の先頭 raw pointer。
    #[inline]
    fn entries_ptr(&self) -> *const MaybeUninit<Entry<K, V>> {
        // SAFETY: UnsafeCell::get 経由で slice の先頭 pointer を返す。
        unsafe { (*self.entries.get()).as_ptr() }
    }

    pub fn contains(&self, key: &K, hash: u64) -> bool {
        self.get_by_hash(key, hash).is_some()
    }

    /// c14s: 条件付き bounded retry。`find_get` が "racing" を観測した時のみ最大
    /// `MAX_READER_RETRY` 回まで再試行する。 racing 観測なしで None が返れば true-miss
    /// 確定で即 None を返す (= 全 slot を 1 回 scan するだけ)。
    ///
    /// 旧版 (無条件 4× retry) は uniform read-only のような miss-heavy workload で
    /// すべての lookup を 4 回 scan させ、HR 0 → c11s 比 -75% の壊滅的 regression を
    /// 引き起こしていた (`docs/reports/2026-05-08-c14s-sweep.md` の経緯参照)。
    pub fn get_by_hash(&self, key: &K, hash: u64) -> Option<V> {
        const MAX_READER_RETRY: usize = 4;
        let needle = Self::needle_from_hash(hash);
        let (v, mut racing) = self.find_get(key, needle);
        if let Some(v) = v {
            return Some(v);
        }
        if !racing {
            return None;
        }
        for _ in 1..MAX_READER_RETRY {
            hint::spin_loop();
            let (v, r) = self.find_get(key, needle);
            if let Some(v) = v {
                return Some(v);
            }
            racing = r;
            if !racing {
                return None;
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
                // Path A 成功: argument key は重複した K として scope 終了で drop される。
                // entries[id] の旧 K は新 Entry に move 済み (drop されない)、旧 V は
                // try_path_a 内で drop 済み。
                drop(key);
                None
            }
            Err(value) => self.path_bc(key, value, needle),
        }
    }

    /// Path A: lock-free CAS update for existing key.
    ///
    /// Returns:
    /// - `Ok(())` — Path A 成功、`value` は entries[id] に install 済み、旧 V は drop 済み
    /// - `Err(value)` — key not present 又は CAS contention exhausted、caller が Path B/C で再試行
    ///
    /// SIEVE 等価性: tag の HASH/ID/LIVE は復帰後も同値 (VERSION のみ flip)、hand/len 不変、
    /// visited を SET (sieve_orig の `freq=1` 一致)。eviction を起こさない。
    fn try_path_a(&self, key: &K, needle: u16, value: V) -> Result<(), V> {
        // c14s: CAS 失敗時は即 Mutex escalate。c13s の retry loop (MAX_RETRY=4) は
        // adversarial-hot で writer 同士の reload 競合を増やすだけだったため廃止。
        const MAX_RETRY: usize = 1;
        let mut value_holder = ManuallyDrop::new(value);
        for _ in 0..MAX_RETRY {
            // find_lockfree: pos と current tag を取得 (visited fetch_or は撃たない)
            let found = self.find_lockfree_for_path_a(key, needle);
            let (pos, expected_tag, id) = match found {
                Some(x) => x,
                None => {
                    // 該当 key は cache 内に居ない → Path B/C へ
                    let v = unsafe { ManuallyDrop::take(&mut value_holder) };
                    return Err(v);
                }
            };
            // tag CAS: expected_tag → EMPTY (slot 所有権獲得)
            // sentinel = EMPTY (LIVE OFF)、reader の SCAN_MASK 比較で確実に外れる。
            // Path C scan_evict / writer_find は EMPTY を見たら spin-wait する。
            match self.tags[pos].compare_exchange(
                expected_tag,
                EMPTY,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Ok(_) => {}
                Err(_) => {
                    // tag が動いた (別 writer の Path A or Path B/C 進行中)
                    // → MAX_RETRY 内で find からやり直し
                    continue;
                }
            }
            // CAS 成功: entries[id] への排他書き込み権獲得
            let entries_mut = self.entries.get();
            let value = unsafe { ManuallyDrop::take(&mut value_holder) };
            // SAFETY: tag CAS で entries[id] への並行アクセスを排他、id は expected_tag が
            //         LIVE 期間 init 済み slot を指していた (writer_find で確認済み)。
            let old_entry: Entry<K, V> = unsafe {
                let slot_ptr = (*entries_mut).as_ptr().add(id) as *const Entry<K, V>;
                std::ptr::read(slot_ptr)
            };
            unsafe {
                let slot_ptr = (*entries_mut).as_mut_ptr().add(id) as *mut Entry<K, V>;
                std::ptr::write(
                    slot_ptr,
                    Entry {
                        key: old_entry.key,
                        value,
                    },
                );
            }
            // old_entry.key は新 Entry に moved、old_entry.value のみが local に残り
            // 関数末で drop される。
            // Wait: partial move — Rust は old_entry.key を drop しない (moved out)、
            // old_entry.value のみ scope 末で drop される。
            //
            // ↓ 以下、scope 末で `drop(old_entry.value)` 相当が走る。
            //   明示の `drop(...)` は不要 (Rust 自動 drop で OK)。

            fence(Ordering::Release);
            // tag を flipped VERSION で CAS 復帰。CAS 失敗 = Path C の shift が `tags[pos]` を
            // 奪った (= `EMPTY` を別 id の next_tag で上書きした)。この場合 `entries[id]` への
            // 更新は失われない: Path C は entries[evict_id] のみを書き換え、shift は tags の
            // id 並びを 1 つ詰めるだけなので、`entries[id]` (= 元 `expected_tag` の id 部位) は
            // shift 後 `tags[pos-k]` 経由で参照される。`unconditional store` を使うと shift
            // 後の `tags[pos] = T_(a+1)` を `T_a ^ VERSION` で上書きし、`tags[pos-1]` (= 同 id
            // I_a を持つ) と重複してしまう (concurrent_invariants_under_zipf flake の root cause)。
            let new_tag = expected_tag ^ VERSION;
            let cas_back = self.tags[pos].compare_exchange(
                EMPTY,
                new_tag,
                Ordering::Release,
                Ordering::Acquire,
            );
            if cas_back.is_ok() {
                // visited SET (sieve_orig の `freq=1` と一致、c11s `writer_update_in_place` と同形)。
                let mask = Self::vbit_mask(pos);
                self.hot.visited.fetch_or(mask, Ordering::Relaxed);
            }
            // CAS 失敗時は visited を `pos` に立てない: その slot は今 別 id を指しており、
            // I_a の visited は shift 後の位置で表現されるべき。立ててしまうと別 entry が
            // visited と誤認される (sweep で 1 周損する程度の semantic ノイズ、UB ではない)。
            // partial-moved old_entry が scope 末で `old_entry.value` を drop する。
            // ↓ 明示的に drop して timing をはっきりさせる (Path A 完了後、即時 drop)。
            drop(old_entry.value);
            // suppress unused warning: id is decoded from expected_tag for slot ownership.
            let _ = (id, pos);
            return Ok(());
        }
        // MAX_RETRY 超過 → Path B/C に escalate
        let v = unsafe { ManuallyDrop::take(&mut value_holder) };
        Err(v)
    }

    /// Path A 用 find: pos と current tag (16 bit, VERSION 含む) と id を返す。
    /// reader の `try_candidate` と異なり visited fetch_or は撃たない (Path A の最後で SET する)。
    /// c14s: AVX2 ⇒ scalar の dispatch (c13s の scalar 64 scan を SIMD 4 chunk に短縮)。
    /// uniform write (= candidate 不在) で効果最大、hit ケースでも seqlock + K::eq は scalar の
    /// ままなので Path A の lock-free 性能は維持される。
    fn find_lockfree_for_path_a(&self, key: &K, needle: u16) -> Option<(usize, u16, usize)> {
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        {
            if std::is_x86_feature_detected!("avx2") {
                // SAFETY: avx2 runtime detect 済み、bmi1 は AVX2 capable CPU の前提。
                return unsafe { self.find_lockfree_for_path_a_avx2(key, needle) };
            }
        }
        self.find_lockfree_for_path_a_scalar(key, needle)
    }

    fn find_lockfree_for_path_a_scalar(&self, key: &K, needle: u16) -> Option<(usize, u16, usize)> {
        let entries_base = self.entries_ptr();
        let len = self.hot.len.load(Ordering::Acquire);
        for pos in 0..len {
            let t1 = self.tags[pos].load(Ordering::Acquire);
            if (t1 & Self::SCAN_MASK) != needle {
                continue;
            }
            if let Some(found) = self.try_path_a_candidate(pos, t1, key, entries_base) {
                return Some(found);
            }
        }
        None
    }

    #[cfg(all(target_arch = "x86_64", not(miri)))]
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_lockfree_for_path_a_avx2(
        &self,
        key: &K,
        needle: u16,
    ) -> Option<(usize, u16, usize)> {
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
                    // SIMD load (atomic 性質なし) と Acquire load の間に tag が動いた
                    // 可能性。改めて load してから SCAN_MASK と seqlock を検証する。
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

    /// SIMD scan が拾った candidate に対して seqlock 検証 + K::eq を行う。
    /// hit なら `(pos, t1, id)`、torn / mismatch なら None。
    #[inline]
    fn try_path_a_candidate(
        &self,
        pos: usize,
        t1: u16,
        key: &K,
        entries_base: *const MaybeUninit<Entry<K, V>>,
    ) -> Option<(usize, u16, usize)> {
        let id = Self::id_of(t1);
        // SAFETY: ManuallyDrop で torn buf を drop しないように包む (try_candidate と同じ)
        let buf: ManuallyDrop<Entry<K, V>> = unsafe {
            ManuallyDrop::new(std::ptr::read(entries_base.add(id) as *const Entry<K, V>))
        };
        let t2 = self.tags[pos].load(Ordering::Acquire);
        if t1 != t2 || (t2 & LIVE) == 0 {
            return None;
        }
        if buf.key == *key {
            return Some((pos, t1, id));
        }
        None
    }

    /// Path B (warmup install) と Path C (evict + shift + install) を writer Mutex 配下で実行。
    /// Path A が CAS exhausted で escalate してきたケースでは、Mutex 取得後に再 find して
    /// 既存 key 更新ならその場で行う (Mutex 配下なので CAS 不要、senba::Cache 流の
    /// `writer_update_in_place` 相当)。
    fn path_bc(&self, key: K, value: V, needle: u16) -> Option<(K, V)> {
        let mut state = self.hot.writer.lock();

        // (a) writer_find で既存 key を再確認 (Path A retry 中に別 writer が install した可能性)
        if let Some((pos, expected_tag)) = self.writer_find(&key, needle) {
            self.writer_update_in_place(pos, expected_tag, key, value);
            return None;
        }

        let len = self.hot.len.load(Ordering::Relaxed);
        // (b) Path B: warmup (len < cap)
        if len < self.capacity {
            self.writer_warmup_install(len, key, value, needle);
            return None;
        }

        // (c) Path C: 定常 evict + shift + install
        Some(self.writer_evict_and_install(&mut state, key, value, needle))
    }

    /// writer 内部 find: tags を Acquire load + key 比較。Mutex 配下だが Path A と並行する
    /// ので tag の WRITER claim sentinel (= EMPTY) が見える。EMPTY なら spin-wait。
    fn writer_find(&self, key: &K, needle: u16) -> Option<(usize, u16)> {
        let entries_base = self.entries_ptr();
        let len = self.hot.len.load(Ordering::Relaxed);
        for pos in 0..len {
            // Path A 進行中の slot (= EMPTY sentinel) は spin-wait で完了を待つ。
            // Path A は O(1) (CAS + ptr::read + ptr::write + fence + store + fetch_or) なので
            // 待ち時間は < 100ns 想定、livelock 無し。
            loop {
                let t = self.tags[pos].load(Ordering::Acquire);
                if t == EMPTY && pos < len {
                    // Path A 進行中 (LIVE が一時 EMPTY 化) → spin-wait
                    hint::spin_loop();
                    continue;
                }
                if (t & LIVE) == 0 {
                    // Path A が完了して tag が次に動いた / 真の EMPTY (range 外)
                    break;
                }
                if (t & Self::SCAN_MASK) != needle {
                    break;
                }
                let id = Self::id_of(t);
                let buf: ManuallyDrop<Entry<K, V>> = unsafe {
                    ManuallyDrop::new(std::ptr::read(entries_base.add(id) as *const Entry<K, V>))
                };
                let t2 = self.tags[pos].load(Ordering::Acquire);
                if t != t2 || (t2 & LIVE) == 0 {
                    continue;
                }
                if buf.key == *key {
                    return Some((pos, t));
                }
                break;
            }
        }
        None
    }

    /// writer Mutex 配下の既存 key 更新 (Path A 失敗後の escalate path)。
    /// CAS は不要、senba::Cache `writer_update_in_place` の atomic 版。
    /// VERSION bit を flip して reader の seqlock dance を fire させる。
    fn writer_update_in_place(&self, pos: usize, expected_tag: u16, key: K, value: V) {
        let id = Self::id_of(expected_tag);
        // 1. tag を一度 EMPTY 化 (reader の seqlock を fire)
        self.tags[pos].store(EMPTY, Ordering::Release);
        let entries_mut = self.entries.get();
        // SAFETY: writer Mutex 排他下、Path A も同 pos には EMPTY を踏んで spin で抜ける。
        //         id は LIVE tag が指していた有効 slot。
        unsafe {
            let slot_ptr = (*entries_mut).as_ptr().add(id) as *const Entry<K, V>;
            let old_entry: Entry<K, V> = std::ptr::read(slot_ptr);
            let slot_ptr_mut = (*entries_mut).as_mut_ptr().add(id) as *mut Entry<K, V>;
            std::ptr::write(
                slot_ptr_mut,
                Entry {
                    key: old_entry.key,
                    value,
                },
            );
            drop(old_entry.value); // 旧 V を drop
        }
        // 引数 `key` は重複した K として scope 末で drop される。
        drop(key);
        fence(Ordering::Release);
        // tag を flipped VERSION で復帰
        let new_tag = expected_tag ^ VERSION;
        self.tags[pos].store(new_tag, Ordering::Release);
        let mask = Self::vbit_mask(pos);
        self.hot.visited.fetch_or(mask, Ordering::Relaxed);
    }

    /// Path B: warmup install (len < capacity)。tags[len] に新 tag、entries[len] に新 entry。
    /// senba::Cache lineage で entry_id = len (warmup slot)。len += 1。
    fn writer_warmup_install(&self, len: usize, key: K, value: V, needle: u16) {
        let entry_id = len as u16;
        let entries_mut = self.entries.get();
        // SAFETY: writer Mutex 排他下、entries[len] は uninit slot。
        unsafe {
            let slot_ptr = (*entries_mut).as_mut_ptr().add(len) as *mut Entry<K, V>;
            std::ptr::write(slot_ptr, Entry { key, value });
        }
        // 新 install は visited=0 で開始 (sieve_orig も新 entry は freq=0)
        let mask = Self::vbit_mask(len);
        self.hot.visited.fetch_and(!mask, Ordering::Relaxed);
        // tag を Release store: VERSION = 0 で初期化
        let new_tag = LIVE | (entry_id << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        fence(Ordering::Release);
        self.tags[len].store(new_tag, Ordering::Release);
        self.hot.len.store(len + 1, Ordering::Release);
    }

    /// Path C: 定常 evict + shift + install。senba::Cache `Shard::insert:541-573` の atomic 版。
    /// shift は per-tag store + transient EMPTY 窓 (reader は seqlock retry で吸収)。
    fn writer_evict_and_install(
        &self,
        state: &mut WriterState,
        key: K,
        value: V,
        needle: u16,
    ) -> (K, V) {
        let cap = self.capacity;
        debug_assert_eq!(self.hot.len.load(Ordering::Relaxed), cap);
        // hand を範囲内に修正
        if state.hand >= cap {
            state.hand = 0;
        }
        // SIEVE hand 巡回で evict_pos を決定
        let evict_pos = self
            .scan_evict(state.hand, cap)
            .or_else(|| self.scan_evict(0, state.hand))
            .unwrap_or(state.hand);
        let evict_tag = self.read_live_tag_with_spin(evict_pos);
        let evict_id = Self::id_of(evict_tag);

        // 旧 entry を取り出し
        let entries_mut = self.entries.get();
        // SAFETY: LIVE tag が指していた有効 slot。tag は Mutex 配下で安定、Path A 並行は
        //         scan_evict / read_live_tag_with_spin で WRITER claim sentinel を spin-wait。
        let evicted_entry: Entry<K, V> = unsafe {
            let slot_ptr = (*entries_mut).as_ptr().add(evict_id) as *const Entry<K, V>;
            std::ptr::read(slot_ptr)
        };

        // shift: tags[evict_pos+1..cap] を tags[evict_pos..cap-1] に下げる
        // 各 tag を Release store、reader が中間状態を見ても seqlock retry で吸収する。
        for i in evict_pos..(cap - 1) {
            // 次 tag を Path A spin-wait しながら取得
            let next_tag = self.read_live_tag_with_spin(i + 1);
            // visited bit を pos i+1 から pos i に転記
            // c14s では s_word / d_word を別 word に振り分ける可能性があったが、c16s は
            // visited が 1 word (cap ≤ 64) なので src/dst は同 word 内 bit。
            let s_mask = Self::vbit_mask(i + 1);
            let d_mask = Self::vbit_mask(i);
            let was_visited = self.hot.visited.load(Ordering::Relaxed) & s_mask != 0;
            // 旧位置の visited を CLEAR
            self.hot.visited.fetch_and(!s_mask, Ordering::Relaxed);
            // 新位置の visited を was_visited で SET / CLEAR
            if was_visited {
                self.hot.visited.fetch_or(d_mask, Ordering::Relaxed);
            } else {
                self.hot.visited.fetch_and(!d_mask, Ordering::Relaxed);
            }
            // tag を移動: 一旦 EMPTY を経由 (reader の seqlock を fire)
            self.tags[i].store(EMPTY, Ordering::Release);
            fence(Ordering::Release);
            self.tags[i].store(next_tag, Ordering::Release);
        }
        // tags[cap-1] は shift 後の "末尾" だが旧 tag の coppy が残っている可能性がある。
        // 一度 EMPTY にしてから新 tag を書く。
        self.tags[cap - 1].store(EMPTY, Ordering::Release);

        // 新 entry を entries[evict_id] に install (id 再利用)
        // SAFETY: writer Mutex 排他下、evict_id は今しがた free にしたばかり。
        //         この slot に対する Path A 並行は無い (LIVE tag が無くなったので find が hit しない)。
        unsafe {
            let slot_ptr = (*entries_mut).as_mut_ptr().add(evict_id) as *mut Entry<K, V>;
            std::ptr::write(slot_ptr, Entry { key, value });
        }
        // 新 install の visited = 0
        let mask = Self::vbit_mask(cap - 1);
        self.hot.visited.fetch_and(!mask, Ordering::Relaxed);
        // 新 tag を tags[cap-1] (= 末尾、SIEVE order の "head") に書く。VERSION = 0 で初期化。
        let new_tag = LIVE | ((evict_id as u16) << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        fence(Ordering::Release);
        self.tags[cap - 1].store(new_tag, Ordering::Release);

        // hand 進め: senba::Cache の `pos < last ? pos : 0` ロジック
        // shift 後、evict_pos の successor は同じ evict_pos (shift で詰まったため)
        state.hand = if evict_pos < cap - 1 { evict_pos } else { 0 };

        (evicted_entry.key, evicted_entry.value)
    }

    /// hand 巡回: visited を見て立っていれば剥がす、立っていなければ evict 候補。
    /// Path A 進行中 (= EMPTY sentinel) の pos は spin-wait で完了を待つ。
    fn scan_evict(&self, lo: usize, hi: usize) -> Option<usize> {
        for i in lo..hi {
            let t = loop {
                let t = self.tags[i].load(Ordering::Acquire);
                if t == EMPTY {
                    // Path A 進行中 → spin-wait
                    hint::spin_loop();
                    continue;
                }
                break t;
            };
            // i < cap (= len) range 内なので t は必ず LIVE (Path A spin 後)。
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

    /// pos の LIVE tag を spin-wait しながら取得 (Path A 中の EMPTY を吸収)。
    /// Mutex 配下から呼ばれることを想定 (Mutex 配下では Path B/C が不在なので、
    /// EMPTY は必ず Path A に起因する)。
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
        // bounded retry は Shard::get_by_hash 内 (c14s 由来)。
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

    /// Path A の lock-free 経路を踏む確認: 既存 key を update したとき、Mutex を取得せずに
    /// (= 並行 reader を block せずに) 値が更新される。
    /// バグると Mutex 取得経由 (Path B/C escalate) になり、test 自体は pass するが
    /// 設計意図から外れる。Path A 専用 unit test として live_ids 確認。
    #[test]
    fn update_via_path_a_preserves_id() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.insert(3, 30);
        cache.insert(4, 40);
        let sh = cache.shard(0);
        let ids_before: Vec<usize> = sh.live_ids();
        // Path A update
        cache.insert(2, 222);
        let ids_after: Vec<usize> = sh.live_ids();
        // id 配置は不変 (Path A は entries[id] の V のみ書き換え、tag の id 部位は元値)
        assert_eq!(
            ids_before, ids_after,
            "Path A update が id mapping を変えている (= 想定外の Path C 経路に落ちた)"
        );
        assert_eq!(cache.get(&2), Some(222));
    }

    /// VERSION bit が flip することの確認: 同じ key を Path A で 2 回 update すると
    /// tag の VERSION bit (0x4000) が反転する (HASH/ID/LIVE 部位は不変)。
    #[test]
    fn path_a_flips_version_bit() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        cache.insert(1, 10);
        cache.insert(2, 20);
        let sh = cache.shard(0);
        // Find pos of key=2
        let mut pos2: Option<usize> = None;
        for i in 0..sh.tags.len() {
            let t = sh.tags[i].load(Ordering::Acquire);
            if t & LIVE != 0 && Shard::<u64, u64>::id_of(t) == 1 {
                // entry_id 1 が key=2 の slot (id_of warmup = pos = len-1)
                pos2 = Some(i);
                break;
            }
        }
        let pos2 = pos2.expect("key=2 not found in tags");
        let tag1 = sh.tags[pos2].load(Ordering::Acquire);
        cache.insert(2, 222);
        let tag2 = sh.tags[pos2].load(Ordering::Acquire);
        // VERSION bit のみ flip、他は同一
        assert_eq!(
            tag1 ^ tag2,
            VERSION,
            "Path A が VERSION bit を flip していない"
        );
        // 値は更新されている
        assert_eq!(cache.get(&2), Some(222));
    }

    /// 既存キー update が visited を 1 に SET (sieve_orig の `freq=1` と一致)。
    /// バグると update 後の entry が次の sweep で即 evict されて Test が失敗する。
    #[test]
    fn update_existing_key_sets_visited_like_oracle() {
        let cache: ConcurrentSieveCache<i32, i32, 1> = ConcurrentSieveCache::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.insert(1, 11); // update via Path A
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

    /// reader hit が tag を変更しないことの確認 (visited 分離が機能している不変条件)。
    #[test]
    fn reader_hit_does_not_modify_tag() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        cache.insert(1, 100);
        let sh = cache.shard(0);
        let tag_before = sh.tags[0].load(Ordering::Acquire);
        assert_eq!(cache.get(&1), Some(100));
        let tag_after = sh.tags[0].load(Ordering::Acquire);
        assert_eq!(
            tag_before, tag_after,
            "reader hit が tag を変更している (visited 分離が崩れている)"
        );
        let mask = Shard::<u64, u64>::vbit_mask(0);
        assert!(
            sh.hot.visited.load(Ordering::Acquire) & mask != 0,
            "visited bit が立っていない"
        );
    }

    /// senba::Cache の shift-on-evict と同じ ID 再利用を c14s でも守っているかの確認。
    /// Path C で eviction が起きたあと、新 entry の id は evicted entry の id を再利用、
    /// tags 配列上の position は cap-1 (末尾) に install される。
    #[test]
    fn evict_reuses_id_at_tail_position() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        for k in 0..4u64 {
            cache.insert(k, k * 10);
        }
        let sh = cache.shard(0);
        let ids_before: Vec<usize> = sh.live_ids();
        // 全 LIVE で len = 4
        assert_eq!(sh.live_count(), 4);
        assert_eq!(ids_before, vec![0, 1, 2, 3]);
        // Path C で 1 個 evict
        let evicted = cache.insert(99, 9900);
        assert!(evicted.is_some());
        // len は依然 4
        assert_eq!(sh.live_count(), 4);
        // 新 entry は cap-1 = pos 3 にあり、id は evicted_id を再利用
        let last_tag = sh.tags[3].load(Ordering::Acquire);
        let last_id = Shard::<u64, u64>::id_of(last_tag);
        // evicted (k=0) の id は 0 だったので、新 entry の id も 0
        assert_eq!(last_id, 0, "Path C で id 再利用していない");
    }

    /// 並行不変条件の一括検証 (zipf workload で短時間多 thread)。
    /// - len <= cap
    /// - shard ごとの live_ids に重複なし
    /// - 各 shard の live_count == len
    /// - 全 key の get 結果が破壊されていない (k で insert したら k に近い値が返る)
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
                assert_eq!(v, k, "key {k} の value が破壊されている");
            }
        }
    }

    /// Path A は eviction を起こさない。既存 key を多数回 update しても evicted (K, V) は
    /// 返らない (insert の戻り値 = None)。これが c12s と異なる core property。
    #[test]
    fn path_a_does_not_evict() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        for k in 0..4u64 {
            assert_eq!(cache.insert(k, k), None);
        }
        // 既存 key を 100 回 update
        for _ in 0..100 {
            for k in 0..4u64 {
                assert_eq!(
                    cache.insert(k, k * 1000),
                    None,
                    "Path A update が evicted を返した (= Path C に落ちた)"
                );
            }
        }
        // 全 key が生存、最新 value が読み出せる
        for k in 0..4u64 {
            assert_eq!(cache.get(&k), Some(k * 1000));
        }
    }

    /// sieve_orig (oracle) と外部一致: 1 shard 同期で SIEVE 意味論完全一致。
    /// c12s では `c12s_1shard_diverges_from_orig_on_synthetic_zipf` が divergent を確認していたが、
    /// c14s は SIEVE 等価なのでここで一致する。詳細は research/tests/oracle.rs。
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
                "1-shard で sieve_orig と c14s が key {k} で食い違う"
            );
        }
    }

    #[test]
    fn bit_layout_exclusivity_u64_u64() {
        type S = Shard<u64, u64>;
        // Entry<u64,u64> は 16 byte ⇒ ID_SHIFT = 4
        assert_eq!(S::ID_SHIFT, 4);
        assert_eq!(S::ID_MASK, 0x03f0);
        // hash mask は LIVE と VERSION と ID を除いた 8 bit。
        // 0x3FFF & !0x03f0 = 0x3c0f
        assert_eq!(S::HASH_MASK, 0x3c0f);
        assert_eq!(S::SCAN_MASK, LIVE | S::HASH_MASK);

        // LIVE | VERSION | ID | HASH の 4 区画で 0xFFFF を埋め切る。
        assert_eq!(LIVE | VERSION | S::ID_MASK | S::HASH_MASK, 0xFFFF);
        assert_eq!(LIVE & VERSION, 0);
        assert_eq!(LIVE & S::ID_MASK, 0);
        assert_eq!(LIVE & S::HASH_MASK, 0);
        assert_eq!(VERSION & S::ID_MASK, 0);
        assert_eq!(VERSION & S::HASH_MASK, 0);
        assert_eq!(S::ID_MASK & S::HASH_MASK, 0);
        // hash mask の有意 bit 数は 8 (c8 と同じ)。
        assert_eq!(S::HASH_MASK.count_ones(), 8);
        // SCAN_MASK は VERSION を含まない。
        assert_eq!(S::SCAN_MASK & VERSION, 0);
    }
}
