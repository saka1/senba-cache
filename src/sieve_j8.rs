//! J8 — j7 + tag 内 entry_id 埋込 + entries arena = capacity (slack なし) + free_list 廃止
//! + **§8.2(a) BLSR ×2** + **§8.3(a) sizeof(Entry)-aware bit layout**。
//!
//! ## 動機 (`docs/improvement-ideas.md` §M1, §M2.3, §M5.3 を統合;
//!         `docs/reports/2026-05-06-j8-candidate-loop-analysis.md` §8.2(a) + §8.3(a) を反映)
//!
//! j7 (M2.3) は u16 tag に live + visited + 14-bit hash を packing して
//! Twitter cluster018 全帯域で j5/j6 を支配したが、`order_cap = 2 × capacity`
//! の slack を **inline 物理 36 B/cap** が払う形になり、orig (25 B/cap) との
//! memory-fair 比較ではハンデが残った。
//!
//! j8 は次の 3 つの直交アイディアを 1 設計に畳み込む:
//!
//! 1. **slack を片側に寄せる** (§M5.3): tags は `2 × capacity` (slack 持ち、
//!    tombstone 用)、entries は `capacity` (slack なし)
//! 2. **tag bit に entry_id を埋める** (本設計の中核): u16 tag に
//!    `[live(1) | visited(1) | hash + id (14 bit)]` を packing。`order` 別配列が
//!    不要になり、entries 1× cap と整合
//! 3. **free_list 廃止**: insert API のみで `remove` を露出しないため、
//!    evict が返した freed_id は同一 insert 呼び出し内で必ず消費される
//!    → 保管不要
//!
//! 結果として **inline 物理 20 B/cap** (j7 比 −44%、orig 比 −20%) を
//! 達成しつつ、SIEVE 意味論は j7 と完全一致 (= `sieve_orig` oracle 通過)。
//!
//! ## bit レイアウト (§8.3(a) sizeof(Entry) 連動版)
//!
//! 元設計 (§M5.3) は id を bits 8..13 に固定していたが、`find_avx2` の inner
//! ループ末尾に **「id × sizeof(Entry) のための `shl ebx, 4`」が残る** という
//! 課題があった (`docs/reports/2026-05-06-j8-candidate-loop-analysis.md` §4.1)。
//!
//! 本実装はこれを **bit レイアウト変更で消す**:
//!
//! ```text
//! ID_SHIFT = log2(sizeof(Entry<K, V>))     ; 2 の冪を要求 (1..=256 byte)
//! ID_MASK  = ((MAX_PER_SHARD - 1) as u16) << ID_SHIFT
//!          = 6-bit id を tag 内 [ID_SHIFT, ID_SHIFT+6) 区間に置く
//! HASH_MASK = bits 0..14 のうち id 領域以外  (常にちょうど 8 bit)
//! ```
//!
//! 具体例 — bench / 実測の主流ケース `Entry<u64, u64>` (sizeof=16):
//!
//! ```text
//! ID_SHIFT = 4                ; log2(16) = 4
//! ID_MASK  = 0x03f0           ; bits 4..9
//! HASH_MASK = 0x3c0f          ; bits 0..3 (低 4 bit) + bits 10..13 (高 4 bit)
//! SCAN_MASK = LIVE | HASH_MASK = 0xbc0f
//!
//!   bit 15  : live
//!   bit 14  : visited
//!   bits 10..13 : hash 高 4 bit
//!   bits 4..9   : id (6 bit)
//!   bits 0..3   : hash 低 4 bit
//! ```
//!
//! ### なぜこれで `shl` が消えるか (§8.3(a) のキモ)
//!
//! id (6 bit) を **`log2(sizeof(Entry))` ビット目から始める** と:
//!
//! ```text
//! tag & ID_MASK
//!   = id の bit パターンがそのまま「ID_SHIFT 桁 左に詰まった値」
//!   = id × (1 << ID_SHIFT)
//!   = id × sizeof(Entry)
//!   = entries 配列内の **byte offset**
//! ```
//!
//! が成立する。よって `(entries_ptr as *const u8).add(tag & ID_MASK)` で 1 命令直接
//! Entry に到達でき、`movzx → and → shl → cmp+load` の 4 命令連鎖が
//! `movzx → and → cmp+load` の 3 命令に縮む (Path A −1 cy)。
//!
//! ### sizeof 制約と非対応サイズ
//!
//! `assert!(sizeof(Entry).is_power_of_two() && sizeof <= 256)` を const eval
//! で要求する。bench/oracle テストで使う型 (`Entry<u64,u64>=16`,
//! `Entry<i32,i32>=8`, `Entry<u64,String>=32` 等) は全て power-of-2 で適合。
//! &str (= 16 byte) を組み合わせた `Entry<i32, &str>` (= 24 byte) のような
//! 非 2 冪サイズはコンパイル時 panic する — その場合は 2 冪型に
//! padding するか別 variant で実装する。
//!
//! - SIMD scan の比較対象は `LIVE | HASH_MASK` (= `SCAN_MASK`)。visited と id bit は
//!   mask out されるので scan の一致判定に影響しない。
//! - `MAX_PER_SHARD = 64` を構造的上限として `Inner::new` で `assert!`。
//!   per_shard sweet spot ∈ [16, 32] (§7.3) と整合する。
//!
//! ## 不変条件
//!
//! - I4: live tag の集合 = `{ tags[i] : i < tail, tags[i] & LIVE != 0 }`、個数 = `len`
//! - I5: live tag が指す entry_id の集合は重複なく、サイズ = `len`
//! - I6: I5 集合の id についてのみ `entries[id]` は init 済み
//! - I7: I5 集合 ⊆ `0..capacity`
//! - I8: warm-up 中 (= 一度も evict が走っていない) は I5 集合 = `0..len` (連続)
//!
//! I8 が `entry_id = self.len` (warm-up 時) の正当性を担保する。

use crate::hash::Xxh3Build;
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
///
/// 制約: 2 の冪 かつ `<= 256`。
/// - 2 の冪でないと `id × sizeof = id << ID_SHIFT = tag & ID_MASK` の同値が壊れる。
/// - 256 を超えると ID_SHIFT >= 9 で id 領域 (6 bit) が visited bit (14) に侵食する。
const fn id_shift_from_entry_size(s: usize) -> u32 {
    assert!(
        s.is_power_of_two(),
        "sieve_j8: sizeof(Entry<K,V>) must be a power of two (extend with padding if needed)"
    );
    assert!(s <= 256, "sieve_j8: sizeof(Entry<K,V>) must be <= 256");
    // s >= 1 は is_power_of_two が保証 (0 is not power of two)。
    s.trailing_zeros()
}

/// 6-bit id を「左に ID_SHIFT 詰めた」値の集合をカバーする mask。
const fn id_mask_from_shift(id_shift: u32) -> u16 {
    ((MAX_PER_SHARD - 1) as u16) << id_shift
}

/// hash は bits 0..14 のうち id 領域以外。常にちょうど 8 bit 確保される
/// (1 + 1 + 6 + 8 = 16 を割り当てるレイアウトなので算術的に保証)。
const fn hash_mask_from_id_mask(id_mask: u16) -> u16 {
    // bits 0..13 全体 (= LIVE/VISITED 以外) から id 領域を抜いたもの。
    0x3FFF & !id_mask
}

struct Entry<K, V> {
    key: K,
    value: V,
}

/// 1 shard 分の SIEVE。
struct Inner<K, V> {
    capacity: usize,
    /// 並列配列 #1: tag 列。`order_cap = 2 × capacity` の LANE 揃え (slack 持ち)。
    tags: Vec<u16>,
    /// 並列配列 #2: entries arena。`capacity` (slack なし)。
    /// id (= tag に埋め込んだ 6 bit) で indexing する。
    entries: Vec<MaybeUninit<Entry<K, V>>>,
    /// tags への次挿入位置 (`0..=order_cap`)。
    tail: usize,
    /// SIEVE hand cursor (`0..=tail`)、tags 上を巡回。
    hand: usize,
    /// 現在 live な entry 数 (= live tag 数)。
    len: usize,
}

// レイアウト関連 (bounds なし) — Drop からも呼び出せるよう Hash + Eq の外側に置く。
impl<K, V> Inner<K, V> {
    /// `Entry<K, V>` のサイズ。`entries_ptr.add(id)` 相当を byte 単位
    /// arithmetic で行う際の倍率。`ID_SHIFT` で `log2()` を取る前提なので
    /// 2 の冪である必要がある (see `id_shift_from_entry_size`)。
    const ENTRY_SIZE: usize = std::mem::size_of::<Entry<K, V>>();
    /// id (6 bit) を tag のどの bit 位置に置くか。bit pattern が
    /// **`id × sizeof(Entry)` の値と一致する** ように `log2(ENTRY_SIZE)` を取る。
    const ID_SHIFT: u32 = id_shift_from_entry_size(Self::ENTRY_SIZE);
    /// id 領域を覆う mask。`tag & ID_MASK = id × sizeof(Entry)`
    /// (= entries 内の byte offset) の関係が成立する。
    const ID_MASK: u16 = id_mask_from_shift(Self::ID_SHIFT);
    /// hash 領域を覆う mask (常にちょうど 8 bit 分散)。
    const HASH_MASK: u16 = hash_mask_from_id_mask(Self::ID_MASK);
    /// SIMD scan の比較対象。visited と id を mask out して live + hash のみで突合。
    const SCAN_MASK: u16 = LIVE | Self::HASH_MASK;

    /// tag から id (0..MAX_PER_SHARD) を抽出する。
    /// SIMD inner ループでは「byte offset そのもの」(= `tag & ID_MASK`) を使うので
    /// この関数を使わない方がコード生成上有利。スカラー path / drop / evict 用。
    #[inline]
    fn id_of(tag: u16) -> usize {
        ((tag & Self::ID_MASK) >> Self::ID_SHIFT) as usize
    }
}

impl<K, V> Inner<K, V>
where
    K: Hash + Eq,
{
    fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        assert!(
            capacity <= MAX_PER_SHARD,
            "per-shard capacity ({capacity}) must be <= {MAX_PER_SHARD} (6-bit ID limit)"
        );
        let raw = capacity.checked_mul(2).expect("capacity * 2 overflow");
        // tags 側は j7 と同じく LANE 揃え。tail 範囲外は EMPTY=0 を保つ
        // ことで SIMD scan の false hit を防ぐ。
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

    /// 64-bit hash の上位 8 bit を tag の hash 部に流し込む。
    /// shard 選択は下位 log2(SHARDS) bit なので独立 entropy。
    ///
    /// `HASH_MASK` は §8.3(a) のレイアウト変更により id 領域を挟んで分断される
    /// (例: ID_SHIFT=4 では bits 0..3 + bits 10..13)。
    /// 8 bit の hash を「低 ID_SHIFT bit」と「残り (8 − ID_SHIFT) bit」に
    /// 分割して、それぞれを HASH_MASK の低位/高位サブブロックに置く。
    ///
    /// - 低位: hash の bit 0..(ID_SHIFT-1) を tag の bit 0..(ID_SHIFT-1) にそのまま
    /// - 高位: hash の bit ID_SHIFT..7 を tag の bit (ID_SHIFT+6)..13 へ
    ///   (id 領域 6 bit を「飛び越える」ので追加で `<< 6` シフト)
    ///
    /// ID_SHIFT=4 のとき:
    /// `tag = ((h & 0x0F) as u16) | (((h & 0xF0) as u16) << 6)` で HASH_MASK=0x3c0f に
    /// 一致するペイロードを得る。
    #[inline]
    fn needle_from_hash(hash: u64) -> u16 {
        let h = (hash >> 56) as u8;
        let s = Self::ID_SHIFT;
        // ID_SHIFT >= 8 のときは元レイアウト同等で hash 全 8 bit が低位に連続。
        let spread = if s >= 8 {
            h as u16
        } else {
            // 低位: 下位 ID_SHIFT bit
            let low_mask: u8 = ((1u32 << s) - 1) as u8;
            let low = (h & low_mask) as u16;
            // 高位: 残り bit を 6 bit (= id 幅) 分追加でシフトして
            // id 領域を飛び越える。例: ID_SHIFT=4, h=0xF0 → 0x3C00。
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
                let e = unsafe { self.entries[id].assume_init_ref() };
                if &e.key == key {
                    return Some(i);
                }
            }
        }
        None
    }

    /// AVX2 + BMI1: `vpand` (SCAN_MASK) → `vpcmpeqw` → `vpmovmskb` → inner candidate
    /// ループ。`docs/reports/2026-05-06-j8-candidate-loop-analysis.md` §8.2(a) +
    /// §8.3(a) の最適化を反映:
    ///
    /// **§8.3(a) bit レイアウトトリック (Path A 短縮)**
    /// id を `[ID_SHIFT, ID_SHIFT+6)` に置いたので `tag & ID_MASK` がそのまま
    /// `id × sizeof(Entry)` (= entries arena 内の **byte offset**) になる。
    /// よって `(entries_ptr as *const u8).add(id_bytes)` 1 命令で Entry に到達でき、
    /// 旧実装の `movzx → and 0x3f → shl 4 → cmp+load` (4 命令連鎖) が
    /// `movzx → and 0x03f0 → cmp+load` (3 命令) に縮む。Path A の dep chain −1 cy。
    ///
    /// **§8.2(a) BLSR ×2 (Path B 短縮)**
    /// `vpcmpeqw + vpmovmskb` の出力は **必ず偶数位置のビットペア** (epi16 一致が
    /// 2 byte/lane なので bit が 2 連で立つ)。BMI1 の `BLSR (= x & (x − 1))` は
    /// 「最下位 1 ビットをクリア」する命令なので、2 回適用するとちょうど
    /// 1 ペア (= 1 candidate 分) が落ちる:
    ///
    /// ```text
    ///   mask = ...0011_0000   ; lane=2 が match
    ///   blsr → ...0010_0000  ; 最下位 1 (bit 4) を消す
    ///   blsr → ...0000_0000  ; 残った上位 (bit 5) を消す
    /// ```
    ///
    /// 旧 `mask &= !(0b11 << (lane << 1))` は `mov + shl + not + and` の
    /// 4 ops + tzcnt との依存連鎖で **Path B = 7 cy**。BLSR ×2 は **2 ops、
    /// tzcnt 結果に依存しない** (mask だけ参照) ので **Path B = 2 cy**。
    /// false-match 連発時の inner ループ throughput が直接効く。
    ///
    /// `_blsr_u32` intrinsic を直書きして LLVM が BLSR を確実に出すよう強制している
    /// (`x & (x - 1)` パターンは出ないことがある)。`bmi1` は AVX2-capable CPU
    /// (Haswell 2013+) では同梱なので `is_x86_feature_detected!("avx2")` の下では
    /// 実行時に必ず利用可能 — `target_feature` で `bmi1` を有効化して inline 化を許す。
    ///
    /// **§8.4(c) chunk base ptr hoist (Path A 更に短縮 + inner ops 削減)**
    /// `tzcnt(mask)` の戻り値 `bit` は `vpmovmskb` の性質から
    /// `bit = lane * 2` (lane = u16 lane index) で、しかも u16 1 個 = 2 byte なので
    /// **`bit` がそのまま「chunk 内の byte offset」**。outer ループで
    /// `chunk_byte_ptr = tags_byte_ptr + i * 2` を 1 回計算しておけば、inner では
    /// `chunk_byte_ptr + bit` で目的の tag を直接 load できる:
    ///
    /// - 旧: `tzcnt → mov,shr (lane=bit>>1) → or (pos=i+lane) → movzwl [tags+pos*2]`
    ///   (4 ops 連鎖、依存深さ 3 cy)
    /// - 新: `tzcnt → movzwl [chunk+bit]` (2 ops 連鎖、依存深さ 1 cy)
    ///
    /// `lane = bit >> 1; pos = i + lane` の計算は **hit (success path) でしか
    /// 必要ない** (= return 値の構築) ので、外に出してから条件分岐後に行う。
    /// false-match を 1 周回す inner ループの本体はそのぶん軽くなる。
    ///
    /// outer ループ側の追加コストは `lea chunk, [tags + i*2]` 1 命令のみ。
    /// per_shard=16 (運用 sweet spot) では candidate 数 ≈ 0.69/scan なので
    /// inner −3 ops × 0.69 = −2.1 ops/scan、outer +1 op で純減 −1.1 ops/scan。
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,bmi1")]
    unsafe fn find_avx2(&self, key: &K, needle: u16) -> Option<usize> {
        use std::arch::x86_64::*;
        let limit = self.tags.len();
        let tags_ptr = self.tags.as_ptr();
        // §8.4(c): tags を **byte pointer** でも保持しておく。
        // inner ループでは `chunk_byte_ptr + bit` の形で tag を直接 load する
        // (bit は tzcnt(mask) の戻り値で「chunk 内 byte offset」と同値)。
        let tags_byte_ptr = tags_ptr as *const u8;
        // entries を **byte ポインタ**で持つ。`tag & ID_MASK` (= id × sizeof(Entry))
        // を直接 byte offset として加算するため (§8.3(a))。
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

            // §8.4(c): chunk 先頭の **byte pointer** を outer で 1 回作る。
            // i は u16 単位の index なので byte 単位では `i * 2`。
            // この計算 (lea 1 命令) を outer に追い出すことで inner からは
            // `lane = bit >> 1; pos = i + lane` の 3 ops が消える。
            let chunk_byte_ptr = unsafe { tags_byte_ptr.add(i * 2) };

            while mask != 0 {
                // bit = `vpmovmskb` 結果 mask の最下位 set bit 位置。
                // - vpcmpeqw は一致 lane を 2 byte 全部 0xFF にする
                // - vpmovmskb は各 byte の MSB を 1 bit に圧縮
                //   ⟹ 一致 lane k は mask の bits 2k, 2k+1 が両方立つ「ペア」
                // - tzcnt は最下位 set bit (= 2k) を返す
                //
                // ここで u16 1 個 = 2 byte なので **`bit` (= 2k) はそのまま
                // chunk 内 byte offset** = `lane * sizeof(u16)`。
                // → `chunk_byte_ptr + bit` で当該 u16 tag を直接 load できる。
                let bit = mask.trailing_zeros() as usize;

                // tag を読み戻して id 部の bit を抽出。bit pattern が
                // 「id × sizeof(Entry)」と一致するので shift 不要 — そのまま byte offset (§8.3(a))。
                //
                // SAFETY: chunk_byte_ptr は tags 配列内、bit ≦ 31 (mask は u32 で
                // 取り得る最大 bit = 31)、chunk 1 個分 = 32 byte のため境界内。
                // u16 alignment: bit は必ず偶数 (vpmovmskb の偶数側 bit が tzcnt で取れる)
                // かつ tags は u16 整列 → アライン済 read。
                let tag = unsafe { *(chunk_byte_ptr.add(bit) as *const u16) } as u32;
                let id_bytes = (tag & id_mask_u32) as usize;
                // SAFETY:
                // - needle は LIVE bit を含む → cmp 一致 ⟹ tag も live ⟹ entries[id] init 済み (I6)
                // - id_bytes = id × sizeof(Entry) で id < MAX_PER_SHARD ≦ capacity
                //   ⟹ entries arena (capacity 要素 = capacity × sizeof byte) の境界内
                // - sizeof(Entry) is power of two (id_shift_from_entry_size の assert)
                //   ⟹ id_bytes は Entry の alignment 倍数 (実体 alignment は power of two のため)
                let entry_ptr = unsafe {
                    entries_byte_ptr.add(id_bytes) as *const MaybeUninit<Entry<K, V>>
                };
                let e = unsafe { (*entry_ptr).assume_init_ref() };
                if &e.key == key {
                    // §8.4(c): success path 限定で lane / pos を計算。
                    // failure を回し続ける inner 側からはこの 2 ops を追い出した。
                    let lane = bit >> 1;
                    return Some(i + lane);
                }
                // §8.2(a): BLSR ×2 で「最下位ペア」を 1 候補ぶん落とす。
                // tzcnt 結果 (= bit) と独立な依存関係なので OOO の観点でも
                // 次 iter の tzcnt をブロックしない。
                // (`unsafe fn find_avx2` のスコープ内なので追加 unsafe ブロックは不要。)
                mask = _blsr_u32(mask); // 最下位 1 bit を消去
                mask = _blsr_u32(mask); // 残った上位 (= ペアの上位) も消去
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
        // visited をセット: tags 配列内 in-place、find が触ったキャッシュライン内。
        self.tags[pos] |= VISITED;
        let id = Self::id_of(self.tags[pos]);
        // SAFETY: pos は find が tag マッチを確認した位置 (= live)。
        let e = unsafe { self.entries[id].assume_init_ref() };
        Some(&e.value)
    }

    fn insert(&mut self, key: K, value: V, hash: u64) -> Option<(K, V)> {
        let needle = Self::needle_from_hash(hash);
        if let Some(pos) = self.find(&key, needle) {
            let id = Self::id_of(self.tags[pos]);
            // SAFETY: find が live を確認した。
            let e = unsafe { self.entries[id].assume_init_mut() };
            e.value = value;
            self.tags[pos] |= VISITED;
            return None;
        }

        // 新規 insert: entry_id を取得。
        // - warm-up (len < capacity): I8 より `entry_id = len` が未使用 slot を指す
        // - steady (len == capacity): evict 直後の freed_id を pass-through
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
        // 新規挿入は visited=0。entry_id を ID_SHIFT 左シフトして id 領域に配置、
        // hash bit は needle の HASH_MASK 部 (= LIVE 以外) をそのまま流し込む。
        self.tags[pos] = LIVE | (entry_id << Self::ID_SHIFT) | (needle & Self::HASH_MASK);
        // SAFETY: entry_id は warm-up なら未使用、steady なら直前 evict の
        // assume_init_read で uninit に戻った slot。
        self.entries[entry_id as usize].write(Entry { key, value });
        self.len += 1;

        evicted
    }

    /// SIEVE の victim 探索 + freed entry_id を返す。
    /// j3/j5/j6/j7 と同じ「2 パス + first_live フォールバック」。
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
        // SAFETY: live を呼び出し側で保証済み。assume_init_read 後 entries[id] は uninit。
        let entry = unsafe { self.entries[id as usize].assume_init_read() };
        self.tags[pos] = EMPTY;
        self.len -= 1;
        self.hand = pos + 1;
        if self.hand >= self.tail {
            self.hand = 0;
        }
        ((entry.key, entry.value), id)
    }

    /// tags のみ前詰め。entries arena は不変 (= id-based indexing なので物理位置を動かす必要がない)。
    /// j7 比で memcpy 量 1/9 (`2 B vs 18 B per slot`)。
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
                // tag だけ前詰め。tag に埋まった id は不変なので
                // entries arena の物理対応関係は保たれる。
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
}

impl<K, V> Drop for Inner<K, V> {
    fn drop(&mut self) {
        // tags scan で live tag を列挙し、id 抽出して entries[id] を drop。
        // I5 (id 重複なし) より同じ entries[id] の二重 drop は起きない。
        for i in 0..self.tail {
            let t = self.tags[i];
            if t != EMPTY {
                let id = Self::id_of(t);
                // SAFETY: live ⟹ entries[id] init 済み (I6)。
                unsafe { self.entries[id].assume_init_drop() };
            }
        }
    }
}

// ---------------- 外側 (set-associative wrapper) ----------------

pub const DEFAULT_SHARDS: usize = 8;

pub struct SieveCache<K, V, const SHARDS: usize = DEFAULT_SHARDS> {
    shards: [Inner<K, V>; SHARDS],
    hasher: Xxh3Build,
}

impl<K, V, const SHARDS: usize> SieveCache<K, V, SHARDS>
where
    K: Hash + Eq,
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
        let shards: [Inner<K, V>; SHARDS] = std::array::from_fn(|i| {
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

    #[inline]
    fn shard_of_hash(hash: u64) -> usize {
        // 下位ビットで shard 選択。tag は上位 8 bit → 独立 entropy。
        (hash as usize) & (SHARDS - 1)
    }
}

impl<K, V, const SHARDS: usize> crate::Cache<K, V> for SieveCache<K, V, SHARDS>
where
    K: Hash + Eq,
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
    //! j7 のテストミラー + j8 固有の bit layout / id embed テスト + j7/sieve_orig oracle。

    use super::*;

    /// テスト用: SHARDS=8 で per_shard <= MAX_PER_SHARD を保つため
    /// 全体 cap も 8 × 64 = 512 を超えないように選ぶ。
    const TEST_SHARDS: usize = DEFAULT_SHARDS;

    #[test]
    fn cache_initially_empty() {
        let cache: SieveCache<i32, i32> = SieveCache::new(TEST_SHARDS * 4);
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.capacity(), TEST_SHARDS * 4);
        assert!(cache.is_empty());
    }

    // sizeof(Entry<i32, i32>) = 8 (= 2^3) で id_shift_from_entry_size の制約を満たす。
    // 元 j7 テストは `&str` 値型を使っていたが Entry が 24 byte (非 2 冪) になり
    // §8.3(a) レイアウトと両立しないので i32 値で書き換えている。

    #[test]
    fn insert_then_get() {
        let mut cache: SieveCache<i32, i32> = SieveCache::new(TEST_SHARDS * 4);
        assert!(cache.insert(1, 10).is_none());
        assert_eq!(cache.get(&1), Some(&10));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn get_missing_returns_none() {
        let mut cache: SieveCache<i32, i32> = SieveCache::new(TEST_SHARDS * 4);
        cache.insert(1, 10);
        assert_eq!(cache.get(&2), None);
    }

    #[test]
    fn contains_key_reflects_insertions() {
        let mut cache: SieveCache<i32, i32> = SieveCache::new(TEST_SHARDS * 4);
        assert!(!cache.contains_key(&1));
        cache.insert(1, 10);
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
    }

    #[test]
    fn insert_existing_key_updates_value() {
        let mut cache: SieveCache<i32, i32> = SieveCache::new(TEST_SHARDS * 4);
        cache.insert(1, 10);
        assert!(cache.insert(1, 20).is_none());
        assert_eq!(cache.get(&1), Some(&20));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn evicts_oldest_when_full_and_unvisited() {
        let mut cache: SieveCache<i32, i32, 1> = SieveCache::new(2);
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
        let mut cache: SieveCache<i32, i32, 1> = SieveCache::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.get(&1);
        let evicted = cache.insert(3, 30);
        assert_eq!(evicted, Some((2, 20)));
    }

    #[test]
    fn all_visited_clears_bits_then_evicts() {
        let mut cache: SieveCache<i32, i32, 1> = SieveCache::new(2);
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
        let mut cache: SieveCache<u64, u64> = SieveCache::new(cap);
        for k in 0..10_000u64 {
            cache.insert(k, k);
            assert!(cache.len() <= cap);
        }
        assert_eq!(cache.len(), cap);
    }

    #[test]
    fn churn_keeps_a_full_capacity_set() {
        let cap = TEST_SHARDS * 16;
        let mut cache: SieveCache<u64, u64> = SieveCache::new(cap);
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

    #[test]
    #[should_panic]
    fn capacity_below_shards_panics() {
        let _: SieveCache<u64, u64> = SieveCache::new(TEST_SHARDS - 1);
    }

    #[test]
    #[should_panic]
    fn non_power_of_two_shards_panics() {
        let _: SieveCache<u64, u64, 3> = SieveCache::new(9);
    }

    #[test]
    #[should_panic]
    fn per_shard_above_max_panics() {
        // per_shard = 65 > MAX_PER_SHARD=64 ⇒ panic。
        let _: SieveCache<u64, u64, 1> = SieveCache::new(65);
    }

    #[test]
    fn per_shard_at_max_works() {
        // per_shard = 64 = MAX_PER_SHARD は OK (id 6 bit が 0..63 を使い切る)。
        let mut cache: SieveCache<u64, u64, 1> = SieveCache::new(64);
        for k in 0..200u64 {
            cache.insert(k, k * 11);
        }
        assert_eq!(cache.len(), 64);
    }

    #[test]
    fn works_with_non_default_shards() {
        let mut cache_2: SieveCache<u64, u64, 2> = SieveCache::new(64);
        let mut cache_16: SieveCache<u64, u64, 16> = SieveCache::new(64);
        for k in 0..1000u64 {
            cache_2.insert(k, k);
            cache_16.insert(k, k);
        }
        assert!(cache_2.len() <= 64);
        assert!(cache_16.len() <= 64);
        assert_eq!(cache_2.capacity(), 64);
        assert_eq!(cache_16.capacity(), 64);
    }

    /// MAX_PER_SHARD まで詰めて全 hit を確認 (false-match が起きても key 等価で弾ける)。
    #[test]
    fn distinct_keys_full_per_shard_all_hit() {
        let n: u64 = 64;
        let mut cache: SieveCache<u64, u64, 1> = SieveCache::new(n as usize);
        for k in 0..n {
            cache.insert(k, k * 7);
        }
        for k in 0..n {
            assert_eq!(cache.get(&k), Some(&(k * 7)), "miss for key {k}");
        }
    }

    /// j7 と外部一致: 同じ trace を流して各 key の get 結果が一致。
    #[test]
    fn matches_j7_externally() {
        use crate::sieve_j7::SieveCache as J7;
        let cap = 128usize;
        let mut a: J7<u64, u64, 8> = J7::new(cap);
        let mut b: SieveCache<u64, u64, 8> = SieveCache::new(cap);
        for k in 0..10_000u64 {
            let key = (k.wrapping_mul(2654435761)) % 1024;
            let _ = a.insert(key, key);
            let _ = b.insert(key, key);
        }
        for k in 0..1024u64 {
            assert_eq!(
                a.get(&k).copied(),
                b.get(&k).copied(),
                "j7 と j8 が key {k} で食い違う"
            );
        }
    }

    /// sieve_orig (oracle) と外部一致: 1 shard 同士で SIEVE 意味論が完全一致。
    /// j8 の per_shard <= 64 制約に合わせて cap=64 でテストする。
    #[test]
    fn matches_sieve_orig_externally_1shard() {
        use crate::sieve_orig::SieveCache as Orig;
        let cap = 64usize;
        let mut a: Orig<u64, u64> = Orig::new(cap);
        let mut b: SieveCache<u64, u64, 1> = SieveCache::new(cap);
        for k in 0..10_000u64 {
            let key = (k.wrapping_mul(2654435761)) % 256;
            let _ = a.insert(key, key);
            let _ = b.insert(key, key);
        }
        for k in 0..256u64 {
            assert_eq!(
                a.get(&k).copied(),
                b.get(&k).copied(),
                "1-shard で sieve_orig と j8 が key {k} で食い違う"
            );
        }
    }

    /// bit layout の排他性: LIVE / VISITED / ID_MASK / HASH_MASK で u16 を埋め尽くす。
    /// Inner の associated const は `K, V` 依存 — bench / oracle で主流の
    /// `Entry<u64, u64>` (sizeof=16, ID_SHIFT=4) を代表ケースで検証。
    #[test]
    fn bit_layout_exclusivity_u64_u64() {
        type I = Inner<u64, u64>;
        // sizeof(Entry<u64, u64>) = 16 → ID_SHIFT = 4。
        assert_eq!(I::ID_SHIFT, 4);
        // ID_MASK = 0x03f0 (bits 4..9)、HASH_MASK = 0x3c0f (bits 0..3 + 10..13)。
        assert_eq!(I::ID_MASK, 0x03f0);
        assert_eq!(I::HASH_MASK, 0x3c0f);
        assert_eq!(I::SCAN_MASK, LIVE | I::HASH_MASK);
        assert_eq!(I::SCAN_MASK, 0xbc0f);

        // 排他性: 16 bit を漏れなく重複なく分割。
        assert_eq!(LIVE | VISITED | I::ID_MASK | I::HASH_MASK, 0xFFFF);
        assert_eq!(LIVE & VISITED, 0);
        assert_eq!(LIVE & I::ID_MASK, 0);
        assert_eq!(LIVE & I::HASH_MASK, 0);
        assert_eq!(VISITED & I::ID_MASK, 0);
        assert_eq!(VISITED & I::HASH_MASK, 0);
        assert_eq!(I::ID_MASK & I::HASH_MASK, 0);

        // id (= MAX_PER_SHARD - 1 = 63) を ID_SHIFT 桁シフトすると ID_MASK 全ビット。
        assert_eq!((MAX_PER_SHARD - 1) as u16, I::ID_MASK >> I::ID_SHIFT);

        // §8.3(a) のキー不変条件:
        //   tag に id を埋めると `tag & ID_MASK = id × sizeof(Entry)` (= byte offset)。
        let entry_size = std::mem::size_of::<Entry<u64, u64>>();
        for id in 0..MAX_PER_SHARD {
            let tag_id_field = (id as u16) << I::ID_SHIFT;
            assert_eq!(
                (tag_id_field & I::ID_MASK) as usize,
                id * entry_size,
                "id={id}: tag & ID_MASK が byte offset と一致しない"
            );
        }

        // needle_from_hash で生成される値は LIVE bit が立っており
        // SCAN_MASK 適用後も保存される (visited / id は元から 0)。
        let needle = Inner::<u64, u64>::needle_from_hash(0xABCD_EF01_2345_6789u64);
        assert_eq!(needle & LIVE, LIVE);
        assert_eq!(needle & I::SCAN_MASK, needle);
        assert_eq!(needle & VISITED, 0);
        assert_eq!(needle & I::ID_MASK, 0);
    }

    /// 8-bit hash → tag bit への spread が「単射」であること
    /// (= 異なる hash 値が異なる needle になる)。
    #[test]
    fn needle_spread_is_injective_u64_u64() {
        let mut seen = std::collections::HashSet::new();
        for h in 0..=255u64 {
            let needle = Inner::<u64, u64>::needle_from_hash(h << 56);
            assert!(seen.insert(needle), "hash={h} で衝突: needle={needle:#x}");
        }
        assert_eq!(seen.len(), 256);
    }

    /// ID_SHIFT が異なるサイズ (Entry<i32, i32> = 8 byte) でも spread が単射。
    #[test]
    fn needle_spread_is_injective_i32_i32() {
        // sizeof(Entry<i32, i32>) = 8 → ID_SHIFT = 3。
        assert_eq!(Inner::<i32, i32>::ID_SHIFT, 3);
        let mut seen = std::collections::HashSet::new();
        for h in 0..=255u64 {
            let needle = Inner::<i32, i32>::needle_from_hash(h << 56);
            assert!(seen.insert(needle));
        }
        assert_eq!(seen.len(), 256);
    }

    /// warm-up→steady の遷移: 5 個目の insert で初 evict、freed_id が再利用される。
    #[test]
    fn warm_up_to_steady_transition() {
        let mut cache: SieveCache<u64, u64, 1> = SieveCache::new(4);
        // warm-up: 4 個目までは evict なし、id = len で連続割り当て。
        assert_eq!(cache.insert(1, 100), None);
        assert_eq!(cache.insert(2, 200), None);
        assert_eq!(cache.insert(3, 300), None);
        assert_eq!(cache.insert(4, 400), None);
        assert_eq!(cache.len(), 4);
        // 5 個目で evict 発火。len は 4 のまま (evict 後に新規 fill)。
        let evicted = cache.insert(5, 500);
        assert!(evicted.is_some(), "5 個目で evict が走るはず");
        assert_eq!(cache.len(), 4);
        // 5 (新挿入) は hit、evict された key は miss。
        assert_eq!(cache.get(&5), Some(&500));
    }

    /// compact 前後で id ↔ entries の対応が壊れないこと。
    /// tags が order_cap (= 16 here) に到達するまで挿入を繰り返し、
    /// compact 発火後も既存 key の get が正しく値を返すことを確認。
    #[test]
    fn compact_preserves_id_mapping() {
        // 1 shard、cap=4 → order_cap = max(16, 8 LANE round) = 16。
        // tags が 16 埋まると compact 発火。
        let mut cache: SieveCache<u64, u64, 1> = SieveCache::new(4);
        // 16 個分の tail 消費を起こすには、4 個 warm-up + 12 個 evict-and-fill。
        // fill 時に tail が増え続け、tail==16 で compact 発火。
        for k in 0..40u64 {
            cache.insert(k, k * 13);
        }
        // 直近の cap 個は in-cache のはず (実際の生存者は SIEVE 動作次第)。
        let alive: u64 = (0..40u64)
            .filter(|&k| cache.get(&k) == Some(&(k * 13)))
            .count() as u64;
        assert_eq!(alive, 4, "compact 後も live entry の値が正しく取れる");
    }

    #[test]
    fn drop_runs_for_live_entries_only() {
        let mut cache: SieveCache<u64, String> = SieveCache::new(TEST_SHARDS * 2);
        for k in 0..64u64 {
            cache.insert(k, format!("value-{k}"));
        }
        assert_eq!(cache.len(), TEST_SHARDS * 2);
    }
}
