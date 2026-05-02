# `sieve_v0` が `sieve_orig` と挙動分岐する条件の発見レポート

- 日付: 2026-05-03
- きっかけ: ベンチマーク・ハーネス基盤 (spec: `docs/superpowers/specs/2026-05-03-bench-harness-design.md`) を構築し、`sieve_orig` を oracle とした差分テストを初めて流したところ、複数の入力で `sieve_v0` の evict 列が `sieve_orig` と一致しないことを確認。
- ステータス: ハーネスは完成。`sieve_v0` 側の修正は本レポート時点で未着手 (次セッション持ち越し)。

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

## 仮説

未検証のメモ。次セッションで詰める。

1. **compaction 時の hand 位置保存**
   - `sieve_v0::compact()` は `old_hand = self.hand.min(old_tail)` で現 hand 位置を確定し、走査中に `old_pos >= old_hand` を満たす最初の生存 entry を新 hand とする (`sieve_v0.rs:204-243`)。
   - `sieve_orig` の hand は entry への直接ポインタなので、compaction 概念がない。両者の semantics 対応付けが正しいか要確認。
   - 特に `old_hand` が tombstone を踏んでいたとき (= 圧縮で消える entry を指していたとき) の「次の生存 entry を新 hand にする」処理が、orig の `Sieve_remove_obj` 相当の「hand を obj.prev に逃がす」セマンティクスと一致しているか。
2. **wrap-around の方向**
   - `sieve_orig` は **tail → head** (古い → 新しい) に hand を進める。
   - `sieve_v0` は logical queue で `hand` を `+1` していくので、これは「古い → 新しい」(配列先頭=古い、tail=新しい) の方向と一致しているはず。要確認。
3. **eviction 時の hand 移動**
   - `sieve_orig::evict_one` (sieve_orig.rs:187-210) は victim 確定後に `self.hand = node.prev` を保存してから unlink。
   - `sieve_v0::evict_one` (sieve_v0.rs:157-196) は victim を確定したら `self.hand += 1` して、tombstone を立てて return する。
   - 両者の「次回 hand の起点」が、同じ論理位置を指しているか要再検算。

## 次セッションの作業候補

優先順:

1. **最小再現の生成**
   - 合成 Zipf 5644 req は長すぎる。`tests/oracle.rs` のロジックで「不一致になる最短プレフィクス」を二分探索するヘルパを追加し、必要 req 数を 1〜数百行に切り詰める。
   - キー数も `keys=10000` でなく数十まで縮められれば、トレースを test source に直書きできる。
2. **`sieve_v0` の修正方針決定**
   - compaction を伴わない単純シナリオで orig と一致するなら、compaction が原因。一致しないなら eviction 本体が原因。
   - 上記の最小再現を `cap=2`〜`cap=4` レベルに絞り込めれば、紙の上で hand の動きを追って原因を確定できる。
3. **修正後、 `tests/oracle.rs` を緑にする**
   - 緑になれば `sieve_v0` は `sieve_orig` と挙動同値。性能比較が初めて意味を持つ。
4. **(任意) get/insert 混在シナリオの oracle テスト追加**
   - 今回は insert-only。get で visited bit を立てる経路の差は未検証。

## ハーネス側の確認結果 (参考)

| 項目 | コマンド | 結果 |
|---|---|---|
| 既存単体テスト | `cargo test --lib` | 37 件 pass (BitSet 8, sieve_orig 12, sieve_v0 14, workload::zipf 3) |
| 差分ハーネス | `cargo test --test oracle` | 2 件 fail (= 本レポート対象) |
| Criterion | `cargo bench --no-run` | コンパイル pass |
| CLI | `cargo build --release --bin bench` | OK |
| `cargo check` | | clean |
| `cargo clippy` | | sieve_v0 既存 2 件のみ (`manual_div_ceil`, `len_without_is_empty`) |

依存追加: `rand = "0.9"`, `rand_distr = "0.5"`, `criterion = "0.5"` (dev)。

ハーネスは「次の variant を入れたら oracle が即走る」状態。`sieve_v0` 修正後にこのレポートを close できる。
