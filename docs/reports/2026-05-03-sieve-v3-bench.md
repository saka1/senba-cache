# sieve_v3 (bit-parallel + Option 剥がし + 2-pass evict) ベンチマーク結果 (2026-05-03)

- 日付: 2026-05-03
- きっかけ: v1 (bit-parallel scan) と v2 (`Vec<Option<EntryId>>` → `Vec<EntryId>`) の
  改善を合流させた v3 を作る。さらに v1 の `evict_one` が `[hand,tail) → [0,hand)` を
  最大 4 周していた構造を 2 周に圧縮できるはずなので、それも同時に入れる。
- ステータス: 完了。v3 を実装、oracle 一致を確認、bench 再走。

## v3 で入れた変更 (v1 起点)

1. **`order` の `Option` 剥がし** (v2 と同じ): `Vec<Option<EntryId>>` → `Vec<EntryId>`。
   tombstone bitmap が「dead か live か」の情報を持っているので `Option` ラッパは冗長。
2. **`evict_one` を最大 2 パスに圧縮**: v1 では「`!combined` (visited も tombstone も 0)
   を探す 2 パス + visited を全クリアし終わった後にもう 2 パス回って `!tombstone` の最初を
   拾う」という 4 パス構造だった。これを `find_victim_in_range` が **同一スイープ中に**:
   - `!combined` の最初の qpos (=即 victim)
   - `!tombstone` の最初の qpos (= visited 全クリア後の victim 候補)
   両方を記録するように拡張し、3+4 パス目を畳んで最大 2 パスで終わらせる。

## 正しさ

`tests/oracle.rs` に v3 の差分テストを追加 (v1 のミラー):

- `v3_diverges_when_victim_is_newest_entry` — v0/v1/v2 と同じ minimal repro で
  oracle (orig) との既知差分を踏むことを確認
- `v3_matches_v1_on_synthetic_zipf` — Zipf 4 条件 (skew × cap) で v1 と完全一致
- `v3_matches_orig_on_synthetic_zipf` — orig 起点の差分は minimal repro と同じ
  ものだけで、Zipf workload では一致
- `v3_matches_orig_on_bundled_zipf` — `external/NSDI24-SIEVE/mydata/zipf/zipf_1.0` の
  100k req で 3 cap (256/1024/4096) すべて一致

全 15 件の oracle テスト + 70 件のユニットテストが green。

## 測定環境

`docs/reports/2026-05-03-realistic-workload-bench.md` と同じ:

- CPU: Intel (WSL2 上の Linux カーネル 6.6)
- `cargo bench --bench micro` (criterion, `release`, `debug = "line-tables-only"`)
- `sample_size=20`, `warm_up=500ms`, `measurement_time=3s`
- workload: `insert_only` グループ、Zipf trace 1,000,000 req
- skew ∈ {0.6, 0.8, 1.0, 1.2}, cap ∈ {100, 1000, 10000} (footprint=100k の 0.1%/1%/10%)

## 結果サマリ

`mean_ms` = 1M req のトレース 1 回。比は v3 vs 各実装。

| skew | cap   | orig  | v0    | v1    | v2    | v3    | v3/orig | v3/v1 | v3/v2 |
|-----:|------:|------:|------:|------:|------:|------:|--------:|------:|------:|
| 0.60 |   100 | 39.06 | 45.32 | 48.01 | 41.86 | 47.57 |  1.218x | 0.991 | 1.136 |
| 0.60 |  1000 | 35.97 | 40.75 | 42.52 | 38.24 | 40.64 |  1.130x | 0.956 | 1.063 |
| 0.60 | 10000 | 35.41 | 38.61 | 42.55 | 38.56 | 41.42 |  1.170x | 0.973 | 1.074 |
| 0.80 |   100 | 37.75 | 41.70 | 41.41 | 39.95 | 42.85 |  1.135x | 1.035 | 1.073 |
| 0.80 |  1000 | 32.27 | 36.94 | 38.65 | 35.26 | 38.69 |  1.199x | 1.001 | 1.097 |
| 0.80 | 10000 | 29.26 | 34.94 | 34.81 | 33.90 | 35.76 |  1.222x | 1.027 | 1.055 |
| 1.00 |   100 | 33.63 | 37.49 | 38.93 | 34.98 | 39.04 |  1.161x | 1.003 | 1.116 |
| 1.00 |  1000 | 24.93 | 28.41 | 29.77 | 28.10 | 29.36 |  1.177x | 0.986 | 1.045 |
| 1.00 | 10000 | 20.70 | 24.22 | 24.81 | 23.45 | 23.50 |  1.135x | 0.947 | 1.002 |
| 1.20 |   100 | 22.23 | 25.75 | 25.62 | 25.34 | 26.15 |  1.176x | 1.021 | 1.032 |
| 1.20 |  1000 | 16.58 | 18.42 | 18.87 | 18.04 | 18.67 |  1.126x | 0.989 | 1.035 |
| 1.20 | 10000 | 14.82 | 16.52 | 15.86 | 16.22 | 16.43 |  1.109x | 1.036 | 1.014 |

(時間は `target/criterion/insert_only/<impl>_skew<α>/<cap>/new/estimates.json` の
`mean.point_estimate` を ms 換算)

## 読み

**v3 は v1 とほぼ tie、v2 にはむしろ微負け、orig には全条件で 1.11–1.22x 負け。**
期待していた「2-pass 化 + Option 剥がしで v1 から目に見える改善」は得られなかった。

### なぜ 2-pass 化が効かなかったか

v1 の 4 パス構造で実際に 3, 4 パス目まで到達するのは「[hand, tail) と [0, hand) の両方
で `!combined` の slot がひとつも見つからない = 全 live slot が visited」のときだけ。
Zipf の steady state では hand から数 slot で `!combined` が見つかるケースが多く、
1 パス目で完了する eviction が支配的。`first_live` を追跡するための分岐が増えた分、
fast-path で僅かに損している可能性がある。

### なぜ bit-parallel が効いていないように見えるか

v3 (bit-parallel) は v2 (素直な linear scan + Option 剥がし) より 0–14% 遅い。
bit-parallel が効くのは「visited が密に連続した帯を walk する」状況だが、Zipf では
hand から数 slot 進むだけで victim にぶつかるので、word ロード 2 本 (visited + tombstone)
+ マスク計算のオーバーヘッドが、線形 byte scan の単純さに負けている。

### Option 剥がしの寄与

v0 → v2 で 0.97–1.07x、v1 → v3 でも同様の傾向。Option 剥がし単体は微増程度の効きしかなく、
劇的な改善ではない。

### orig (linked list) が依然最速の理由

array-based queue scan は本質的に「hand を進めて非 tombstone slot を探す」コストを払う。
linked list (orig) は victim 探索が真に O(1) (hand pointer の next を辿るだけ)、
tombstone のような副次的な dead slot 概念もない。トレースが long で eviction 比率が
高い `insert_only` ベンチでは、このギャップがそのまま現れる。

## 次に試すなら

`evict_one` の中の細かい最適化はサチっている感がある。筋が良さそうなのは:

- (a) hand を「次の live qpos」へジャンプするスキップリスト的な構造を持つ。
  bit-parallel scan の word ロードを skip-table 1 lookup に置き換えるイメージ。
- (b) `visited` / `tombstone` を 1 本の bitmap (`alive_unvisited`) に統合する。
  word ロードが 2 本 → 1 本になり、`!combined` の代わりに直接 `alive_unvisited` の
  trailing_zeros を取れる。tombstone を別途持たないので compaction のトリガを変える必要あり。
- (c) そもそも array-based の路線を諦めて、orig ベースの最適化 (arena chunk size,
  prev/next の `Option` 剥がし、頻出パスの inlining) に振る。
