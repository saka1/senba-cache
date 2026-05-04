# 2026-05-05 — `sieve_j6` (M2.1) Twitter trace 初期 AB

## 1. 目的

`docs/improvement-ideas.md` §M2.1 の単独実装。j5 の `Entry { key:u64,
value:u64, visited:bool }` (24 B + 7 B padding) から **`visited` を tag バイトの
bit6 に同居** させ、`Entry` を 16 B に圧縮する。memfair レポート
(`2026-05-05-j5-vs-orig-2x-memfair.md`) で確定した「j5 inline 34 B/cap vs
orig 25 B/cap」を、**inline footprint で orig と同等水準まで縮める** のが目標。

期待されていた副次効果 (improvement-ideas より):
1. メモリ -28% (Entry padding 消失で K=V=u64 で 24→16 B)
2. hit-path -1〜2 ns/op (visited RMW が tags 配列内 in-place、別 cache line 不要)

## 2. 実装

`src/sieve_j6.rs` (standalone、j3/j5 のコードに依存しない 1 ファイル完結)。

tag バイトのレイアウト:

| bit | 意味 |
|---|---|
| 7 | live (1=occupied、0=EMPTY/tombstone) |
| 6 | visited |
| 0..5 | hash (上位 6 bit) |

主な不変条件:
- `EMPTY = 0x00`、live tag は必ず bit7=1。
- 探索 needle は `LIVE | (hash >> 58 & 0x3F)` で visited=0、つまり `0x80..=0xBF` の範囲。
- `(tags[i] & 0xBF) == needle` で visited bit を無視して tag 比較。
- visited セット: `tags[i] |= 0x40` (tags 配列の同一 cache line 内 RMW)。
- visited クリア: `tags[i] &= !0x40` (evict scan 時)。

AVX2 経路は `vpand`(mask) → `vpcmpeqb`(cmp) → `vpmovmskb`(extract) で、j3/j5 比 +1 命令 (mask 1 個)。

外側 set-associative wrapper は j5 と同じ — 下位 log2(SHARDS) bit で shard 選択、
shard 内で 6-bit tag。tag bit が j5 (7 bit) より 1 本減るので false-match 率は
1/128 → 1/64 (内側 key 等価で必ず弾けるため correctness 影響なし)。

correctness:
- 16 unit tests (j3/j5 ミラー + j5 との外部一致テスト) all green。
- `matches_j5_externally`: cap=128, 8 shards, Zipf 風 trace で j5 と同じ key set / 同じ get 結果。
- Twitter cluster018 1M req で j5_n32 と j6_n32 の (hits, misses, evictions) が完全一致 (627574, 372426, 368330)。

## 3. ベンチ条件

- `scripts/sweep_j6_twitter.sh`: cluster018 × cap ∈ {1024, 4096, 16384} × per_shard ∈ {32, 64, 128} ×
  TRIALS=5、各 cell で `j5_n*` と `j6_n*` を 1 つずつ揃える。LEN=1M。
- ホスト: 既存 j5 sweep と同一 (差分は同 run 内で取れる、絶対値の rerun 比較ではない)。
- 出力: `profiles/j6_twitter_pareto_2026-05-05.csv`。

## 4. 結果

各 cell の中央値 (5 trials):

| cap | per_shard | shards | j5 ns/op | j6 ns/op | Δ (j6−j5) |
|---:|---:|---:|---:|---:|---:|
| 1024 | 32 | 32 | 31.67 | 34.40 | **+2.73** |
| 1024 | 64 | 16 | 35.88 | 41.41 | +5.53 |
| 1024 | 128 | 8 | 42.81 | 54.09 | +11.28 |
| 4096 | 32 | 128 | 31.53 | 34.66 | +3.13 |
| 4096 | 64 | 64 | 34.40 | 38.88 | +4.48 |
| 4096 | 128 | 32 | 40.99 | 50.61 | +9.62 |
| 16384 | 32 | 512 | 28.69 | 31.15 | +2.46 |
| 16384 | 64 | 256 | 31.92 | 35.36 | +3.44 |
| 16384 | 128 | 128 | 39.26 | 45.16 | +5.90 |

orig 参考値: cap=1024 → 37.42、cap=4096 → 30.14、cap=16384 → 27.61。

## 5. 観察

- **j6 は全 9 cell で j5 より遅い** (Δ +2.5 〜 +11.3 ns/op)。
- per_shard が大きい (= 1 shard 内の SIMD scan が長い) ほど劣化幅が増える: per_shard=32 で +2〜3 ns、
  per_shard=128 で +6〜11 ns。スケールは scan 長に比例。
- j5 が orig を抜いていたセル (cap=1024, per_shard=32: 31.67 vs 37.42) は j6 でも維持
  (34.40 vs 37.42)、cap=4096 / per_shard=32 では j6 のほうが orig より遅くなった (34.66 vs 30.14、
  j5=31.53 だった)。**j5 の throughput 優位は M2.1 の追加で部分的に失う**。

## 6. 解釈 (仮説)

期待されていた「visited RMW が tags 同居になり -1〜2 ns」は出ず、逆に劣化。要因の候補:

1. **AVX2 経路の +1 命令 (vpand)** が SIMD scan のクリティカルパス上にあり、scan 長に比例する
   コストが乗る。per_shard=128 で劣化が大きいことと整合する。
2. **scan 中 tag RMW (visited クリア) が `vpand` の port 5 と競合**する可能性。j5 では visited は
   別配列 (Entry 内 bool) なので scan の SIMD 経路と独立して書ける。
3. **Entry 16 B 化で `entries` ベクタが contiguous により詰まる** ぶんの cache 利得は、Twitter
   trace の hit ratio 帯 (~63%) では assume_init_ref が触る回数が j5 と同じなので相殺されない。

memory 削減効果 (-28% の inline footprint) 自体は構造的に達成しているが、本ベンチでは
**未だ memory-fair な比較フレームを組んでいない** (cap ベース sweep)。M2.1 の真の評価は
「同じ inline bytes 予算で j5 と j6 のどちらが速いか」で出すべきで、それは別レポート。

## 7. 結論

- **M2.1 単体での throughput は劣化** (Twitter cluster018 で +2.5〜+11.3 ns/op)。
- correctness は確定 (j5 と外部一致、unit tests 全 pass)。
- improvement-ideas.md §M2.1 の「hit-path -1〜2 ns 改善」予想は **棄却**。
- ただし memory-fair (= 同 inline bytes/cap) の比較はまだ。次は cap_orig = X、
  cap_j5 = X * (25/34)、cap_j6 = X * (25/20) で揃えた sweep をかけ、
  「同じメモリ予算で j6 の hit ratio + throughput が j5 を上回るか」を測る。

## 8. 次の一手

1. **memory-fair sweep**: cap を inline bytes/cap で割って揃え、hit ratio × throughput Pareto。
2. M1 (`order_cap` slack 削減) との合わせ技で更に footprint を絞り、再度 j5 と比較。
3. AVX2 命令ストール疑惑の検証: `vpand` を抜いた scalar 比較版 j6 を試作し、Δ がどこから来るか切り分け。
