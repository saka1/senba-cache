# 2026-05-06 — `senba::SieveCache` ライブラリ化設計

- 種別: **設計ドキュメント** (実装着手前のスペック、ベンチなし)
- 親: `2026-05-05-sieve-j8-design.md`、`2026-05-06-j8-c-hoist.md`、`2026-05-06-j8-twitter-pareto.md`、`2026-05-06-j8-vs-mini-moka-twitter.md`
- 関連: `improvement-ideas.md` §M2.3, §M5.3

## 1. 位置付け

これまでの `sieve_*` 系列はすべて research artifact として位置付けられてきた。
ベンチで「j8 が moka 0.12 / mini-moka 0.10 を 12〜16× 上回りつつ HR でも勝つ」
(`2026-05-06-j8-vs-mini-moka-twitter.md`)、「c8 が並列で moka を 15× 上回る」
(`2026-05-06-c8-vs-moka-thread-sweep.md`) が示せた段階で、SIEVE の実用利得は
研究上の問いから「ライブラリにして配るべきか」のフェーズに移った。

本設計は **publishable な Rust crate API** としての ST 版 `senba::SieveCache` を
確定する。並行版 (= c8 系の後継) と `Cache` trait 再設計は本スペックの scope 外、
別スペックで「新 `SieveCache` の上に再構築」として独立に扱う。

## 2. TL;DR

- 新公開型: `senba::SieveCache<K, V, S: SlotSize = Slot32, const SHARDS: usize = 8>`
- `SlotSize` は **sealed trait**、impl は `Slot16` / `Slot32` (default) / `Slot64` の 3 ブラケット
- entries arena の **stride を `S::SIZE` 固定** (= 自動 padding)。`Entry<K, V>` の
  sizeof が SLOT::SIZE 以下なら任意。違反は const-eval panic で次のブラケット案内
- j8 の throughput 利得 (c-hoist / byte-offset trick) は維持。`tag & ID_MASK = id × S::SIZE`
  の不変条件は SLOT 単位で同型に成立
- 公開 API: `new`, `get`, `insert`, `remove` (slow-path), `contains_key`, `len`, `is_empty`, `capacity`
- `remove` は per-shard rebuild (= swap-to-fill-gap、O(per_shard) ≤ O(64)) で
  warm-up 不変条件 I8 を回復させ、free_list を持たない構造を維持
- 既存 `src/sieve_j8.rs` は research artifact として残置 (過去 bench との比較ベース)
- 並行版・`generic_const_exprs` 化・crate rename (`senba_cache` → `senba`) は別スペック

## 3. 動機

### 3.1 j8 の構造的制約

j8 は `assert!(sizeof(Entry).is_power_of_two() && sizeof <= 256)` を const eval で
要求する (`src/sieve_j8.rs:113-119`)。これは c-hoist (`docs/reports/2026-05-06-j8-c-hoist.md`)
で示した **`tag & ID_MASK = id × sizeof(Entry)`** という byte-offset trick — inner SIMD
ループから `shl` を消して 1 cy / candidate 削減 — を成立させるための制約。

実用上の障害:

- `Entry<String, String>` = 48 byte → 2 冪でない、コンパイル panic
- `Entry<i32, &str>` = 24 byte → 同上
- 「string-cache としての利用」が事実上できない (既知の bench 系列で string ワークロードが
  立ち上げられていないのはこれが原因)

### 3.2 「実用化」の目標

publishable crate として配る。比較対象は orig ではなく `moka` / `mini-moka` で、
そちらに対しては既に 10× オーダーの throughput アドバンテージがある (`c8-vs-moka-thread-sweep.md`)。
つまり c-hoist trick で稼いだ 1 ns 単位の細工を「あえて捨てる」必要はなく、**j8 fast-path
は保ったまま padding 自動化を被せる**のが正解。

### 3.3 padding 機構の API 制約

利用者に「`Entry<K, V>` を 2 冪に padding せよ」と書かせるのは内部実装の漏れ。逆に
「ライブラリが内部で padding する」を完全自動化するには `size_of::<Entry<K, V>>()` を
struct field の配列長に流す必要があり、これは stable Rust では `generic_const_exprs`
(nightly) なしには直接書けない。

stable で取れる現実解として **「const generic / sealed trait でブラケットを露出、
default で 99% カバー、はみ出すケースだけ explicit を要求」** を採用する (本スレッド議論で
合意済)。`SlotSize` を sealed trait にすると arbitrary 値 (`SLOT=33` など) を書かれず、
将来 `Slot::Auto` 追加で in-place 移行できる利点がある。

## 4. 設計

### 4.1 モジュール構成

新規 `src/sieve_cache.rs` を作る。`lib.rs` から re-export して以下を crate top に出す:

```rust
// src/lib.rs (追加部分)
pub mod sieve_cache;
pub use sieve_cache::{SieveCache, SlotSize, Slot16, Slot32, Slot64};
```

既存 `sieve_orig`, `sieve_v0`〜`v3`, `sieve_j3`〜`j8`, `sieve_c8` は手付かず。
利用者は `senba_cache::SieveCache` を import する (crate を `senba` に rename した場合は
`senba::SieveCache`、本スペック scope 外)。

### 4.2 `SlotSize` sealed trait

```rust
mod sealed { pub trait Sealed {} }

pub trait SlotSize: sealed::Sealed + 'static {
    /// このブラケットの slot stride (byte)。常に 2 の冪。
    const SIZE: usize;
    /// ブラケットごとの記憶セル型。`size_of::<Storage<E>>() == SIZE` を保つように
    /// 各 impl で union を使って定義する (§4.3)。
    type Storage<E>: Sized;
}

pub struct Slot16;
pub struct Slot32;
pub struct Slot64;

impl sealed::Sealed for Slot16 {}
impl sealed::Sealed for Slot32 {}
impl sealed::Sealed for Slot64 {}

impl SlotSize for Slot16 { const SIZE: usize = 16; type Storage<E> = Slot16Storage<E>; }
impl SlotSize for Slot32 { const SIZE: usize = 32; type Storage<E> = Slot32Storage<E>; }
impl SlotSize for Slot64 { const SIZE: usize = 64; type Storage<E> = Slot64Storage<E>; }
```

`Slot16/32/64` は ZST (`struct Slot16;`)。型レベル札としてのみ使う。
GAT (`type Storage<E>`) は stable since 1.65。

### 4.3 `SlotStorage` union

各ブラケットごとに `[u64; N]` で padding した union を定義する。
union の sizeof は variant の最大値 (= max(sizeof(E), N×8))、alignment は max(align(E), 8)。

```rust
use std::mem::ManuallyDrop;

#[repr(C)]
pub union Slot16Storage<E> {
    entry: ManuallyDrop<E>,
    _pad: [u64; 2],   // 16 byte
}

#[repr(C)]
pub union Slot32Storage<E> {
    entry: ManuallyDrop<E>,
    _pad: [u64; 4],   // 32 byte
}

#[repr(C)]
pub union Slot64Storage<E> {
    entry: ManuallyDrop<E>,
    _pad: [u64; 8],   // 64 byte
}
```

`ManuallyDrop<E>` は union のメンバになれる (Rust の union 制約は drop semantics 由来)。
init / drop は `Inner` 側で `ptr::write` / `ptr::drop_in_place` 経由で管理する。

#### サイズ / alignment 不変条件

実装は次の 2 つを const assert で要求する:

```rust
impl<K, V, S: SlotSize> Inner<K, V, S> {
    const _SIZE_OK: () = assert!(
        std::mem::size_of::<Entry<K, V>>() <= S::SIZE,
        "sieve_cache: sizeof(Entry<K, V>) exceeds the chosen SlotSize. \
         Try a larger SlotSize (e.g. Slot64)."
    );
    const _STORAGE_SIZE_OK: () = assert!(
        std::mem::size_of::<S::Storage<Entry<K, V>>>() == S::SIZE,
        "sieve_cache: SlotStorage size differs from SLOT::SIZE. \
         (likely caused by Entry alignment > 8 byte)"
    );
}
```

2 つ目の assert は **alignment edge case** をガードする: `Entry` が `repr(align(16))`
以上の高アライン要求を持つ場合、union align は max(align(E), 8) = align(E) になり、
union の sizeof が SLOT::SIZE を超えて切り上げられる可能性がある。これが起きると
c-hoist 不変条件 (`tag & ID_MASK = id × S::SIZE`) が破綻するので必ず compile-fail させる。
通常の K, V (整数 / String / Vec / Box / Arc) ではすべて align ≤ 8 なので問題は出ない。

### 4.4 `SieveCache` 公開型

```rust
pub struct SieveCache<K, V, S: SlotSize = Slot32, const SHARDS: usize = 8> {
    shards: [Inner<K, V, S>; SHARDS],
    hasher: Xxh3Build,
}

impl<K, V, S, const SHARDS: usize> SieveCache<K, V, S, SHARDS>
where K: Hash + Eq, S: SlotSize
{
    pub fn new(capacity: usize) -> Self;
    pub fn get(&mut self, key: &K) -> Option<&V>;
    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)>;
    pub fn remove(&mut self, key: &K) -> Option<V>;
    pub fn contains_key(&self, key: &K) -> bool;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn capacity(&self) -> usize;
}
```

#### 典型的な利用コード

```rust
use senba_cache::SieveCache;

// 99% の利用者: SLOT を意識しない
let mut c: SieveCache<u64, String> = SieveCache::new(1024);
//        ^^^^^^^^^^^^^^^^^^^^^^^ Entry sizeof = 32, default Slot32 で exact fit
c.insert(1, "hello".into());
assert_eq!(c.get(&1), Some(&"hello".into()));
c.remove(&1);
```

`(String, String)` のように Entry > 32 を扱う場合だけ:

```rust
use senba_cache::{SieveCache, Slot64};

let mut c: SieveCache<String, String, Slot64> = SieveCache::new(1024);
// Slot32 のままだとコンパイル時に "exceeds the chosen SlotSize" panic
```

### 4.5 内部 `Inner<K, V, S>`

```rust
struct Entry<K, V> { key: K, value: V }

struct Inner<K, V, S: SlotSize> {
    capacity: usize,
    tags: Vec<u16>,                                  // size = order_cap (2 × cap、LANE 揃え)
    entries: Vec<MaybeUninit<S::Storage<Entry<K, V>>>>,  // size = capacity
    tail: usize,
    hand: usize,
    len: usize,
}
```

j8 の `Inner<K, V>` を `S` で parametrize しただけ。論理は j8 と同型。

### 4.6 不変条件

j8 と同じ I1〜I9 が成立 (`src/sieve_j8.rs` の冒頭 doc comment 参照)。読み替え:

- I6: I5 集合の id についてのみ `entries[id]` の **`entry` フィールド** が init 済み
  (union メンバとしての init / uninit を `Inner` 側で管理する)

### 4.7 ID_SHIFT / マスクの SLOT 一般化

j8 で `sizeof(Entry)` から導いていた定数群を `S::SIZE` から導くだけで同型に動く:

```rust
impl<K, V, S: SlotSize> Inner<K, V, S> {
    const ID_SHIFT: u32 = (S::SIZE as u32).trailing_zeros();
    const ID_MASK: u16 = ((MAX_PER_SHARD - 1) as u16) << Self::ID_SHIFT;
    const HASH_MASK: u16 = 0x3FFF & !Self::ID_MASK;
    const SCAN_MASK: u16 = LIVE | Self::HASH_MASK;
}
```

ブラケットごとの bit 配置 (`MAX_PER_SHARD = 64`):

| SLOT | ID_SHIFT | ID_MASK  | HASH_MASK | SCAN_MASK | hash bit 数 |
|------|----------|----------|-----------|-----------|-------------|
| 16   | 4        | `0x03f0` | `0x3c0f`  | `0xbc0f`  | 8           |
| 32   | 5        | `0x07e0` | `0x381f`  | `0xb81f`  | 8           |
| 64   | 6        | `0x0fc0` | `0x303f`  | `0xb03f`  | 8           |

3 ブラケット全て hash 8 bit を確保 (false-match 率 1/256 が j8 と同じ)。

### 4.8 `needle_from_hash` の SLOT 一般化

j8 の hash spread (低位 ID_SHIFT bit + 高位 (8 − ID_SHIFT) bit を id 領域を跨いで配置) を
`Self::ID_SHIFT` で書き直すだけ。j8 で既に一般化済 (`src/sieve_j8.rs:222-239`) なので
そのまま流用できる。

## 5. 操作

### 5.1 `get` / `insert`

j8 と同じ。byte-offset trick `tag & ID_MASK = id × S::SIZE` (= entries arena 内の
byte offset) は SLOT 単位で同型に成立、c-hoist で稼いだ inner ループ短縮はそのまま。

implementation 注意点:
- `S::Storage<Entry>` は union なので、init / read は `(*entries[id].as_mut_ptr()).entry`
  経由で `ManuallyDrop` メンバを操作する
- `MaybeUninit::write` は使えない (内部の union は別)、`ptr::write` で entry フィールドに
  直接書く

### 5.2 `remove` (新規 slow-path)

```rust
pub fn remove(&mut self, key: &K) -> Option<V> {
    let h = self.hasher.hash_one(key);
    let i = Self::shard_of_hash(h);
    self.shards[i].remove(key, h)
}

// Inner 内
fn remove(&mut self, key: &K, hash: u64) -> Option<V> {
    let needle = Self::needle_from_hash(hash);
    let pos = self.find(key, needle)?;
    let removed_id = Self::id_of(self.tags[pos]);

    // (1) entries[removed_id] から Entry を取り出して破壊、tag を EMPTY に
    let entry = unsafe {
        let p = self.entries[removed_id].as_mut_ptr();
        ManuallyDrop::take(&mut (*p).entry)
    };
    self.tags[pos] = EMPTY;
    self.len -= 1;

    // (2) I8 (live ids = 0..len) を回復: max_id (= self.len、self.len-=1 後は最大 live id)
    //     を removed_id に swap で詰める。max_id を指していた tag の id field を更新。
    let max_id = self.len;
    if removed_id < max_id {
        // max_id を指す live tag を線形探索
        for i in 0..self.tail {
            let t = self.tags[i];
            if t & LIVE != 0 && Self::id_of(t) == max_id {
                unsafe {
                    let src = self.entries[max_id].as_ptr();
                    let dst = self.entries[removed_id].as_mut_ptr();
                    std::ptr::copy_nonoverlapping(src, dst, 1);
                }
                let cleared = t & !Self::ID_MASK;
                let new_id_field = (removed_id as u16) << Self::ID_SHIFT;
                self.tags[i] = cleared | new_id_field;
                break;
            }
        }
    }

    Some(entry.value)
}
```

#### 計算量と前提

- find: O(per_shard / LANE) SIMD = O(per_shard) 上限
- 第 (2) 段: tags の線形探索 O(per_shard)
- 合計: O(per_shard) ≤ O(64)、絶対値で 100〜300 ns 見込み (slow-path として許容)

#### `remove` 後の不変条件

- I8 (warm-up 不変条件: live ids = 0..len) が swap-to-fill-gap で**回復する**
- → 次の `insert` 時 `entry_id = self.len` がそのまま機能
- → free_list を生やす必要がない (j8 の構造を維持)

#### hand カーソル

- `pos < hand`: hand は不変 (削除位置は既に通過済み)
- `pos >= hand`: hand は不変 (次回 scan で `tags[pos] == EMPTY` を skip する既存ロジックで処理)
- 既存 j8 の `scan_evict` / `first_live` が EMPTY を skip する作りなので、追加処理不要

### 5.3 公開 API その他

- `contains_key`: `find` の結果を bool 化 (visited 立てない)
- `len` / `is_empty` / `capacity`: 全 shard 合算 (j8 と同じ)

### 5.4 公開しない (本スペック scope 外)

- `iter()` / `keys()` / `values()`: 後続スペックで設計
- `clear()`: 後続
- `Cache` trait impl: placeholder の `src/cache.rs` は触らない
- `Drop` カスタマイズ以外の thread-safety: ST のみ

## 6. アルゴリズム整合性

### 6.1 SIEVE 意味論

j8 と完全同型 (storage 型を変えただけ、tag bit 配分も SLOT に応じてシフトするだけで
SIEVE のロジック ─ visited 巡回・hand 進行・evict 判定 ─ には影響しない)。

`remove` を含めた挙動は **`sieve_orig` の `remove` API がある** (CLAUDE.md 確認済) ので
oracle として使える。

### 6.2 oracle テスト戦略

`sieve_orig` (1 shard) と新 `SieveCache::<K, V, S, 1>` で:

1. ランダム insert / get / remove sequence を 100k step 流し、各 step の戻り値が一致
2. `evict` 列が完全一致 (j7→j8 の確認手順を流用)
3. 各ブラケット (Slot16 / 32 / 64) で同じ trace を実行

`Entry<u64, u64>` (sizeof=16) は 3 ブラケット全部で動作可能なので、Slot16 / 32 / 64 の
3 通り test を回して相互一致を確認。

### 6.3 compile-fail テスト

`trybuild` クレートを使い:

- `Entry<String, String>` (=48) を `Slot32` で使う → expected error
- `Entry<u64, [u8; 256]>` (=264) を `Slot64` で使う → expected error
- `MAX_PER_SHARD` (64) を超える per_shard → expected error

## 7. メモリ予算

K=u64, V=u64 (Entry sizeof=16) を Default Slot32 で使う場合:

- entries: 32 B × cap = 32 B/cap (j8 fast-path 16 → 32、+16 B/cap)
- tags: 4 B/cap (j8 と同じ)
- 合計 36 B/cap

| K, V | sizeof(Entry) | SLOT | entries B/cap | tags B/cap | 合計 | vs orig (25) |
|------|--------------:|------|--------------:|-----------:|-----:|-------------:|
| (u64, u64) | 16 | Slot16 | 16 | 4 | **20** | −20% |
| (u64, u64) | 16 | Slot32 (default) | 32 | 4 | **36** | +44% |
| (u64, String) | 32 | Slot32 | 32 | 4 | **36** | +44% |
| (String, u64) | 32 | Slot32 | 32 | 4 | **36** | +44% |
| (String, String) | 48 | Slot64 | 64 | 4 | **68** | +172% |
| (Arc<str>, Arc<str>) | 32 | Slot32 | 32 | 4 | **36** | +44% |

#### consciously accepted cost

j8 の「memory も throughput も orig 超え」の宣伝はこの構造では弱まる。代わりに得るのは
**「padding 自動化と remove API、利用者は SLOT を 99% 意識しない」** のライブラリ性。
小さい K, V で memory の絶対競争したい利用者は `SieveCache::<K, V, Slot16>` を explicit に
書くことで j8 の元利得を取り戻せる (`(u64, u64)` で 20 B/cap)。

#### slot ごとの sweet spot

- **Slot16**: small primitive type cache、メモリ tight、`(u32, u32)` `(u64, u64)`
- **Slot32 (default)**: `(String, V_small)` `(K_small, String)` `(Arc<str>, Arc<str>)` 等の
  string-cache 主流ケース
- **Slot64**: `(String, String)` `(K, V_struct_up_to_56B)` の重量ケース

## 8. 性能の机上検討

### 8.1 throughput

c-hoist trick が SLOT::SIZE 単位で同型成立するので j8 と同等を見込む (`+0 ns ± 1 ns`)。

inline 物理サイズが大きくなる (16→32 B/entry) と L1d footprint は増えるが、
per_shard ≤ 64 の前提で:
- Slot32: 64 entries × 32 B = 2 KB/shard、L1d (32 KB) に収まる
- Slot64: 64 × 64 = 4 KB/shard、同じく収まる

実測想定は cluster018 sweep (`2026-05-06-j8-twitter-pareto.md` と同条件) で確認:
- `(u64, u64) + Slot32`: j8 比 +0〜+2 ns (entries が j8 16B → 32B 倍増、prefetch 1 line 余分)
- `(u64, String) + Slot32`: j8 では動かないので比較不能、orig との直接比較

### 8.2 remove のコスト

slow-path 想定で 100〜300 ns / call 程度。`per_shard=32` の線形 scan が支配的成分。
ベンチで頻度別の影響を測る必要があるが、本スペックでは「remove は throughput-critical
ではない」前提で許容。

### 8.3 `(String, String)` が default (Slot32) で compile-fail する件

これは「**最も典型的な string-cache 利用が `Slot64` 明示を要求する**」ことを意味する。
利用者体験としてはやや悪い (典型ケースで設定が必要)。

代替案として **default を `Slot64` にする**ことで `(String, String)` も無設定で動かせるが、
小さい K, V (`u64, u64`) で 4× memory waste になり j8 系列の memory 利得ストーリーが
完全に死ぬ。

S2 で `Slot32` default を選んだ理由 (D1):「common cases で 0% waste」「(String, String)
利用者だけ 1 行書く」を踏襲する。エラーメッセージを「`Slot64` を試せ」と具体的に書くことで
利用者の戸惑いを最小化する (§4.3 の const assert メッセージ)。

## 9. 制約とリスク

### 9.1 構造的制約

| 制約 | 内容 | 緩和策 |
|---|---|---|
| `MAX_PER_SHARD ≤ 64` | 6-bit ID、j8 から継承 | `Inner::new` で `assert!`、bench 帯と整合 |
| `sizeof(Entry) ≤ S::SIZE` | const assert で要求 | エラーメッセージで次のブラケット案内 |
| `align(Entry) ≤ 8` (実質) | union align overflow を防ぐため | §4.3 の `_STORAGE_SIZE_OK` assert で検出 |
| `S::SIZE` は 2 冪 | trait の sealed 性で保証 | sealed なので利用者は破れない |

### 9.2 alignment edge case

`Entry<K, V>` の alignment が 8 を超える (例: `K` が `repr(align(16))` を持つ) と、
union の sizeof が SLOT::SIZE を超えて切り上げられる可能性がある。c-hoist 不変条件
が破綻するので **`_STORAGE_SIZE_OK` assert で必ず compile-fail させる**。

通常の K, V (整数 / String / Vec / Box / Arc / 標準ライブラリ型) は align ≤ 8 なので
ヒットしない。SIMD 型を value にする独自 struct で問題が出る程度の稀ケース。

### 9.3 throughput 退行リスク

- entries B/entry が j8 16 → Slot32 32 で 2× → L1d 1 line/entry → 2 line/entry へ
  (cache line=64 B 前提だと entry あたり line 数は 0.25 → 0.5)
- prefetch 1 line 余分が hit path で +0.5 ns 程度の悪化候補

実測で +3 ns 以上ズレた場合の原因:
- entries access pattern の散在 (SLOT 大型化で L1d miss 増)
- `Inner` 1 個の cache footprint が L1d 1/8 を超える (Slot64 + per_shard=64 で 4 KB)

### 9.4 `(String, String)` 利用者の追加負担

§8.3 参照。default `Slot32` で compile-fail することで、利用者は `Slot64` を 1 つ
import + 型に書く必要がある。エラーメッセージでガイドする以外の緩和策はなし。

## 10. テスト計画

### 10.1 j8 テストの移植

`src/sieve_j8.rs` の test を `src/sieve_cache.rs` 内に移植:

- `cache_initially_empty`, `insert_then_get`, `get_missing_returns_none`,
  `contains_key_reflects_insertions`, `insert_existing_key_updates_value`,
  `evicts_oldest_when_full_and_unvisited`, `visited_entry_survives_first_pass`,
  `all_visited_clears_bits_then_evicts`, `total_capacity_is_respected_under_churn`,
  `churn_keeps_a_full_capacity_set`
- `bit_layout_exclusivity` を 3 ブラケット (Slot16/32/64) で確認

### 10.2 新規テスト

| 名前 | 内容 |
|---|---|
| `slot16_small_entry` | `Entry<u32, u32>` (=8) を Slot16 で動作確認 |
| `slot32_default_string_value` | `Entry<u64, String>` (=32) を default Slot32 で |
| `slot64_string_string` | `Entry<String, String>` (=48) を Slot64 で |
| `compile_fail_string_string_default` | `trybuild` で Slot32 + (String, String) 拒否 |
| `compile_fail_oversize_value` | `trybuild` で Slot64 + Entry > 64 拒否 |
| `compile_fail_high_align_entry` | `trybuild` で `repr(align(16))` 型を拒否 |
| `remove_basic` | insert→remove→get で None |
| `remove_then_insert_reuses_id` | remove 後の insert で I8 が回復していること (内部状態 introspection 経由) |
| `remove_during_churn_oracle_match` | insert/get/remove ランダム sequence を `sieve_orig` と外部一致 |
| `matches_sieve_orig_externally_per_slot` | 3 ブラケット × `sieve_orig` 1-shard 一致 |

### 10.3 ベンチ

`benches/micro.rs` または新規 `benches/sieve_cache.rs` で:

- Twitter cluster018/034 × cap ∈ {1024, 4096, 16384} × per_shard ∈ {16, 32}
- variant: `orig` / `sieve_j8` (= research baseline) / `SieveCache<u64, u64, Slot32>`
- string ワークロード: `(String, String)` で `SieveCache<_, _, Slot64>` を `moka` / `mini-moka` と AB

## 11. 実装計画

incremental、各段階で `cargo test` 全 green を確認:

| 段階 | 内容 | ゲート |
|---|---|---|
| 0 | `sealed` mod + `SlotSize` trait + `Slot16/32/64` ZST + `Slot*Storage` union 定義のみ | `cargo check` 通過 |
| 1 | `Inner<K, V, S>` を sieve_j8 から書き起こし、entries 型を `S::Storage<Entry>` 化 | sieve_orig oracle (j8 既存) 通過 |
| 2 | const assert (`_SIZE_OK`, `_STORAGE_SIZE_OK`) 配置 | trybuild compile-fail テスト通過 |
| 3 | `SieveCache` 公開型 + 既存 j8 テスト移植 | 全テスト green |
| 4 | `remove` slow-path 実装 (swap-to-fill-gap) | remove 単体テスト + oracle 一致 |
| 5 | bench harness 追加 (`benches/sieve_cache.rs` または既存 micro 追加) | j8 比 throughput ±2 ns |

scope-out (本スペックでは触らない):

- 並行版 (= c8 系の後継、別スペック)
- `generic_const_exprs` 移行 (in-place 化、stabilize 後に minor bump)
- crate rename `senba_cache` → `senba` (別 PR)
- `Cache` trait 整合化 (placeholder のまま、別 issue)
- `iter()`, `clear()`, `entry()` 系 API (後続スペック)

## 12. オープン課題

| # | 課題 | 優先度 |
|---|---|---|
| OQ1 | crate rename `senba_cache` → `senba` のタイミング (本実装完了後の独立 PR) | 中 |
| OQ2 | `Cache` trait (placeholder) と新 `SieveCache` の整合化 | 低 (別 issue) |
| OQ3 | `Slot128` 追加要否 (現状 YAGNI、利用者要望次第) | 低 |
| OQ4 | `clear()`, `iter()`, `entry()` API 露出 | 中 |
| OQ5 | `generic_const_exprs` 安定後の auto-default 化 (in-place、major bump 不要) | 低 (待機) |
| OQ6 | 並行版 (`SyncSieveCache`?) を新 `SieveCache` の上に再構築 | 高 (本実装後の最優先) |
| OQ7 | `Drop` 実装の union メンバ手動 drop の正確性検証 (Miri) | 中 |
| OQ8 | non-AVX2 環境 (ARM 等) の SIMD path フォールバック | 低 (j8 既に scalar fallback あり) |

## 13. 結論

本設計は j8 系列で確立した throughput / HR の競争力を保ったまま:

- **任意の K, V (sizeof ≤ 64) を扱える** padding 自動化
- **`remove` を持つ** ライブラリ的 API
- **sealed trait による安全な内部最適化露出** (将来 in-place migration 可能)

を実現する。`senba::SieveCache` として publishable な crate API の核を作る。

memory-fair 比較で orig を絶対勝ちする j8 の研究結論はそのまま残し (research artifact)、
ライブラリ向け実装はそこから 16 B/cap 程度の memory tax を払って padding 自動化と
利用者体験を取る、という trade-off を意識的に選んでいる。

実装は incremental (§11) で各段階 `cargo test` green を確認しながら進める想定。
ベンチは Twitter cluster018 で j8 比 throughput retention を確認、追加で string
ワークロード (`(String, String) + Slot64`) で moka / mini-moka との AB を取る。
