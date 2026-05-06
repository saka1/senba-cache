# `senba::Cache` における compact 廃止と shift-on-evict 化

## TL;DR

`src/sieve_cache.rs` の `Inner` から **`compact` 経路と `tail` フィールドを撤去**し、
steady-state insert を **shift-on-evict** モデルに書き換えた。`tags` 配列のサイズが
`round_up(2 × capacity, LANE)` から `round_up(capacity, LANE)` に半減し、SIMD scan
窓もそれに合わせて縮小。perf-gate (`benches/sieve_cache_perf.rs`) は 3 シナリオ全てで
改善 (insert_u64 −2.9%, mixed_u64 −3.7%, insert_string −1.0%)。`sieve_orig` との
oracle 等価性 (eviction 列の byte-for-byte 一致) は保たれ、新たに strengthening した
`remove_during_churn_oracle_match` (毎 insert で eviction 戻り値を比較) も pass。

## 動機

`Inner::insert` 内の以下の分岐が、**ほぼあらゆる定常状態で必ず else 側を取る**にも
関わらずホットパスに居座っていた:

```rust
if self.tail == self.tags.len() {
    self.compact();
}
```

`compact` は j8 系列から継承した「append-at-tail で tail が `2 × capacity` まで成長
したら一気に live tag を前詰め」の機構。append-at-tail を採用しているため tail が
無制限に伸び、`compact` で帳尻を合わせる必要があった。

しかし**新エントリを evict 位置にそのまま書けば**、tail が伸びることもなく compact
も不要になるはず — というのが出発点。

## 試行 1: 単純な in-place reuse (失敗)

```rust
// pos = self.find_evict_pos();   // SIEVE scan で犠牲位置決定
// 1. entries[id_of(tags[pos])] を読み出して evicted を返す
// 2. tags[pos] に新エントリの tag を上書き、entries[id] に書き直し
// 3. self.hand = pos + 1 (wrap)
```

unit test と insert-only oracle test (`matches_sieve_orig_externally_1shard`) は通る
が、**`remove_during_churn_oracle_match` で発散** (step ~66 の insert で eviction 列
が `sieve_orig` と乖離)。

### 原因

cap=3、`tags=[A*, B, C*]` (`*` = visited) の状態で D を insert:

| | 配列状態 | 次サイクル順 (hand から) |
|---|---|---|
| 自モデル | `tags=[A, D, C*], hand=2` | C\*, A, D |
| sieve_orig | `list=[D, C*, A], hand=C*` | C\*, D, A |

サイクル順が違う (`A, D` vs `D, A`)。in-place reuse は新エントリを **元エントリの
位置**に置くため、フラット配列の「位置 → 挿入順序」対応が壊れる。`sieve_orig` の
リストでは新規は常に head、つまり「最も head 寄り」(= 配列なら `len-1`) でなければ
いけない。

`A` と `D` の visited bit がたまたま同じなら結果も一致するが、別だと次の eviction
選択が分岐し、それ以降の trace が累積的にずれていく。

## 試行 2: shift-on-evict (採用)

evict 時に **`tags[pos+1..len]` を1つ前にシフト**し、新 tag は **末尾 `tags[len-1]`**
に書く。これでフラット配列が常に `sieve_orig` の "tail (oldest) → head (newest)"
リスト順と対応し続ける:

```rust
let pos = self.find_evict_pos();
let id = id_of(self.tags[pos]) as u16;
let entry = ptr::read(self.entry_ptr(id));   // evict
let last = self.len - 1;
self.tags.copy_within(pos + 1..self.len, pos);  // shift
// hand = victim's prev: shift 後は pos に successor が居る。
// pos == last (head 削除) なら sieve_orig の prev=NIL → wrap to tail と等価。
self.hand = if pos < last { pos } else { 0 };
write_tag(last, id, hash);
ptr::write(self.entry_ptr_mut(id), Entry { key, value });
```

### 検証トレース (cap=3, `tags=[A*, B, C*]`, hand=0 → insert D)

1. SIEVE scan: `A*` visited → clear、`B` 未 visited → pos=1
2. evict B、`tags.copy_within(2..3, 1)` → `tags=[A, C*, _]`
3. write D at `tags[2]` → `tags=[A, C*, D]`
4. hand = pos = 1 (= C\* の新位置)
5. サイクル順 (hand=1 から): **C\*, D, A** — sieve_orig と一致 ✓

`remove` も同じ shift で SIEVE 順序を保つ (もともとそう書いていた)。entry-id の
swap-to-fill-gap は引き続き使い、I8 (live ids = `0..len`) を維持。

## 副次的に消えるもの

- `tail` フィールド (`Inner` から撤去、`len` 1本で表現)
- `compact` 関数まるごと
- `do_evict_returning_id` / `evict_one_returning_id` / `first_live` (`find_evict_pos`
  に統合・簡略化)
- `scan_evict` / `find_scalar` の `EMPTY` skip 分岐 (I4' 「`tags[0..len]` は常に
  LIVE」が成立するため)
- `tags` 配列の slack (`2 × capacity` → `capacity` に半減)

## トレードオフ: shift コスト

各 evict (= 各 steady-state insert) で `(len - 1 - pos) * 2` バイトの memmove が
発生する。capacity ≤ MAX_PER_SHARD = 64 なので最悪 126 バイト、平均 ~32 u16 = 64
バイト。これは L1 内の小サイズ memmove で、modern CPU では数サイクルに収まる。

最初は `for` ループで書いたところ **mixed_u64 が +4.8% リグレッション** (perf gate
の 5% 閾値ぎりぎり)。`Vec::copy_within` (= `ptr::copy` 経由 memmove) に置換すると
**−3.7% 改善**に転じた。コンパイラが for ループから memmove を導けず、bound check
込みの素朴ループになっていたものと推定。

## ベンチ結果 (`benches/sieve_cache_perf.rs`)

baseline (refactor 前) との比較:

| シナリオ | time 変化 | 評価 |
|---|---|---|
| `insert_u64/384` | −2.9% (CI [−4.2%, −1.7%]) | 改善 |
| `mixed_u64/384` (50% get / 50% insert) | −3.7% (CI [−4.8%, −2.5%]) | 改善 |
| `insert_string/256` (Slot64, String キー) | −1.0% (CI [−2.0%, −0.0%]) | 改善 |

3 シナリオ全て p < 0.05 で有意改善。SIMD scan 窓が半減した分の恩恵が、shift コスト
を上回っている。

## 不変量 (更新版)

- I4' (新規): `tags[0..len]` は全て LIVE (穴なし); `tags[len..]` は全て EMPTY
- I5: live tags が参照する entry_id は一意で個数 = `len`
- I6: I5 の id についてのみ `entries[id].entry` が initialized
- I7: I5 集合 ⊆ `0..capacity`
- I8: live id = `0..len` (warm-up と remove の swap-to-fill-gap で維持)

I4 から I4' への強化が今回の本質的な変更。

## oracle 等価性

`remove_during_churn_oracle_match` を strengthening し、毎 insert で eviction 戻り値
を `sieve_orig` と比較するようにした (旧版は最終 `get` 結果のみ比較)。3000 step ×
periodic remove で完全一致を確認 — `sieve_orig` の双方向リスト操作と byte-for-byte
等価。

## 残課題

- `MAX_PER_SHARD` を超えるキャパでの shift コスト未測定 (現状の 6-bit ID 制約で
  per_shard ≤ 64 なので問題にならないが、将来的にキャパ上限を緩める設計に進む場合は
  再検証要)。
- micro.rs での比較は未走 (この refactor は library `Cache` 専用で、experimental 系
  には影響なし)。
