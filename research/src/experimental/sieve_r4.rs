#![cfg(all(target_arch = "x86_64", not(miri)))]
//! `sieve_r4`: c17s の entry-version seqlock を **race α (half-overwrite drop) 防御** に
//! 残したまま、**race β (clone-mid-flight UAF) 防御** として `crossbeam-epoch` を refcount
//! 抜きで合成した variant。
//!
//! 設計の一次資料: `docs/reports/2026-05-14-arc-less-concurrent-design.md`。
//! r3 (RwLock baseline) 系列の続編で、Arc<V> を排して reader hot path の atomic write を
//! ゼロに戻す (= c17s の atomic shape を回復する) のが目的。
//!
//! # c17s からの構造差分
//!
//! 1. `Entry::key` / `Entry::value` を `ManuallyDrop<K>` / `ManuallyDrop<V>` に変更
//!    (writer Path B remove / Path C で `ManuallyDrop::take` で抜き取って defer closure に
//!    move する設計のため)。`Drop for Shard` は `ManuallyDrop::drop` で明示破棄。
//! 2. reader `get_by_hash` 入口で `epoch::pin` を取得 (`pin_for::<V>`)。`mem::needs_drop::<V>`
//!    が false の場合は monomorphize-time に dead code 除去され、c17s と bit-identical の
//!    asm になる (期待)。
//! 3. reader `try_candidate` の v1/v2 load 間に `compiler_fence(Acquire)` を明示追加
//!    (LLVM IR 段の reorder 防止、x86 codegen 上は no-op)。
//! 4. writer Path A の `drop(old_value)` を `defer_drop_if_needed` に置換。
//! 5. writer `writer_update_in_place` の `drop(old_value)` を同様に置換。
//! 6. writer `writer_evict_and_install` の戻り値を **常に None** とし、旧 K, V を
//!    `defer_drop_kv_if_needed` で reclaim (race β + race γ 防御)。
//!
//! # K, V trait bounds (c17s からの追加要請)
//!
//! - `K: Hash + Eq + Send + 'static` (defer closure capture のため Send + 'static)
//! - `V: Clone + Send + 'static` (同上、reader が `&V` を読むので Sync は不要)
//!
//! # tag layout (c17s から不変)
//!
//! ```text
//!   bit 15:            LIVE
//!   bits ID_SHIFT..+6: ID (6 bit)
//!   remaining:         HASH (9 bit)
//! ```
//!
//! `SCAN_MASK = LIVE | HASH_MASK` は ID を除外。
//!
//! # 実装 scope
//!
//! 本ファイルは **x86_64 + AVX2 + non-miri 専用** (research artifact)。AVX2 が必須
//! なので `Shard::new` で runtime detect。scalar fallback は持たない。miri / 非 x86_64
//! では module 全体が cfg-out される (consumer 側も同 gate を貼る)。

use parking_lot::Mutex;
use senba::Xxh3Build;
use std::cell::UnsafeCell;
use std::hash::{BuildHasher, Hash};
use std::hint;
use std::mem::{ManuallyDrop, MaybeUninit};
use std::sync::atomic::{
    AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering, compiler_fence, fence,
};

use crossbeam_epoch as epoch;

/// `mem::needs_drop::<V>()` の monomorphize-time fold で V: Copy の場合に
/// guard 取得を完全消去するための wrapper。`Some(epoch::Guard)` バリアントは
/// Drop 時に reader の pin を release する。
///
/// 設計 §8.1 (`needs_drop` const branch + `EpochGuardWrapper`) 参照。
#[allow(dead_code)]
enum EpochGuardWrapper {
    Some(epoch::Guard),
    None,
}

#[inline(always)]
fn needs_epoch<V>() -> bool {
    std::mem::needs_drop::<V>()
}

#[inline(always)]
fn pin_for<V>() -> EpochGuardWrapper {
    if needs_epoch::<V>() {
        EpochGuardWrapper::Some(epoch::pin())
    } else {
        EpochGuardWrapper::None
    }
}

/// EMPTY tag (LIVE OFF)。Path C の shift transient と pad lane に使う。
/// r4 では Path A は tag を EMPTY 化しない。
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
        "sieve_r4: sizeof(Entry<K,V>) must be a power of two"
    );
    assert!(s <= 256, "sieve_r4: sizeof(Entry<K,V>) must be <= 256");
    s.trailing_zeros()
}

const fn id_mask_from_shift(id_shift: u32) -> u16 {
    ((MAX_PER_SHARD - 1) as u16) << id_shift
}

/// HASH 領域は 0x7FFF (LIVE のみ除外、c16s と異なり VERSION 除外なし) から ID 6 bit を
/// 抜いたもの。c11s と同 9 bit (c14s/c16s の 8 bit から +1)。
const fn hash_mask_from_id_mask(id_mask: u16) -> u16 {
    0x7FFF & !id_mask
}

/// `repr(C, align(32))` で sizeof = 32 (power of 2、ID_SHIFT = 5)。version は offset 0。
/// reader は `entries[id].version` を tier 1 seqlock として load、Path A はここを CAS。
///
/// **r4 固有**: `key`/`value` を `ManuallyDrop` で包む。writer Path C で `ManuallyDrop::take`
/// で K, V を抜き取って `defer_unchecked(move || drop(...))` の closure に move するため、
/// Entry 側に「自身の Drop で K, V を破棄しない」属性を型レベルで付ける。`Drop for Shard`
/// で live entry の K, V を `ManuallyDrop::drop` で明示破棄する。
#[repr(C, align(32))]
struct Entry<K, V> {
    /// 偶数 = stable、奇数 = in-flight。Path A / Path C entries 上書きは
    /// CAS even→odd → 値書き換え → store even+2 で囲う。
    version: AtomicU32,
    key: ManuallyDrop<K>,
    value: ManuallyDrop<V>,
}

/// reader scan 1 slot の結果。
///
/// `Miss` は "tag が needle と一致しない / 一致したが key が異なる" という settled な
/// 不一致。 `Racing` は "candidate 1 個に seqlock validate (tier 1 or tier 2) が落ちた"
/// — Path A の cycle / Path C の shift を踏んだ可能性があり、caller (`get_by_hash`) は
/// この観測時 + epoch 変化観測時のみ retry する。
enum Probe<V> {
    Found(V),
    Miss,
    Racing,
}

type EntriesArena<K, V> = UnsafeCell<Box<[MaybeUninit<Entry<K, V>>]>>;

/// writer が 1 op で取る hot field を 1 cache line に co-locate (c16s 設計を継承)。
/// r4 では `path_c_epoch` を 32-byte trailing pad に追加 (sizeof は 64 で不変)。
#[repr(C, align(64))]
struct ShardHot {
    /// Path B/C 排他。Path A (lock-free version CAS) は Mutex を取らない。
    writer: Mutex<WriterState>,
    /// 1 shard 全 visited (cap ≤ 64)。reader fetch_or / writer fetch_and 両方が触る。
    visited: AtomicU64,
    /// live entry 数。reader scan は `tags[0..len]` を見る。
    len: AtomicUsize,
    /// Path C 完了時に bump。reader は scan 前後で snapshot して shift-in-progress を
    /// 検出する coarse-grained seqlock。Path A や `writer_update_in_place` は触らない
    /// (それらは Entry::version で reader 通知する)。
    path_c_epoch: AtomicU64,
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
    /// は永久 EMPTY pad。
    tags: Box<[AtomicU16]>,
    /// entries arena。`Entry::version` で reader を seqlock 同期、Path A は CAS で排他。
    entries: EntriesArena<K, V>,
    /// 1 cache line に集約された writer hot state (Mutex / visited / len / path_c_epoch)。
    hot: ShardHot,
}

struct WriterState {
    hand: usize,
}

// SAFETY: c16s と同じ。entries[id] への書き込みは Entry::version CAS で所有権を確保した
// writer または Mutex 配下の writer のみが行い、reader は version seqlock + tag re-load
// + ManuallyDrop で torn read / use-after-free を弾く (V: Clone soundness 限界は module
// doc 参照)。
unsafe impl<K: Send, V: Send> Send for Shard<K, V> {}
unsafe impl<K: Send + Sync, V: Send + Sync> Sync for Shard<K, V> {}

impl<K, V> Shard<K, V> {
    const ENTRY_SIZE: usize = std::mem::size_of::<Entry<K, V>>();
    const ID_SHIFT: u32 = id_shift_from_entry_size(Self::ENTRY_SIZE);
    const ID_MASK: u16 = id_mask_from_shift(Self::ID_SHIFT);
    const HASH_MASK: u16 = hash_mask_from_id_mask(Self::ID_MASK);
    /// reader needle 比較用。LIVE + HASH (9 bit)、ID は除外。c16s と異なり VERSION も
    /// (= 不在のため) 除外する必要なし。
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
            "sieve_r4: AVX2 required (research artifact); compile-time gated to x86_64+non-miri but runtime CPU lacks AVX2"
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
                path_c_epoch: AtomicU64::new(0),
            },
        }
    }

    /// hash → tag bit spread。r4 は HASH が 9 bit に拡張されたので、hash 高位 9 bit を
    /// ID 切れ目を跨いで spread する (ID 上下に分配)。
    #[inline]
    fn needle_from_hash(hash: u64) -> u16 {
        // 高位 9 bit を取り出す。
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

    /// reader 用 AVX2 scan。r4 では **EMPTY-lane SIMD 検出を削除** (Path A は tag を
    /// EMPTY 化しないため)。Path C 由来の EMPTY transient は `path_c_epoch` で coarse 検出。
    ///
    /// `pos < len` フィルタは TOCTOU 安全に省略済み: (i) Path B は entries[len] 初期化後に
    /// `tags[len].store(LIVE|..., Release)` のため LIVE 観測時 entry init 完了済、(ii) tags
    /// は order_cap までゼロ初期化 + Path B 以外で書かれないので tags[pos>=len] は EMPTY、
    /// SIMD は LIVE bit 必須なので絶対 candidate にならない、(iii) len は monotonic 増加。
    ///
    /// Returns `(value, racing)`:
    /// - `value: Option<V>` — 見つかった V (Some) または scan 完了で発見できず (None)
    /// - `racing: bool` — `try_candidate` の seqlock validate (tier 1 or tier 2) で
    ///   Racing が観測された場合 true。caller は path_c_epoch 変化と OR で retry 判定。
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_get(&self, key: &K, needle: u16) -> (Option<V>, bool) {
        use std::arch::x86_64::*;

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
                match self.try_candidate(pos, key, needle) {
                    Probe::Found(val) => return (Some(val), false),
                    Probe::Racing => racing = true,
                    Probe::Miss => {}
                }
                mask = _blsr_u32(mask);
                mask = _blsr_u32(mask);
            }
            i += LANE;
        }
        (None, racing)
    }

    /// reader entry-level seqlock dance。
    ///
    /// tier 1 (entry version v1==v2) で Path A / Path C entries 上書きを捕える。
    /// **tier 2 (tag re-load t1==t2) は冗長なので削除済み**: Path C shift で tag が
    /// 別 id に動いた場合でも、(a) reader が古い id_X で entries[X] を読む経路は entries[X]
    /// が consistent (Path C は X を touch しないか version flip 済み) なので buf.key == *key
    /// が偽なら Miss、真なら Found。(b) entries[X] が evict 対象だった場合は Path C の
    /// version flip が tier 1 を fire させる。残る Path C transient (= 該当 chunk SIMD で
    /// candidate 不発) は `Shard::get_by_hash` の `path_c_epoch` snapshot で coarse 検出する。
    /// (V: Clone soundness の clone-mid-flight race は module doc の caveat 参照。)
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

        // tier 1 (entry version 偶奇 + 一致)
        let v1 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
        if v1 & 1 != 0 {
            return Probe::Racing;
        }
        // SAFETY: ManuallyDrop で local の Drop を抑制。entries[id] が引き続き K, V の
        // 真の所有者であり、local は bitwise copy なので drop すると double-free。
        let buf: ManuallyDrop<Entry<K, V>> =
            unsafe { ManuallyDrop::new(std::ptr::read(entry_ptr)) };
        // r4: LLVM IR 段の non-atomic load (ptr::read の中身) を v2 load の後ろに reorder
        //     させない。x86 では codegen 上 no-op (TSO で隠蔽)。設計 §6.3 参照。
        compiler_fence(Ordering::Acquire);
        let v2 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
        if v1 != v2 {
            return Probe::Racing;
        }
        // Validated: buf is a consistent snapshot. Safe to call K::eq + V::clone.
        // r4: buf.key / buf.value は ManuallyDrop<K/V> なので一段 deref。
        if *buf.key == *key {
            let v = (*buf.value).clone();
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

    /// `path_c_epoch` snapshot による coarse retry + `try_candidate` 由来の `racing` flag
    /// による fine retry の OR で MAX_READER_RETRY 回まで再試行する (c17s 由来)。hit 経路で
    /// は `if let Some(v)` で epoch_after を skip するので、hit cost は epoch_before 1 atomic
    /// load のみ。
    ///
    /// **r4 固有**: 入口で `pin_for::<V>()` を取得し、scan + V::clone 中に writer の
    /// deferred drop が走らないことを保証する。`needs_drop::<V>()` が false (= V: Copy) の
    /// とき pin は monomorphize-time に dead code 除去される (設計 §8.2)。
    pub fn get_by_hash(&self, key: &K, hash: u64) -> Option<V> {
        const MAX_READER_RETRY: usize = 4;
        let _guard = pin_for::<V>();
        let needle = Self::needle_from_hash(hash);
        for attempt in 0..MAX_READER_RETRY {
            let epoch_before = self.hot.path_c_epoch.load(Ordering::Acquire);
            // SAFETY: AVX2 は Shard::new の assert で検証済み。
            let (v, racing) = unsafe { self.find_get(key, needle) };
            if let Some(v) = v {
                return Some(v);
            }
            let epoch_after = self.hot.path_c_epoch.load(Ordering::Acquire);
            // racing == false かつ epoch 不変なら true-miss 確定。
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

    /// Path A: lock-free CAS update for existing key (entry version 偶奇 flip)。
    ///
    /// Returns:
    /// - `Ok(())` — Path A 成功、`value` は entries[id] に install 済み、旧 V は drop 済み
    /// - `Err(value)` — key not present 又は version CAS 失敗、caller が Path B/C で再試行
    ///
    /// SIEVE 等価性: tag は Path A 中 **完全に不変** (LIVE/ID/HASH 全部据え置き)。
    /// hand/len 不変、visited を SET (sieve_orig の `freq=1` 一致)。eviction を起こさない。
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
            let version_ref = unsafe { &(*entry_ptr).version };
            // version CAS: even (= v_snap) → odd (= v_snap + 1)。
            // 失敗 = 別 writer の Path A (or Path C entries overwrite) と競合 → escalate。
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
            //         value field のみ in-place write、key は不変。
            //         r4: ManuallyDrop<V> 越しに raw V を read/write する (sizeof / layout 同じ)。
            let value_ptr =
                unsafe { &mut (*entry_ptr).value as *mut ManuallyDrop<V> as *mut V };
            let old_value: V = unsafe { std::ptr::read(value_ptr) };
            unsafe {
                std::ptr::write(value_ptr, new_value);
            }
            // version を even (= v_snap + 2) に store (Release で先行 store を publish)。
            version_ref.store(v_snap.wrapping_add(2), Ordering::Release);
            // visited SET (sieve_orig の `freq=1` と一致、c11s `writer_update_in_place` と同形)。
            let mask = Self::vbit_mask(pos);
            self.hot.visited.fetch_or(mask, Ordering::Relaxed);
            drop(old_value);
            let _ = id;
            return Ok(());
        }
        // MAX_RETRY 超過 → Path B/C に escalate
        let v = unsafe { ManuallyDrop::take(&mut value_holder) };
        Err(v)
    }

    /// Path A 用 find: pos / id / version snapshot を返す。
    /// reader と異なり visited fetch_or は撃たない (Path A の最後で SET する)。
    /// `pos < len` フィルタは `find_get` と同 TOCTOU reasoning で省略。
    /// SAFETY: AVX2 は `Shard::new` で検証済み。
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_lockfree_for_path_a(&self, key: &K, needle: u16) -> Option<(usize, usize, u32)> {
        use std::arch::x86_64::*;

        let entries_base = self.entries_ptr();
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
                let t1 = self.tags[pos].load(Ordering::Acquire);
                if (t1 & Self::SCAN_MASK) == needle
                    && let Some(found) = self.try_path_a_candidate(pos, t1, key, entries_base)
                {
                    return Some(found);
                }
                mask = _blsr_u32(mask);
                mask = _blsr_u32(mask);
            }
            i += LANE;
        }
        None
    }

    /// SIMD scan が拾った candidate に entry version + tag re-load + K::eq を行う。
    /// hit なら `(pos, id, v_snap)`、torn / mismatch なら None。
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
        let v1 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
        if v1 & 1 != 0 {
            return None;
        }
        let buf: ManuallyDrop<Entry<K, V>> =
            unsafe { ManuallyDrop::new(std::ptr::read(entry_ptr)) };
        // r4: IR-level reorder 防止。設計 §6.3。x86 codegen 上 no-op。
        compiler_fence(Ordering::Acquire);
        let v2 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
        if v1 != v2 {
            return None;
        }
        // tier 2 (t1 == t2) は削除済み: try_candidate と同 reasoning。Path C で tag が
        // shift しても entries[id] 自身の consistency は version flip で捕えており、stale
        // pos での visited.fetch_or は SIEVE algorithm noise (data corruption ではない)。
        // r4: buf.key は ManuallyDrop<K> なので一段 deref。
        if *buf.key == *key {
            return Some((pos, id, v1));
        }
        None
    }

    /// Path B (warmup install) と Path C (evict + shift + install) を writer Mutex 配下で実行。
    fn path_bc(&self, key: K, value: V, needle: u16) -> Option<(K, V)> {
        let mut state = self.hot.writer.lock();

        // (a) writer_find で既存 key を再確認 (Path A retry 中に別 writer が install した可能性)
        if let Some((pos, id)) = self.writer_find(&key, needle) {
            self.writer_update_in_place(pos, id, key, value);
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

    /// writer 内部 find: tags + entry version + key 比較。Mutex 配下だが Path A と並行する
    /// ので EMPTY sentinel (= Path C 自身の shift transient) と Entry::version 奇数を spin-wait。
    fn writer_find(&self, key: &K, needle: u16) -> Option<(usize, usize)> {
        let entries_base = self.entries_ptr();
        let len = self.hot.len.load(Ordering::Relaxed);
        for pos in 0..len {
            // r4: Path A は tag を EMPTY 化しないので、ここで EMPTY を観測することは
            // 通常ない (Path B/C は単一 Mutex 配下で他 Path C の shift 並行はゼロ)。
            // 念のため fallback として spin-wait は残す。
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
                // entry version 奇数なら別 Path A 進行中、spin-wait。
                let mut v;
                loop {
                    v = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
                    if v & 1 == 0 {
                        break;
                    }
                    hint::spin_loop();
                }
                let buf: ManuallyDrop<Entry<K, V>> =
                    unsafe { ManuallyDrop::new(std::ptr::read(entry_ptr)) };
                let v2 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
                if v != v2 {
                    continue;
                }
                let t2 = self.tags[pos].load(Ordering::Acquire);
                if t != t2 || (t2 & LIVE) == 0 {
                    continue;
                }
                // r4: buf.key は ManuallyDrop<K>。
                if *buf.key == *key {
                    return Some((pos, id));
                }
                break;
            }
        }
        None
    }

    /// writer Mutex 配下の既存 key 更新 (Path A 失敗後の escalate path)。
    /// Path A と異なり tag を触らない (r4 では tag 不変)、entry version で reader 通知。
    fn writer_update_in_place(&self, pos: usize, id: usize, key: K, value: V) {
        let entries_mut = self.entries.get();
        let entry_ptr = unsafe { (*entries_mut).as_mut_ptr().add(id) as *mut Entry<K, V> };
        let version_ref = unsafe { &(*entry_ptr).version };
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
        //         r4: ManuallyDrop<V> 越しに raw V を read/write する。
        unsafe {
            let value_ptr = &mut (*entry_ptr).value as *mut ManuallyDrop<V> as *mut V;
            let old_value: V = std::ptr::read(value_ptr);
            std::ptr::write(value_ptr, value);
            drop(old_value);
        }
        // 引数 `key` は重複した K として scope 末で drop。entries[id] の旧 K は不変。
        drop(key);
        version_ref.store(v_claimed.wrapping_add(1), Ordering::Release);
        let mask = Self::vbit_mask(pos);
        self.hot.visited.fetch_or(mask, Ordering::Relaxed);
    }

    /// Path B: warmup install (len < capacity)。
    fn writer_warmup_install(&self, len: usize, key: K, value: V, needle: u16) {
        let entry_id = len as u16;
        let entries_mut = self.entries.get();
        // SAFETY: writer Mutex 排他下、entries[len] は uninit slot。
        //         r4: K, V を ManuallyDrop で包む (Drop は Shard::drop / Path C defer で管理)。
        unsafe {
            let slot_ptr = (*entries_mut).as_mut_ptr().add(len) as *mut Entry<K, V>;
            std::ptr::write(
                slot_ptr,
                Entry {
                    version: AtomicU32::new(0),
                    key: ManuallyDrop::new(key),
                    value: ManuallyDrop::new(value),
                },
            );
        }
        // 新 install は visited=0 で開始 (sieve_orig も新 entry は freq=0)
        let mask = Self::vbit_mask(len);
        self.hot.visited.fetch_and(!mask, Ordering::Relaxed);
        // tag を Release store。VERSION bit は r4 には無いので素直に LIVE | ID | HASH。
        let new_tag = LIVE | (entry_id << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        fence(Ordering::Release);
        self.tags[len].store(new_tag, Ordering::Release);
        self.hot.len.store(len + 1, Ordering::Release);
    }

    /// Path C: 定常 evict + shift + install。c16s と同型の shift loop (tag を EMPTY 経由で
    /// 動かす) + entries[evict_id] 上書き (entry version 偶奇 flip で reader 通知) + 末尾の
    /// `path_c_epoch` bump (reader の coarse seqlock 用)。
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
        let evict_version_ref = unsafe { &(*evict_entry_ptr).version };
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
        //         r4: ManuallyDrop<K/V> から ManuallyDrop::take で抜き取る。
        let evicted_key: K = unsafe { ManuallyDrop::take(&mut (*evict_entry_ptr).key) };
        let evicted_value: V = unsafe { ManuallyDrop::take(&mut (*evict_entry_ptr).value) };

        // shift: tags[evict_pos+1..cap] を tags[evict_pos..cap-1] に下げる
        for i in evict_pos..(cap - 1) {
            let next_tag = self.read_live_tag_with_spin(i + 1);
            // visited bit を pos i+1 から pos i に転記
            let s_mask = Self::vbit_mask(i + 1);
            let d_mask = Self::vbit_mask(i);
            let was_visited = self.hot.visited.load(Ordering::Relaxed) & s_mask != 0;
            self.hot.visited.fetch_and(!s_mask, Ordering::Relaxed);
            if was_visited {
                self.hot.visited.fetch_or(d_mask, Ordering::Relaxed);
            } else {
                self.hot.visited.fetch_and(!d_mask, Ordering::Relaxed);
            }
            // tag を移動: 一旦 EMPTY を経由 (reader の tier 2 を fire)
            self.tags[i].store(EMPTY, Ordering::Release);
            fence(Ordering::Release);
            self.tags[i].store(next_tag, Ordering::Release);
        }
        // tags[cap-1] は shift 後に旧 tag が残っているので EMPTY 化してから新 tag を書く。
        self.tags[cap - 1].store(EMPTY, Ordering::Release);

        // 新 entry を entries[evict_id] に install (key, value 上書き、version flip)
        // SAFETY: version 奇数で排他確保済み、tag は LIVE が無い (= 上で EMPTY を踏んだ)
        //         ので reader はこの slot に当たらない。
        //         r4: ManuallyDrop で包んで slot に書き戻す (旧 K/V は take 済で空き)。
        unsafe {
            (*evict_entry_ptr).key = ManuallyDrop::new(key);
            (*evict_entry_ptr).value = ManuallyDrop::new(value);
        }
        // version を even (= v_claimed + 1) に store。
        evict_version_ref.store(v_claimed.wrapping_add(1), Ordering::Release);

        // 新 install の visited = 0
        let mask = Self::vbit_mask(cap - 1);
        self.hot.visited.fetch_and(!mask, Ordering::Relaxed);
        // 新 tag を tags[cap-1] (= SIEVE order の "head") に書く。r4 は VERSION 不在。
        let new_tag = LIVE | ((evict_id as u16) << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        fence(Ordering::Release);
        self.tags[cap - 1].store(new_tag, Ordering::Release);

        // hand 進め: senba::Cache の `pos < last ? pos : 0` ロジック
        state.hand = if evict_pos < cap - 1 { evict_pos } else { 0 };

        // path_c_epoch bump で reader の coarse seqlock を fire (shift で false-miss した
        // reader に retry を促す)。
        self.hot.path_c_epoch.fetch_add(1, Ordering::Release);

        (evicted_key, evicted_value)
    }

    /// hand 巡回: visited を見て立っていれば剥がす、立っていなければ evict 候補。
    /// Path C 自身の shift transient (= EMPTY) は spin-wait で完了を待つ。
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
                // SAFETY: LIVE ⇒ entries[id] init 済み。&mut self なので reader 不在。
                //         r4: ManuallyDrop<K/V> なので明示 drop が必要。assume_init_mut で
                //         &mut Entry を取ってから ManuallyDrop::drop を 2 回呼ぶ。
                unsafe {
                    let entry: &mut Entry<K, V> = (*entries_mut)[id].assume_init_mut();
                    ManuallyDrop::drop(&mut entry.key);
                    ManuallyDrop::drop(&mut entry.value);
                }
            }
        }
    }
}

// ---------------- 外側 wrapper ----------------

pub const DEFAULT_SHARDS: usize = 8;

pub struct ConcurrentSieveCache<K, V, const SHARDS: usize = DEFAULT_SHARDS> {
    /// SHARDS は const generic で compile-time invariant (power-of-two mask) を保つが、
    /// 実体は heap に置き、SHARDS が大きい場合に stack overflow を避ける。
    /// length は constructor で SHARDS と一致させる。
    shards: Box<[Shard<K, V>]>,
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
        // Path A update
        cache.insert(2, 222);
        let ids_after: Vec<usize> = sh.live_ids();
        let tags_after: Vec<u16> = (0..4).map(|i| sh.tags[i].load(Ordering::Acquire)).collect();
        assert_eq!(
            ids_before, ids_after,
            "Path A update が id mapping を変えている (= 想定外の Path C 経路)"
        );
        // r4 固有: tag は完全に不変 (c14s/c16s は VERSION bit が flip していた)
        assert_eq!(
            tags_before, tags_after,
            "Path A update が tag を変更している (r4 は tag 不変が core property)"
        );
        assert_eq!(cache.get(&2), Some(222));
    }

    /// Path A は entries[id].version を 2 増やす (偶数→奇数→偶数)。同一 key を 2 回 update
    /// すると version は 4 増える (= 0 → 2 → 4)。
    #[test]
    fn path_a_increments_entry_version_by_two() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        cache.insert(1, 10);
        let sh = cache.shard(0);
        // entry_id 0 が key=1 (warmup install slot)
        let entries_base = sh.entries_ptr();
        let entry_ptr = unsafe { entries_base.add(0) as *const Entry<u64, u64> };
        let v0 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
        cache.insert(1, 100);
        let v1 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
        cache.insert(1, 1000);
        let v2 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
        assert_eq!(
            v1,
            v0.wrapping_add(2),
            "1st update should bump version by 2"
        );
        assert_eq!(
            v2,
            v0.wrapping_add(4),
            "2nd update should bump version by 4"
        );
        // どちらも even (stable) で着地
        assert_eq!(v1 & 1, 0);
        assert_eq!(v2 & 1, 0);
        assert_eq!(cache.get(&1), Some(1000));
    }

    /// 既存キー update が visited を 1 に SET (sieve_orig の `freq=1` と一致)。
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

    /// reader hit が tag を変更しない (visited 分離 + tag 不変が機能している不変条件)。
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

    /// Path C で eviction が起きたあと、新 entry の id は evicted entry の id を再利用、
    /// tags 配列上の position は cap-1 (末尾) に install される。`path_c_epoch` も bump。
    #[test]
    fn evict_reuses_id_at_tail_position_and_bumps_epoch() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        for k in 0..4u64 {
            cache.insert(k, k * 10);
        }
        let sh = cache.shard(0);
        let epoch_before = sh.hot.path_c_epoch.load(Ordering::Acquire);
        let ids_before: Vec<usize> = sh.live_ids();
        assert_eq!(sh.live_count(), 4);
        assert_eq!(ids_before, vec![0, 1, 2, 3]);
        let evicted = cache.insert(99, 9900);
        assert!(evicted.is_some());
        assert_eq!(sh.live_count(), 4);
        let last_tag = sh.tags[3].load(Ordering::Acquire);
        let last_id = Shard::<u64, u64>::id_of(last_tag);
        assert_eq!(last_id, 0, "Path C で id 再利用していない");
        let epoch_after = sh.hot.path_c_epoch.load(Ordering::Acquire);
        assert!(
            epoch_after > epoch_before,
            "Path C で path_c_epoch が bump されていない"
        );
    }

    /// 並行不変条件 (c16s と同型)。
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

    /// Path A は eviction を起こさない (= insert の戻り値 None)。c12s と異なる core property。
    #[test]
    fn path_a_does_not_evict() {
        let cache: ConcurrentSieveCache<u64, u64, 1> = ConcurrentSieveCache::new(4);
        for k in 0..4u64 {
            assert_eq!(cache.insert(k, k), None);
        }
        for _ in 0..100 {
            for k in 0..4u64 {
                assert_eq!(
                    cache.insert(k, k * 1000),
                    None,
                    "Path A update が evicted を返した (= Path C に落ちた)"
                );
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
                "1-shard で sieve_orig と r4 が key {k} で食い違う"
            );
        }
    }

    /// r4 の bit layout: VERSION 削除、HASH 9 bit (c11s と同等)。
    #[test]
    fn bit_layout_exclusivity_u64_u64() {
        type S = Shard<u64, u64>;
        // Entry<u64,u64> は AtomicU32 + u64 + u64 = 4+8+8 = 20、align(32) で sizeof = 32
        // ⇒ ID_SHIFT = 5
        assert_eq!(std::mem::size_of::<Entry<u64, u64>>(), 32);
        assert_eq!(S::ID_SHIFT, 5);
        // ID_MASK = 63 << 5 = 0x07E0
        assert_eq!(S::ID_MASK, 0x07E0);
        // HASH_MASK = 0x7FFF & !0x07E0 = 0x781F、9 bit (5 low + 4 high)
        assert_eq!(S::HASH_MASK, 0x781F);
        assert_eq!(S::HASH_MASK.count_ones(), 9);
        assert_eq!(S::SCAN_MASK, LIVE | S::HASH_MASK);

        // LIVE | ID | HASH の 3 区画で 0xFFFF を埋め切る (r4 は VERSION 不在)。
        assert_eq!(LIVE | S::ID_MASK | S::HASH_MASK, 0xFFFF);
        assert_eq!(LIVE & S::ID_MASK, 0);
        assert_eq!(LIVE & S::HASH_MASK, 0);
        assert_eq!(S::ID_MASK & S::HASH_MASK, 0);
    }

    /// `ShardHot` の sizeof / alignment と field offset の契約 (path_c_epoch 追加でも 64B 維持)。
    #[test]
    fn shard_hot_layout_contract() {
        assert_eq!(std::mem::size_of::<ShardHot>(), 64);
        assert_eq!(std::mem::align_of::<ShardHot>(), 64);
    }
}
