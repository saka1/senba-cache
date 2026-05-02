# `sieve_v0` が `sieve_orig` と挙動分岐する条件の発見レポート

- 日付: 2026-05-03
- きっかけ: ベンチマーク・ハーネス基盤 (spec: `docs/superpowers/specs/2026-05-03-bench-harness-design.md`) を構築し、`sieve_orig` を oracle とした差分テストを初めて流したところ、複数の入力で `sieve_v0` の evict 列が `sieve_orig` と一致しないことを確認。
- ステータス (2026-05-03 追記): 原因究明完了。最小再現 7 req を `tests/oracle.rs::v0_diverges_when_victim_is_newest_entry` に追加し、`sieve_v0.rs:189` に 1 行のラップ条件を足す対症修正で oracle 全 3 件 + 既存単体 37 件が green になることを確認。以下 [原因](#原因-2026-05-03-追記) 節を参照。設計面の再考 (= "order_cap = cap*2 でコンパクションをサボる" 方針自体の是非) は別論として残置。

## 原因 (2026-05-03 追記)

### 一行サマリ

**`sieve_v0::evict_one` は victim が "当時の最新エントリ" (= `qpos == tail - 1`) だったとき、次回 hand の起点として `tail` を保存する。続く `insert` が `qpos = tail` に新エントリを置くため、hand が新規挿入したエントリ自身を指してしまい、次の eviction で即 victim にされる。**

`sieve_orig` ではこの場合 `hand = victim.prev = None` になり、次回は **tail (最古)** から再開する。両者は完全に逆方向の victim を選ぶ。

### 最小再現

cap=3、トレース `[1, 2, 3, 1, 2, 4, 5]` の 6 ステップ目 (= `insert(5)`) で発散する。`tests/oracle.rs::v0_diverges_when_victim_is_newest_entry` 参照。

| step | req | sieve_orig | sieve_v0 |
|---:|---|---|---|
| 0 | insert 1 | None | None |
| 1 | insert 2 | None | None |
| 2 | insert 3 | None | None |
| 3 | insert 1 (re-insert) | None (visited=1) | None (visited=1) |
| 4 | insert 2 (re-insert) | None (visited=1) | None (visited=1) |
| 5 | insert 4 | evict 3 | evict 3 |
| 6 | insert 5 | **evict 1** | **evict 4** ← 分岐 |

step 5 直後の状態:

- orig: list (oldest→newest) = `[1(visited=0), 2(visited=0), 4(visited=0)]`, `hand = None` (= 直前 victim 3 が head だったため `victim.prev = None`)
- v0: `order = [1, 2, _, 4, _, _]` (`_` = tombstone), `tail = 4`, **`hand = 3`** (= 直前 victim 3 の `qpos + 1`)
- ここで v0 の `order[3] = 4`、つまり **hand は新規挿入された "4" の位置を指している**

step 6 の evict:

- orig: `hand = None` → `cur = tail = 1`, `1.freq = 0`, victim = 1。
- v0: `hand = 3` → `pos = 3`, `tombstone[3] = false`, `visited[3] = false`, **victim = 4 (= 直前に挿入されたばかりのエントリ)**。

### なぜこのバグが unit test を素通りしたか — order_cap = cap * 2 とコンパクションの "救済" 効果

これはユーザーが指摘した「v0 はコンパクションを見越してキャパを大きく取っている部分があり、そこで *ズル* をしている可能性」の正体でもある。

`evict_one` 直後に `maybe_compact` が走り、その閾値は `dead >= len || tail == order.len()`。コンパクションは hand 位置を再計算する:

```rust
// sieve_v0.rs:204-243 抜粋
let old_hand = self.hand.min(old_tail);
let mut new_hand: Option<usize> = None;
for old_pos in 0..old_tail {
    if self.tombstone.get(old_pos) { continue; }
    if new_hand.is_none() && old_pos >= old_hand {
        new_hand = Some(write);
    }
    ...
}
self.hand = if self.len == 0 { 0 } else { new_hand.unwrap_or(0) };
```

ここで **問題のシナリオ (victim が tail-1) では `old_hand == old_tail` になるので、ループ中に `old_pos >= old_hand` を満たす live エントリは存在せず、`new_hand` は `None` のまま。結果として `hand = 0` にリセット** される。これは偶然 orig の「hand=None → tail から再開」と等価な挙動になる。

つまり **コンパクションがバグを覆い隠している**:

| cap | dead が len に追いつく速度 | バグ顕在化の条件 |
|---|---|---|
| 2 | 毎回の eviction (1 dead, 1 live) でただちに compact | 顕在化しない |
| 3 | 2 回目の eviction で compact | 1 回目の eviction が "victim=tail-1" シナリオ かつ その次の eviction で発覚 |
| ≥ 4 | dead が len/2 を越えるまで蓄積 | 高頻度で顕在化 |

`order_cap = capacity * 2` という設計は「tombstone を貯める余裕を作る」目的だが、**結果として `dead >= len` 経由のコンパクションを cap 中盤以降で頻発させ、本バグを隠蔽する側に働いていた**。`sieve_v0` の単体テスト 14 件はすべて cap ≤ 3 か、または "再 insert を伴わない churn テスト" (= visited bit が立たないので hand が tail-1 まで進まない) なので、バグ条件を一切踏まない。

これが「単体テスト 18 件 pass なのに oracle で落ちる」の正体。

### 対症修正 (適用済み)

最小修正は `evict_one` の victim 確定ブロック末尾で、`hand += 1` のあとに wrap を一段強める:

```rust
// sieve_v0.rs:189
self.hand += 1;
if self.hand >= self.tail {
    self.hand = 0;
}
```

これで「victim が tail-1 だった」場合に hand=0 が次回 evict 開始位置として保存され、以降の `insert` が tail を伸ばしても hand は新規エントリを指さない。orig の `hand = None → tail` セマンティクスと等価になる。

検証結果 (この 1 行だけを足した状態):

| 項目 | 結果 |
|---|---|
| `cargo test --lib` | 37 件 pass |
| `cargo test --test oracle` | 3 件 pass (`v0_diverges_when_victim_is_newest_entry`、合成 Zipf 4 (skew,cap), bundled Zipf 3 cap) |

### 残タスク / 設計上の論点

1. **対症修正で済ませてよいか**: 本修正は orig の hand semantics に v0 を寄せる最小パッチ。設計レビュー観点では `order_cap = cap * 2` (= tombstone を貯めて periodic compact する方針) 自体が "compaction が暗黙に hand 不変条件を補正する" という不安定な前提に依存しており、その前提が今回崩れた。本修正でテストは緑になるが、`order_cap` を `cap` ぴったりにする / tombstone をやめて linked list に戻す / SIEVE の hand を別表現にする、等の選択肢を比較する余地は残る。
2. **get/insert 混在シナリオ**: 今回の oracle テストは `insert` のみ。`get` 経路の visited bit 更新は未テスト。`get` で同じバグが新たに表面化するかは要追加検証。
3. **CLI 集計値の再測定**: 旧レポートの C 章 (cap=256 で v0 のほうが hit が +2,038) は本バグ起因の "見かけ上の成績" 。修正後に再計測すると orig と完全一致するはず。性能 (`elapsed_ns`) の劣化傾向は別議論。

## TL;DR

- `sieve_v0` (tombstone + 周期 compaction) は `sieve_orig` (NSDI'24 著者参照ポート) と **eviction 経路で異なる挙動**を取る。
- 発現条件は **eviction が活発に起こる cap × ワークロード**。容量が大きすぎて eviction が起きないケースでは観測できない。
- 単体テスト (`src/sieve_v0.rs::tests`) は全 18 件 pass。**単体テストでは捉えられないバグ** であり、差分ハーネスを入れた成果がそのまま現れた。
- 修正方針はまだ立てていない (本レポートはあくまで「再現条件と症状」の記録)。

## 観測

### A. 合成 Zipf

`tests/oracle.rs::v0_matches_orig_on_synthetic_zipf` (commit 時点) で、

```rust
ZipfGen::new(skew=1.05, n_keys=10_000, seed=42).take(200_000)
cap = 64
```

を流すと **index 5644** で初めて発散する。

```
orig[5644] = Some((320, 320))   // orig は 320 を evict
v0  [5644] = Some((234, 234))   // v0 は 234 を evict
```

直前の context (index 5641〜5643) までは **両者とも同じキーを evict** しており、5644 で枝分かれする:

```
orig[5641..5648]  = [None, Some((2412,2412)), None, Some((320,320)), Some((227,227)), Some((234,234)), None]
other[5641..5648] = [None, Some((2412,2412)), None, Some((234,234)), Some((301,301)), Some((155,155)), None]
                                                ^^^^ 5644: ここから別物
```

つまり「ある時点までは hand の進み方も freq の落とし方も同じ → 突然 victim 選択が違う」。`sieve_v0` の compaction が hand 位置を保存する処理 (`compact()` 内) と、tombstone を踏んだ際のスキップ処理が疑わしい。

`(skew, cap)` 4 通り (`(1.05, 64), (1.1, 128), (1.2, 256), (1.5, 1024)`) を試し、**全件で発散** した。skew が高い (= ホット集中が強い) と evict 頻度は減るが、200k req もあれば確実に発散点に到達する。

### B. 同梱トレース `external/NSDI24-SIEVE/mydata/zipf/zipf_1.0`

`tests/oracle.rs::v0_matches_orig_on_bundled_zipf` で `cap=256, len=100_000` を流すと、

```
[bundled cap=256] first divergence at index 12128
  orig[12128] = Some((759, 759))
  other[12128] = Some((664, 664))
```

`cap=1024` / `cap=4096` でも oracle テスト上は発散するが、後述の CLI スモークでは eviction 自体ほぼ起きない領域 (キー空間 ≪ cap) なので発現しない。oracle テストでは `take(100_000)` で済ませているが、それでも cap=1024 以上で divergence が出るかは未確認 (今回は最初の不一致報告で panic するため)。

### C. CLI ベンチによる集計値の差

`cargo run --release --bin bench -- --source file --path external/NSDI24-SIEVE/mydata/zipf/zipf_1.0 --capacity 256,1024,4096 --variant orig,v0`

の 1M req 全体集計:

| variant | cap | hits | misses | evictions | elapsed_ns |
|---|---:|---:|---:|---:|---:|
| orig | 256 | 800,585 | 199,415 | 199,159 | 18,626,943 |
| v0   | 256 | **802,623** | **197,377** | **197,121** | 18,770,364 |
| orig | 1024 | 999,000 | 1,000 | 0 | 7,936,824 |
| v0   | 1024 | 999,000 | 1,000 | 0 | 9,279,478 |
| orig | 4096 | 999,000 | 1,000 | 0 | 8,528,344 |
| v0   | 4096 | 999,000 | 1,000 | 0 | 9,915,925 |

ポイント:

- `cap=256` (eviction 活発) のとき **v0 のほうが hit が +2,038 多い**。`evictions` も -2,038 少ない。
- v0 が「うまく」キーを残しているように見えるが、これは **正しさが破れている** だけで quality として優れているわけではない (oracle と挙動が違う以上、SIEVE 仕様にも著者参照実装にも合致していない)。
- `cap >= 1024` ではキー空間 (≒1,000) を超えるため eviction 自体ほぼ起きず、orig/v0 の差は出ない。
- 性能 (`elapsed_ns`) は v0 のほうが遅い傾向 (cap=1024 で +17%, cap=4096 で +16%)。eviction が無い workload でこの差。これは別議論。

## 再現手順

```bash
# 差分テスト (panic で divergence を吐く)
cargo test --test oracle

# CLI で集計値の差を観察
cargo run --release --bin bench -- \
    --source file --path external/NSDI24-SIEVE/mydata/zipf/zipf_1.0 \
    --capacity 256 --variant orig,v0

# 合成 Zipf でも同様
cargo run --release --bin bench -- \
    --source zipf --skew 1.05 --keys 10000 --len 200000 --seed 42 \
    --capacity 64 --variant orig,v0
```

`tests/oracle.rs::assert_eviction_streams_eq` は最初の divergence index と前後 3 要素を panic message に乗せる設計なので、ログがそのまま **最小化の取っ掛かり** になる。

## 仮説 (旧、いずれも 2026-05-03 検証で結論済み)

1. **compaction 時の hand 位置保存** — **半分当たり**。compaction の `new_hand = None → 0` フォールバックは、実は orig の `hand=None → tail` セマンティクスと等価。問題はコンパクションが走らない時、hand が `tail` のまま放置され、次の `insert` が `qpos=tail` に新エントリを置くことで hand が新エントリを指すこと。
2. **wrap-around の方向** — 当たり。両者とも oldest → newest で同方向。
3. **eviction 時の hand 移動** — 当たり。`hand += 1` 後に「tail を超えていたら 0 にラップ」の処理が抜けていた (= 上の 1 と同じ問題の別表現)。

## 次セッションの作業候補 (2026-05-03 改訂)

1. **対症修正のレビュー** ([原因](#原因-2026-05-03-追記) 節の 1 行) — 既に oracle 全 3 件 + 単体 37 件 green。レビュー後に commit するか、より大きな設計変更で巻き取るか判断。
2. **設計再考** — `order_cap = cap * 2` + tombstone + periodic compaction 方針自体を維持するか。「コンパクションが偶発的に hand 不変条件を補正する」という暗黙依存を断ち切れるなら、別 variant (`sieve_v1`) として別実装したほうが綺麗な可能性。
3. **get/insert 混在シナリオの oracle テスト追加** — 今回の oracle は `insert` のみ。`get` 経路に同種のバグが潜んでいないか別途検証。
4. **CLI 集計値の再測定** — 修正後、cap=256 で v0 と orig が完全一致するか確認し、本レポート C 章の数値が消えること (バグ起因の見かけ上の hit 増しだったこと) を裏付ける。

## ハーネス側の確認結果 (参考)

| 項目 | コマンド | 結果 |
|---|---|---|
| 既存単体テスト | `cargo test --lib` | 37 件 pass (BitSet 8, sieve_orig 12, sieve_v0 14, workload::zipf 3) |
| 差分ハーネス (修正前) | `cargo test --test oracle` | 2 件 fail (= 本レポート対象) |
| 差分ハーネス (1 行修正後) | `cargo test --test oracle` | 3 件 pass (最小再現テスト追加込み) |
| Criterion | `cargo bench --no-run` | コンパイル pass |
| CLI | `cargo build --release --bin bench` | OK |
| `cargo check` | | clean |
| `cargo clippy` | | sieve_v0 既存 2 件のみ (`manual_div_ceil`, `len_without_is_empty`) |

依存追加: `rand = "0.9"`, `rand_distr = "0.5"`, `criterion = "0.5"` (dev)。

ハーネスは「次の variant を入れたら oracle が即走る」状態。`sieve_v0` の対症修正は本セッションで適用済み (sieve_v0.rs:189 の wrap 1 行) — 設計再考まで含めるかは次セッションに持ち越し。
