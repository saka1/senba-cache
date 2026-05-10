# c13s/c14s/c15s/c16s: Path A の store-back を CAS 化して shift race を解消

**日付:** 2026-05-10
**スコープ:** `research/src/experimental/sieve_c13s.rs`, `sieve_c14s.rs`, `sieve_c15s.rs`, `sieve_c16s.rs`
**結果:** 4 variant 共通の `concurrent_invariants_under_zipf` flake (= live_ids() 内 id 重複) を排除。debug build 100×4 = 400 試行で fail 0、修正前は c13s 2/60、c14s 1/60、c16s 5/134 で再現していた。

## 1. 症状

```
thread 'experimental::sieve_c16s::tests::concurrent_invariants_under_zipf' panicked
  at research/src/experimental/sieve_c16s.rs:1313:13:
  assertion `left == right` failed: shard X で id 重複
```

`live_ids()` は `tags[0..len]` の LIVE tag から `id_of(t)` を集めるヘルパー。
重複が出るのは 2 つの LIVE tag が同 `entries[id]` を指すこと。

debug build の reproduction rate (200 試行ベース、--release では 0/50):
| variant | 修正前 | 修正後 |
|---|---|---|
| c13s | 2/60 | 0/100 |
| c14s | 1/60 | 0/100 |
| c15s | 0/60 (※) | 0/100 |
| c16s | 5/134 | 0/200 |

(※ c15s は構造的に同一バグだが、この施行回数では fail しなかった。修正は予防的に同形展開。)

## 2. 根本原因

### 2.1 関係するコード

Path A (lock-free key-existing update, c12s 由来):

1. `find_lockfree` で pos と現 tag T_a (id I_a) を取得
2. `tags[pos].compare_exchange(T_a, EMPTY, Acquire, Acquire)` で slot 所有権獲得
3. `entries[I_a]` を読み出して新値を書き戻し
4. `tags[pos].store(T_a ^ VERSION, Release)` で復帰 ← **ここが問題**
5. `visited[pos]` を SET

Path C (writer Mutex 配下、`writer_evict_and_install` 内 shift loop):

```rust
for i in evict_pos..(cap - 1) {
    let next_tag = self.read_live_tag_with_spin(i + 1);  // tags[i+1] を spin-read
    // visited 移動
    self.tags[i].store(EMPTY, Ordering::Release);
    fence(Ordering::Release);
    self.tags[i].store(next_tag, Ordering::Release);     // tags[i] = old tags[i+1]
}
```

`read_live_tag_with_spin(i+1)` は EMPTY を見ると spin する (Path A 進行中 = EMPTY を吸収するため)。一方 `tags[i].store(EMPTY)` → `store(next_tag)` の 2 段書きは **無条件**。

### 2.2 race shape

問題は **Path A on `pos_a` (evict_pos < pos_a < cap-1)** と **Path C iter `pos_a-1` / iter `pos_a`** の交錯。

```
Path C iter pos_a-1: read tags[pos_a] = T_a (LIVE, before Path A)
Path C iter pos_a-1: store tags[pos_a-1] = T_a              <- id I_a at pos_a-1
Path A:              CAS tags[pos_a]: T_a → EMPTY (success)
Path A:              modify entries[I_a]
Path C iter pos_a:   read tags[pos_a+1] = T_(a+1) (no spin, pos_a+1 untouched)
Path C iter pos_a:   store tags[pos_a] = EMPTY (overwrite Path A's EMPTY, no-op)
Path C iter pos_a:   store tags[pos_a] = T_(a+1)            <- id I_(a+1) at pos_a
                                          ↑
                                          ここまでで id 重複なし
Path A (遅れた):    store tags[pos_a] = T_a ^ VERSION       <- id I_a at pos_a を上書き
                                                            ↑
                                                            これが Path C を負かす
```

最終状態: `tags[pos_a-1]` も `tags[pos_a]` も id `I_a`。`live_ids()` で重複が出る。

要は **Path A が "EMPTY → 元 tag (VERSION 反転)" を `store` で書き戻すが、間に Path C が同 slot を別 id で上書き済み**でも気づかない。

### 2.3 なぜ c11s/c12s では起きないか

- c11s に Path A は無い (writer は常に Mutex)。
- c12s には Path A があるが、設計上 **id == pos 不変式** を保つので shift しない (eviction は `entries[evict_pos]` を直接書き換え、id は固定)。`tags[pos]` の id 部位は変わらない。よって "shift で id が動く" race shape が成立しない。
- c13s 以降は senba::Cache lineage の **shift-on-evict** を採用したため、id が pos と分離して動くようになった。Path A の store-back との conflict が発生する余地ができた。

## 3. 修正

`tags[pos].store(new_tag, Release)` を `tags[pos].compare_exchange(EMPTY, new_tag, Release, Acquire)` に変更し、CAS 失敗時は何もしない (visited fetch_or もスキップ)。

```rust
let new_tag = expected_tag ^ VERSION;
let cas_back = self.tags[pos].compare_exchange(
    EMPTY, new_tag, Ordering::Release, Ordering::Acquire,
);
if cas_back.is_ok() {
    let mask = Self::vbit_mask(pos);  // c13s/c14s/c15s は (w, b) = vbit(pos)
    self.hot.visited.fetch_or(mask, Ordering::Relaxed);
}
// CAS 失敗: Path C shift が slot を奪った。entries[id] への更新は shift 後の
// tags[pos-k] 経由で参照される (Path C は entries[evict_id] のみ書き換えるので
// entries[id] は無傷)。何もしない。
```

### 3.1 CAS 失敗時 entries[id] が誰に見えるか

- Path A は `entries[I_a]` を `(old.key, new value)` に更新 (key 部位は preserve)。
- Path C は **`entries[evict_id]` だけ** 書き換え、shift は **tags のみ** 動かす。
- `evict_id != I_a` (pos_a > evict_pos なので別 id)。よって `entries[I_a]` は Path C に触れられない。
- shift 後、id `I_a` を持つ tag は `tags[pos_a - 1]` (= 1 つ前にズレた位置) で見つかる。lookup of K は `tags[pos_a-1]` (id I_a) を hash 照合 → key 一致 → Path A の new value を返す。

要は **entries[id] 上の更新は失われず、tag 表示位置だけが shift で 1 つ前にズレる**。Path A の semantics (= 既存 key の value 更新) は保たれる。

### 3.2 visited を立てない理由

`pos` は今や別 entry を指している (Path C iter pos_a が別 id を書き戻した)。
そこに `visited[pos]` を立てると別 entry が visited と誤認される。
`I_a` 自身の "visited" は shift 後の正しい位置 (`pos - 1`) で表現されるべきだが、
そこは Path C iter pos_a-1 が `s_mask = vbit(pos_a)` から `d_mask = vbit(pos_a-1)`
への移動として既に処理済み (= shift 直前の `visited[pos_a]` の状態を引き継ぐ)。
ここで Path A が立て直しても sweep で 1 周分損する程度の semantic ノイズ
(UB ではない) だが、不正確な visited は将来の解析を狂わせるので CAS 失敗時は省略。

### 3.3 別 race との関係

- **Path A vs Path A on 同 key:** 既に `compare_exchange T → EMPTY` で 1 つだけ勝つ。CAS 失敗時 → escalate to path_bc。本修正と無関係。
- **Path A vs writer_update_in_place (writer Mutex 配下):** writer_find が EMPTY を spin 待ちするので writer 視点では Path A 完了後に動く。ただし writer_find が事前に snapshot した expected_tag を使った update の場合、entries[id] に対する 2 重書きが発生しうる (= 別 race shape、id 重複は引き起こさない)。本修正は touch せず。
- **Path C vs Path C:** writer Mutex で serialized。

## 4. 検証

### 4.1 soak (debug)
| variant | 修正前 (試行回) | 修正後 |
|---|---|---|
| c13s | 2 / 60 | **0 / 100** |
| c14s | 1 / 60 | **0 / 100** |
| c15s | 0 / 60 | **0 / 100** |
| c16s | 5 / 134, 3 / 80 (別 run) | **0 / 200** |

### 4.2 quality gates
```
cargo fmt --all                                       # clean
cargo clippy --workspace --all-targets -- -D warnings # clean
cargo test --workspace                                # 388/388 pass + 101 (senba) + doc
```

### 4.3 perf gate
本修正は senba (publishable) には触っていないので `sieve_cache_perf.rs` は対象外。
4 variant とも experimental 配下、micro.rs での comparative bench のみ影響を受けるが、
今回は flake fix なので perf 比較は別 session で必要なら測る。

CAS は uncontended path (Path A 成功後の store-back) 1 回追加なので overhead はほぼ無視可能 (CAS 1 段、cache line は同 line)。

## 5. 影響と未対応事項

- **採用済 variant** (c16s) のみ load-bearing。c13s/c14s/c15s は研究系列の途中変種で日常使用は無いが、同根バグなので予防的に修正。
- **c12s に同種 race は無い** (id == pos invariant)。c11s も Path A 無しで対象外。
- **別 race (entries[id] への 2 重書き = Path A vs writer_update_in_place)** は別件として残る。本テストは id 重複しか検査しないので捕えられない。今回 scope 外。
- 修正後の semantics: Path A succeed 報告後でも、tag 反映位置は shift で 1 つ前にズレる可能性がある (= 同 entry が違う pos に見える)。lookup は問題なし、観察可能な不変式は維持される。

## 6. 次の手

- micro.rs comparative bench で Mops 影響を確認 (期待: ほぼ変化なし、CAS 1 段追加のみ)。
- Path A vs writer_update_in_place の 2 重書き race を別途調べる (concurrent_invariants_under_zipf では検出されない、別 test が要る)。
