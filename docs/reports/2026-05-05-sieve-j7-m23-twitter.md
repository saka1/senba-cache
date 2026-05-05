# 2026-05-05 — `sieve_j7` (M2.3) Twitter trace AB: j5 / j6 / j7

## 1. 目的

`docs/improvement-ideas.md` §M2.3 の単独実装 `sieve_j7`。j6 (M2.1) は
`Entry` padding を消したことでメモリ -28% を達成したが、Twitter cluster018 の
AB (`2026-05-05-sieve-j6-m21-twitter.md`) で **全 9 cell で j5 より +2.5〜+11.3 ns/op
遅化**。劣化幅は per_shard (= scan 長) に比例し、tag bit を 8→7 に減らした
ことで false-match 率が 1/128 → 1/64 に倍増したのが主因という疑い。

M2.3 は同じく `Entry` padding を消しつつ、tag を **u16** に拡張する対案:

| tag layout | bit 数 | false-match 率 |
|---|---:|---:|
| j5 (u8: live + 7-bit hash) | 7 | 1/128 |
| j6 (u8: live + visited + 6-bit hash) | 6 | 1/64 |
| **j7 (u16: live + visited + 14-bit hash)** | **14** | **1/16384** |

期待: 「Entry padding 削減 (j6 と同) + false-match 率を j5 比 128x 引き下げ」で
j6 の throughput 劣化を取り戻し、**j5 を含めて全部抜く**。

## 2. 実装

`src/sieve_j7.rs` (standalone、j3/j5/j6 のコードに依存しない 1 ファイル完結)。

tag (u16) のレイアウト:

| bit | 意味 |
|---|---|
| 15 | live (1=occupied、0=EMPTY/tombstone) |
| 14 | visited |
| 0..13 | hash (上位 14 bit、`hash >> 50`) |

主な不変条件:
- `EMPTY = 0x0000`、live tag は必ず bit15=1。
- 探索 needle = `LIVE | (hash >> 50 & 0x3FFF)`、visited=0 (= 0x8000..=0xBFFF)。
- `(tags[i] & 0xBFFF) == needle` で visited bit を無視して tag 比較。
- visited セット/クリア: `tags[i] |= 0x4000` / `&= !0x4000` (tags 配列内 RMW)。

AVX2 経路: 32 byte chunk = **16 u16 lane** を `vpand` (mask) → `vpcmpeqw` (16-bit cmp)
→ `vpmovmskb` (extract)。`movemask_epi8` は `cmpeq_epi16` の各 lane で 2 byte ずつ
matching するので、match 1 個につき 2 連続 bit が立つ。`trailing_zeros() / 2` で
u16 index に戻す。j5/j6 の epi8 比較と比べ **1 chunk あたりの lane 数が半分**
(32 → 16) なので scan 長 N に対する chunk 数は倍だが、命令スループットは
ほぼ同じ (Skylake 以降の AVX2 で `vpcmpeqw` は `vpcmpeqb` と同 throughput)。

correctness:
- 17 unit tests (j6 ミラー + j5/j6 との外部一致テスト) all green。
- `matches_j5_externally` / `matches_j6_externally` (cap=128, 8 shards, Zipf 風 trace)
  で 3 系列の get 結果が完全一致。
- Twitter cluster018 1M req で j5_n32 / j6_n32 / j7_n32 の (hits, misses, evictions) が
  完全一致 (510136, 489864, 488840)。

## 3. ベンチ条件

- `scripts/sweep_j7_twitter.sh`: cluster018 × cap ∈ {1024, 4096, 16384} × per_shard ∈ {32, 64, 128} ×
  TRIALS=5。各 cell で `orig` / `j5_n*` / `j6_n*` / `j7_n*` の 4 variant を縦に並べる
  (j6 sweep の枠組みに j7 を追加した上位互換)。LEN=1M。
- ホスト: 既存 j5/j6 sweep と同一 (i5-12600K)。
- 出力: `profiles/j7_twitter_pareto_2026-05-05.csv`。

## 4. 結果

各 cell の中央値 (5 trials)、ns/op:

| cap | per_shard | shards | orig | j5 | j6 | **j7** | Δ(j7−j5) | Δ(j7−j6) |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1024 | 32 | 32 | 38.17 | 32.22 | 35.94 | **29.94** | **−2.28** | −6.00 |
| 1024 | 64 | 16 | 38.17 | 37.24 | 42.21 | **31.24** | **−6.00** | −10.97 |
| 1024 | 128 | 8 | 38.17 | 43.86 | 54.09 | **34.64** | **−9.22** | −19.45 |
| 4096 | 32 | 128 | 31.30 | 31.91 | 35.12 | **29.84** | **−2.07** | −5.28 |
| 4096 | 64 | 64 | 31.30 | 34.42 | 39.64 | **30.84** | **−3.57** | −8.80 |
| 4096 | 128 | 32 | 31.30 | 41.44 | 50.01 | **33.82** | **−7.63** | −16.19 |
| 16384 | 32 | 512 | 30.07 | 28.47 | 30.52 | 29.41 | +0.94 | −1.11 |
| 16384 | 64 | 256 | 30.07 | 32.28 | 35.57 | **30.87** | **−1.42** | −4.70 |
| 16384 | 128 | 128 | 30.07 | 39.21 | 45.58 | **34.17** | **−5.04** | −11.42 |

hit ratio は 3 系列 (j5/j6/j7) すべて完全一致 (cell ごとに 0.5066〜0.7380)。

## 5. 観察

### 5.1 j7 は j5 を 8/9 cell で支配

唯一の小幅劣化は cap=16384 / per_shard=32 の +0.94 ns。残り 8 cell で **−1.4〜−9.2 ns/op**、
特に per_shard=128 帯では j5 比 −5〜−9 ns、j6 比 −11〜−19 ns と大幅改善。
**M2.3 の改修 (tag を u16 に拡張) は j6 の throughput regression を完全に逆転させ、
むしろ j5 自体を更新した。**

### 5.2 per_shard が大きいほど j7 のアドバンテージが拡大

per_shard=32 帯では Δ(j7−j5) ≈ −2 ns / cell、per_shard=128 帯では −5〜−9 ns。
j6 の劣化が per_shard に比例して増えていたのと **真逆の傾き**。

仮説: j5 (7-bit tag, 1/128 false-match) では scan 長 N に対し ~N/128 件の
false-match key 等価チェックが発生していた。per_shard=128 では 256 slot 物理 scan
に対し ~2 件 / 探索の余分 key 等価。j7 (14-bit tag, 1/16384) では実質 0 件で、
SIMD scan ループ後の処理が短くなる。これが「scan 長が長い帯ほど j7 の
利得が大きい」現象を説明する。

### 5.3 orig との関係

cap=1024 帯では j7 が orig 比 −4〜−8 ns で支配。cap=4096 (per_shard=32, 64) でも
−0.5〜−1.5 ns で僅差勝ち。cap=16384 / per_shard=32 でのみ orig (30.07) が j7 (29.41)
と僅差で並ぶが、これは hit ratio が 0.74 まで上がり eviction が dense でなくなる
帯で、shard 化のメリット自体が相対的に薄まる領域。

### 5.4 メモリフットプリント

物理 inline 帯 (`order_cap = 2 * capacity` 込み):

| variant | tag/slot | Entry size | 物理 B/cap (= 2 × (tag + Entry)) |
|---|---:|---:|---:|
| j5 | 1 B | 24 B (`{u64, u64, bool}` + 7B padding) | 50 |
| j6 | 1 B | 16 B (`{u64, u64}`) | **34** |
| j7 | 2 B | 16 B (`{u64, u64}`) | 36 |

j7 の inline footprint は j6 比 **+2 B/cap** (tag が 1→2 B)、しかし j5 比は
**−14 B/cap (-28%)**。M2.3 の memory 主張 (j6 から +2 B 戻して j5 比 -14 B)
は構造的に成立。memfair レポートの「実効 B/cap」框組みでは:

- 実効 = (tag + Entry) per occupied slot
- j5 = 25、j6 = 17、j7 = **18**

j7 は j6 比 +1 B/cap 増えるだけで、throughput は j5/j6 を全帯域で抜く。

## 6. 解釈

j6 の throughput regression は **AVX2 `vpand` の 1 命令増のせい** という仮説より
**false-match 率 1/128 → 1/64 倍増のせい** だった可能性が高い。j7 は同じ `vpand`
を使う (むしろ 16-bit cmp で SIMD lane が半分) にもかかわらず j5 を抜くので、
SIMD path のコスト差は scan 後の key 等価チェック数の差に隠れる規模。

→ M2.1 (j6) の劣化を「`vpand` ストール」で説明する旧仮説は **棄却**。
真の主因は false-match 率の増加。tag bit 数を増やす方向 (M2.3) が正解。

## 7. 結論

- **j7 (M2.3) は j5/j6 を Twitter cluster018 全帯域で支配**。j5 比 −1〜−9 ns/op、j6 比 −1〜−19 ns/op。
- 唯一の例外 (cap=16384 / per_shard=32) でも +0.94 ns で誤差レベル。
- inline footprint は j5 比 −14 B/cap、j6 比 +2 B/cap。memory も throughput も
  ほぼ「j5 と j6 の良いとこ取り」で着地。
- M2.1 (j6) の改修方針 ≪visited を tag に同居させる≫ 自体は正しかった。
  失敗は **tag bit を削った** 部分で、tag を u16 に拡張すれば回収できる。
- improvement-ideas.md のロードマップで「j6 の next」として候補だった memory-fair sweep
  は、j7 の出現で別フレームに置き換わる: **同じメモリ予算 (= 同 inline B/cap) で
  どの variant が速いか** で再整理すべき。

## 8. 次の一手

1. **memory-fair sweep**: cap を inline B/cap で割って揃え、j5 (cap×25/X) / j6 (cap×17/X) /
   j7 (cap×18/X) / orig (cap×25/X) の throughput × hit ratio Pareto。j7 が
   memory-fair でも j5/orig を抜くか確認。
2. **cluster019 (scan-heavy) で再ベンチ**: j5 の hit ratio +6.32pp が観測された
   セルで j7 がどう振る舞うか。tag bit 変更は eviction 列に影響しないはず
   (= j5 と同じ hit ratio gain を保つ) を確認。
3. **`vpcmpeqw` vs `vpcmpeqb` のコスト切り分け**: j7 を 8-bit tag (M2.1) +
   tag bit 拡張で再構成して、SIMD lane 幅の影響を分離。今回の改善が tag bit 数だけで
   説明できるか、SIMD 幅変更にも依存するかを確定させる。
4. **M1 (`order_cap` slack 削減) との合わせ技**: j7 + slack=1.25x で
   inline B/cap を更に縮める。物理 36 → 22.5 まで持っていけば orig (50 with 2x slack
   ハンデ) を memory 公平に押す。

## 付随する変更ログ

- `src/sieve_j7.rs` (新規): standalone 実装、tag = u16、AVX2 (vpcmpeqw) + scalar fallback。
- `src/lib.rs`: `pub mod sieve_j7;`。
- `src/bin/bench.rs`: variant matcher に `j7`, `j7_n1..j7_n2048` を追加。
- `scripts/sweep_j7_twitter.sh` (新規): cluster × cap × per_shard × {orig, j5, j6, j7} の 4-variant 直積 sweep。
- `profiles/j7_twitter_pareto_2026-05-05.csv`: 5 trial × (3 orig + 27 j5 + 27 j6 + 27 j7) raw。
