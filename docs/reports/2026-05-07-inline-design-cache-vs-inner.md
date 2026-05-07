# `senba::Cache` の `#[inline]` 設計 — wrapper / atom / helper の3層分け

## 背景

`src/sieve_cache/mod.rs` が 1347 行に肥大していたので、`Inner` (per-shard
SIEVE state、struct + 全 impl + `Drop` + `Clone`) を `inner.rs` に切り出した。
モジュール分割は純粋なコード再配置のはずで、生成コードに差は出ない**はず**だった。

ところが `benches/sieve_cache_perf.rs` を `before` baseline と比較すると、
3シナリオすべてで明確な regression:

| シナリオ | 切り出し直後 vs `before` |
|---|---|
| insert_u64/384 | +14% 〜 +18% 退行 |
| mixed_u64/384 | +6% 〜 +18% 退行 |
| insert_string/256 | +5% 〜 +6% 退行 |

切り出した `Inner::get`/`insert` 等の hot メソッドを `pub(crate)` にしてモジュール
境界をまたぐ呼び出しに変えた瞬間、コンパイラの inline 判断が変わったのが原因。

## 試行 1: hot path に `#[inline]` を撒く

最初の対処として、`Inner` の公開メソッド10個 (`contains`, `get`, `get_mut`,
`peek`, `peek_mut`, `get_key_value`, `peek_key_value`, `get_or_insert_with`,
`insert`, `remove`) すべてに `#[inline]` を付与した。

結果は perf-gate 内に戻ったが、ベンチ経路を冷静に見ると bench が叩くのは
`insert` (全シナリオ) と `get` (mixed_u64) の2つだけ。残り8個は実証されない
予防的 `#[inline]`。剥がして再測定すると:

| シナリオ | 全10個 inline → `insert`+`get` のみ inline |
|---|---|
| insert_u64 | -0.4% (ノイズ) |
| mixed_u64 | +0.85% (ノイズ) |
| insert_string | -2.2% (ノイズ) |

つまり残り8個は実測上ゼロ寄与で、cargo cult。

## 試行 2: そもそも `Inner::*` に inline を付けるのが筋なのか

ここで設計上の問いが浮かぶ:

```
Cache::get → Inner::get → needle_from_hash, find_scalar, id_of, entry_ptr ...
```

の3層のうち、**どこをアトム (= 1つの outline 関数のまま) にすべきか**。
全部 inline すると呼び出しサイトに `Inner::get` (~50行) が複製されてコード
肥大、何も inline しないと細切れの ABI 境界が最適化を妨げる。

標準ライブラリの `HashMap` は次のパターン:

- `HashMap::get` etc は `#[inline]` の thin forwarder
- 内部の `RawTable` 系は基本 non-inline (ここがアトム)
- アトムの内側にある小さな bit/index 操作 helper は `#[inline]`

これを `senba::Cache` に当てはめると:

| 層 | 役割 | `#[inline]` |
|---|---|---|
| `Cache::get` / `Cache::insert` | 3行の thin wrapper (hash + shard select + 委譲) | **付ける** — 呼び出しサイトに溶ける |
| `Inner::get` / `Inner::insert` | 1論理操作の本体、~50行 | **付けない** — outline したアトム |
| `Inner::find_scalar` / `needle_from_hash` / `id_of` / `entry_ptr` | 数行の helper | **付ける** — アトム内部に溶ける |

試行 1 では `Inner::get` 自体に `#[inline]` を付けてアトムを呼び出しサイトに
複製させていたので、コード肥大の方向に振っていた。逆向きにする:
`Cache::get`/`insert` に `#[inline]` を移し、`Inner::get`/`insert` の
`#[inline]` を削除。

## 結果

`before` (refactor 前 = `Inner` がまだ `mod.rs` にいた頃) 比:

| シナリオ | 試行 1 (`Inner::*` inline) | 試行 2 (`Cache::*` inline + `Inner::*` アトム) |
|---|---|---|
| insert_u64/384 | -0.4 〜 +2.0% (ノイズ) | -1.1 〜 -1.6% (ノイズ) |
| mixed_u64/384 | +0.5 〜 +2.4% (ノイズ) | -4.0% 改善 〜 +2.4% (run 振れあり) |
| insert_string/256 | -2.2 〜 -1.5% (ノイズ) | **-4.2 〜 -9.3% 改善** |

insert_string は2回の独立 run で一貫して -4% 〜 -9% の改善、insert_u64 は
ノイズ域、mixed_u64 は run によって ±3% の振れ。ノイズ床を考慮しても、
**試行 2 のレイアウトは少なくとも従来同等、insert_string では実測の改善**。

改善の方向に振れる物理的な理由(推定):

1. `Cache::get`/`insert` を inline すると `hash_one(key)` の計算が呼び出し
   サイトに展開され、key の生存期間や型情報を使った dead store 除去・畳み込みが
   効きやすくなる (特に String キーで顕著)。
2. `Inner::get`/`insert` を outline すると、本体全体が単一の関数として codegen
   され、register allocator がアトムスコープで一貫して走る。逆に `#[inline]`
   を付けると呼び出しサイトに複製され、各サイトでヒューリスティックが走り直すため
   微妙な差が出る。
3. icache: アトムが outline されていれば1コピーで済む。実アプリで `cache.get`
   が散在するときの効きが本質的に違う (bench は呼び出しサイト1箇所なので、
   ここの差は今回の数値には乗っていない)。

## 最終的な配置

```
Cache::get      #[inline]   ← thin wrapper
Cache::insert   #[inline]   ← thin wrapper
  ↓
Inner::get      (none)      ← アトム
Inner::insert   (none)      ← アトム
  ↓
Inner::find_scalar       #[inline]
Inner::needle_from_hash  #[inline]
Inner::id_of             #[inline]
Inner::entry_ptr         #[inline]
Inner::entry_ptr_mut     #[inline]
```

`Cache::get_mut` / `peek` / `peek_mut` / `get_key_value` / `peek_key_value` /
`get_or_insert_with` / `remove` / `contains` / `clear` / `retain` / `drain`
等は perf-gate の経路にないので、現状 `#[inline]` を付けていない。今後 bench
シナリオが追加されてホット化したら、その時点で thin wrapper 化 + `#[inline]`
を実測ベースで判断する (CLAUDE.md の "premature abstraction より3行重複" 哲学)。

## 一般原則 (持ち帰り)

- 公開API は `#[inline]` の thin wrapper にする
- その奥の worker をアトム (= `#[inline]` を付けない outline 関数) にする
- アトム内部の小さい helper には `#[inline]` を付けて、アトム本体に溶け込ませる
- モジュール分割や API 追加で perf 退行が出たら、まず「アトムが意図せず
  inline されてないか / 逆に thin wrapper の inline が取れてないか」を疑う。
  HashMap の linker symbol を眺めるのが参考になる
- 予防的に worker に `#[inline]` を撒くのはコード肥大方向への bias であり、
  実測がない限り筋が悪い

## 関連

- `docs/reports/2026-05-06-senba-sievecache-design.md` — `Cache` 公開 API 設計
- `docs/reports/2026-05-06-sieve-cache-shift-on-evict.md` — `Cache` 直近の perf 改善
- `benches/sieve_cache_perf.rs` — perf-gate (3 シナリオ、~10秒)
