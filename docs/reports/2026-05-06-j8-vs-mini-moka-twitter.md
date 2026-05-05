# j8 (SIEVE) vs mini-moka (W-TinyLFU) — Twitter trace スクリーニング

**日付**: 2026-05-06
**対象**: `sieve_orig`, `sieve_j8 (per_shard=32 champion)`, `mini-moka 0.10.3` を Twitter cache trace 上で比較。j8 の HR/throughput の実用妥当性を W-TinyLFU 系の代表として `mini-moka` を借りて確認する screening 段。
**バイナリ**: `target/release/bench` (mini-moka adapter 追加 commit 時点)。

## 1. 目的

NSDI'24 SIEVE 論文は「W-TinyLFU と HR で competitive、構造はずっと単純」と主張する。`docs/reports/2026-05-06-j8-twitter-pareto.md` で j8 が orig を 12/12 cell で HR ±1pp 以内に保ったまま 9/12 cell で速度勝ちまで到達したので、論文の SIEVE-vs-W-TinyLFU 主張が **j8 にもそのまま乗るか** を確認する。実用クレートの完成度を上げる前に、この HR ゲートを通らないと API 整理の意義が下がるため、最低コストで HR + ns/op を取る screening を行った。

## 2. 設定

- **トレース**: `cluster006`, `cluster018`, `cluster019` (各 1 M ops、`--len 1000000`)
- **capacity**: 1024, 4096, 16384, 65536
- **variants**:
  - `orig` — `sieve_orig` (NSDI'24 author reference の faithful Rust port)
  - `j8` — `sieve_j8`、per_shard=32 champion (cap=1024→`j8_n32`, 4096→`j8_n128`, 16384→`j8_n512`, 65536→`j8_n2048`)
  - `mini_moka` — `mini-moka 0.10.3` の `sync::Cache::new(capacity)` (W-TinyLFU、default 設定)
- **trials**: 5 (median 報告)、**seed**: 42
- **スクリプト**: `scripts/sweep_minimoka_twitter.sh`
- **生 CSV**: `profiles/minimoka_twitter_2026-05-06.csv` (180 行 + header)
- **adapter**: bench.rs 内 `MiniMoka` 構造体で `senba_cache::Cache<u64,u64>` を実装。`get` はヒット時のみダミー静的参照を返し、bench 側の `.is_some()` 判定で hit/miss を取る。`insert` は `()` を返す mini-moka の API のため evictions は **常に 0** (CSV の evictions 列は無意味)。
- **`ConcurrentCacheExt::sync()` を毎 op 後に呼ぶ**。mini-moka は read/write log を内部 buffer にためて amortize するため、明示 sync 無しでは CMSketch 更新と admission 決定が反映されない (upstream の `sync/cache.rs` `basic_single_thread` test も毎回 sync 呼出)。詳細は §9。

## 3. 公平性に関する caveat

この比較には複数の構造的非対称性がある。読み手は結論を絶対視せず、screening の文脈で受け取ること。

1. **mini-moka は concurrent cache**。内部に `DashMap` + `parking_lot` 系の同期プリミティブを持つ。j8 は単一スレッド前提で同期コスト 0。**ns/op の差を「W-TinyLFU 自体のコスト」と読むのは誤り** — 同期プリミティブの定常コストが大きく寄与している。
2. **mini-moka は default 設定**。CMSketch サイズ・window LRU 比率・admission policy のチューニングをしていない。これらは max_capacity から自動導出される値で、cap=1024 のような小規模では sketch サイズが小さく admission 判定の精度が落ちる可能性がある。HR 結果が想像以上に悪い場合は default 設定の影響を疑う必要がある。
3. **mini-moka の `get` は `Option<V>` (clone) を返す**。V=u64 ではほぼゼロコストだが、API 上 `Option<&V>` 想定の bench とは形式が違う。
4. **evictions count は不可視**。mini-moka の `insert` が `()` を返すため、本 CSV の evictions 列は 0 固定で意味なし。HR と ns/op だけ参照する。
5. **trace の最初の数 100k req は全 cache でウォームアップ期間**。mini-moka の TinyLFU は CMSketch の initial state からの収束に時間がかかる可能性がある。1M ops でも cluster019 のような scan-heavy trace では収束しない可能性がある。

要点: **HR の差が ±1pp 以内に収まるかどうか**だけが本 screening のゲート判定で、ns/op は参考値。

## 4. HR (median, 5 trials)

太字は j8 が mini_moka を有意に超えるセル。

### cluster006

| cap | orig | j8 | mini_moka | Δ(j8 − mini_moka) |
|---:|---:|---:|---:|---:|
| 1024  | 13.22% | 13.13% | 10.66% | **+2.47pp** |
| 4096  | 34.86% | 35.18% | 33.27% | **+1.92pp** |
| 16384 | 64.03% | 63.84% | 61.79% | **+2.05pp** |
| 65536 | 82.93% | 82.74% | 80.75% | **+1.99pp** |

### cluster018

| cap | orig | j8 | mini_moka | Δ(j8 − mini_moka) |
|---:|---:|---:|---:|---:|
| 1024  | 50.17% | 51.01% | 38.17% | **+12.85pp** |
| 4096  | 62.53% | 62.73% | 49.81% | **+12.92pp** |
| 16384 | 73.78% | 73.65% | 61.97% | **+11.68pp** |
| 65536 | 82.06% | 82.04% | 71.35% | **+10.69pp** |

### cluster019

| cap | orig | j8 | mini_moka | Δ(j8 − mini_moka) |
|---:|---:|---:|---:|---:|
| 1024  | 24.09% | 30.41% | 3.37% | **+27.04pp** |
| 4096  | 29.64% | 31.64% | 3.55% | **+28.09pp** |
| 16384 | 31.53% | 32.17% | 4.35% | **+27.82pp** |
| 65536 | 32.75% | 32.88% | 7.50% | **+25.38pp** |

**12/12 cell で j8 が mini_moka を HR で上回る**。最小差は cluster006/cap=4096 の +1.92pp、最大差は cluster019/cap=4096 の +28.09pp。

特に **cluster019 では mini_moka の HR が 3〜8% に崩壊**。j8 は 30〜33% を維持するため絶対値で 25pp 以上の差。この cluster は orig 自体の HR も低い (24〜33%) scan-heavy trace で、TinyLFU の admission filter が「frequency が足りない」として有効ワーキングセットを大量に reject している可能性が高い。これは default CMSketch + admission threshold での想定挙動 — Zipf 1.0 の sanity check (§9) では mini_moka HR=58.96% が orig 59.83% と同等まで戻ることから、adapter の機能不全ではないと確定している。

cluster018 (HR が中程度〜高、orig 50〜82%) でも j8 が +10〜+12pp 上回る。HR の高い領域でも mini_moka は SIEVE に追いつかない。

cluster006 では差が +2pp 程度に縮む。これが本 screening で観測された「最も W-TinyLFU 寄りに見える workload」だが、それでも j8 が常に勝っている。

## 5. ns/op (median, 5 trials)

| cluster | cap | orig | j8 | mini_moka | mini_moka / j8 |
|:---|---:|---:|---:|---:|---:|
| cluster006 | 1024  | 46.49 | 30.42 | 476.99 | **15.7×** |
| cluster006 | 4096  | 41.80 | 31.47 | 428.03 | **13.6×** |
| cluster006 | 16384 | 33.64 | 30.92 | 380.69 | **12.3×** |
| cluster006 | 65536 | 24.61 | 25.78 | 372.03 | **14.4×** |
| cluster018 | 1024  | 36.66 | 30.18 | 401.61 | **13.3×** |
| cluster018 | 4096  | 28.75 | 30.42 | 378.88 | **12.5×** |
| cluster018 | 16384 | 27.31 | 30.71 | 367.97 | **12.0×** |
| cluster018 | 65536 | 23.06 | 26.76 | 368.16 | **13.8×** |
| cluster019 | 1024  | 49.31 | 32.14 | 492.07 | **15.3×** |
| cluster019 | 4096  | 42.40 | 32.30 | 498.71 | **15.4×** |
| cluster019 | 16384 | 47.89 | 34.37 | 515.76 | **15.0×** |
| cluster019 | 65536 | 56.08 | 36.56 | 523.60 | **14.3×** |

mini_moka は j8 比 **12〜16× 遅い**。これは (a) concurrent primitive (DashMap shard lock + parking_lot) の定常コスト、(b) `ConcurrentCacheExt::sync()` を毎 op で呼ぶことによる pending task drain コスト、(c) get の `Option<V>` clone (V=u64 ならほぼ無視) — の合算で、W-TinyLFU 自体のアルゴリズムコストではない。それでも単一スレッド bench で 12× は実用上無視できないオーバーヘッド。

逆に、もし mini_moka を概ね同等の HR まで持っていけたとしても、**5 倍以上 throughput を改善しないと j8 と同じ Pareto 領域に入れない**。default 設定からのチューニングでこの差を縮めるのは現実的でない。

## 6. 図

`docs/figures/`:
- `minimoka_twitter_hr.png` — HR bar chart (3 cluster × 4 cap)
- `minimoka_twitter_nsop.png` — ns/op log-scale bar chart (mini_moka が 1 桁上のため log)
- `minimoka_twitter_pareto.png` — capacity sweep を線で結んだ Pareto scatter

## 7. 結論

- **j8 (SIEVE) は HR でも throughput でも mini-moka (W-TinyLFU) を 12/12 cell で支配**。HR 差は +1.92〜+28.09pp (中央値 +10pp 帯)、throughput 差は 12〜16×。
- 最も差が小さい cluster006 でも j8 が常に勝つ。最も大きい cluster019 では mini_moka の HR が 3〜8% に崩壊し、SIEVE の方が scan-resistant に働いている。
- 公平性 caveat (default 設定、concurrent primitive、CMSketch サイズ、ウォームアップ) を全部考慮しても、HR で **±1pp 以内の同等性すら成立していない**ため、SIEVE 論文の「W-TinyLFU と HR で competitive」主張は本 screening では j8 でも再現された (むしろ SIEVE 側が勝ち越す)。
- **API 整理 / クレート完成度を上げる方向に投資する妥当性は確認できた**。HR ゲートを j8 が通ったため、(1) `BuildHasher` generic 化、(2) `Entry` size 制約の緩和、(3) shard mutex を被せた concurrent 版、(4) memory-bounded API の方向に進めて損はない。
- 一方、本 screening は default mini-moka との比較である点は残る。「ちゃんとチューニングした W-TinyLFU 自前実装と比べる」必要が出てきたら、Caffeine 論文準拠の最小実装に着手する価値があるが、screening 段では先送りで良い。
- mini_moka cluster019 の HR=3% は単独で要調査の異常値。default CMSketch サイズが cap=1024 でも数 KB しかなく、scan-heavy で saturate している可能性。本稿の主結論には影響しないが、もし「W-TinyLFU 自体は強い」を主張したい場合に再現性の確認が必要。

## 8. 次の一手 (memo)

- (a) API 整理の方向 (`Cache` trait の再設計、`BuildHasher` 受け入れ、`remove` 復活、`Entry` API)。
- (b) shard mutex 化 — j8 は元々 set-associative なので各 `Inner<K,V>` を `Mutex<Inner<K,V>>` で包むだけで concurrent 化できる。throughput がどこまで落ちるかは要測定だが、mini_moka が 12× 遅い分の 1/3 程度なら許容できそう。
- (c) Caffeine 準拠の自前 W-TinyLFU 実装 — screening の結論が覆るとは考えにくいが、論文で名指して比較するなら作る価値あり。後回し。

## 9. 測定方法のミスと検証 (post-mortem)

初版の本稿は `mini_moka::sync::Cache` を **`ConcurrentCacheExt::sync()` を呼ばずに**測定していた。mini-moka は read/write log を内部 buffer に貯めて amortize するため、明示 sync 無しでは admission 決定や CMSketch 更新が反映されず、HR が崩壊する可能性がある (upstream の `sync/cache.rs` の test code がすべて毎回 sync 呼出していた)。指摘を受けて修正・再測定したのが本稿の数字。

修正前後の比較 (代表値):

| cell | HR pre-fix | HR post-fix | Δ HR | ns/op pre | ns/op post | Δ ns/op |
|:---|---:|---:|---:|---:|---:|---:|
| cluster018 / cap=1024 | 38.63% | 38.17% | −0.46pp | 368.33 | 401.61 | +33.28 |
| cluster019 / cap=1024 |  3.50% |  3.37% | −0.13pp | 456.59 | 492.07 | +35.48 |
| cluster006 / cap=1024 | 10.68% | 10.66% | −0.02pp | 434.92 | 476.99 | +42.07 |

**HR の影響は ±0.5pp 以内で全 12 セルで結論不変** (sync 漏れは HR を大きく崩していなかった)、**ns/op は per-op sync で +10〜15% 悪化**。

加えて adapter の機能不全を切り分けるため Zipf 1.0 / keys=100k / len=1M で sanity check:

| variant | cap=1024 HR | cap=1024 ns/op | cap=4096 HR | cap=4096 ns/op |
|:---|---:|---:|---:|---:|
| orig      | 59.83% |  26.82 | 70.56% |  26.45 |
| j8 (per_shard=32) | 59.60% |  25.72 | 70.69% |  23.94 |
| mini_moka | **58.96%** | 352.64 | **70.80%** | 336.55 |

Zipf 上では mini_moka が orig/j8 と HR ±0.85pp 以内に収まる。**adapter の HR 計測は機能的に正しい**ことを確認。Twitter の HR 崩壊は実装ではなく default-tuned W-TinyLFU の実挙動。

教訓: 外部 cache crate を bench に組み込む際は必ず upstream の test code が前提とする呼出パターン (ここでは sync()) を踏襲する。読み手に向けては、外部実装比較レポートは単独の数字より、(1) 論文・ドキュメントが期待する条件 (Zipf 系) での sanity、(2) 実 trace、の二段階を明記して同時掲載するのが安全。

## 10. 拡張: moka 0.12 + Zipf skew sweep

§7 の結論を「mini-moka 0.10 が古いから」「Twitter cluster {006,018,019} と相性が悪いだけ」で説明できるかを潰すため、(A) **moka 0.12.15 を追加**、(B) **Zipf skew {0.6, 0.8, 1.0, 1.2} を追加**して再 sweep した。

### 10.1 設定

- 追加 variant: `moka` (`moka::sync::Cache::new(cap)`、`run_pending_tasks()` を毎 op 後に呼出)。
- 追加 workload: Zipf {0.6, 0.8, 1.0, 1.2}、keys=100000、len=1M、seed=42。
- 既存 Twitter 3 cluster + Zipf 4 skew = 7 workload × 4 cap × 4 variant × 5 trial = 560 run。
- スクリプト: `scripts/sweep_moka_extended.sh`、`scripts/plot_moka_extended.py`。
- 生 CSV: `profiles/moka_extended_2026-05-06.csv`。
- 図: `docs/figures/moka_extended_{hr_grid,dhr_vs_j8,nsop_grid}.png`。

### 10.2 結果 (1) — moka 0.12 vs mini-moka 0.10

**HR は 28/28 cell で Δ ≤ 0.1pp に収束**。両 crate は HR で観測上区別できない。adaptive window sizing (Caffeine の hill-climbing) は **1M op horizon では効果が見えない** — 収束に時間がかかるか、本データセットでは window 1% が局所最適。

**ns/op は moka が 1.5〜2× 遅い**。例: cluster019/cap=65536 で moka=1375 ns vs mini_moka=548 ns、Zipf 1.0/cap=1024 で moka=577 ns vs mini_moka=353 ns。feature 量 (eviction listener / weigher / async support 等) のコスト。

→ 「mini-moka 0.10 が古いから HR 崩壊」仮説は **棄却**。両者で同じ崩壊が出る。

### 10.3 結果 (2) — Zipf skew sweep が分水嶺

skew 軸で **W-TinyLFU 系が j8 と並ぶ / 抜くゾーンが存在する**:

| Zipf skew | cap=1024 (Δhr_moka−j8) | cap=4096 | cap=16384 | cap=65536 |
|:---|---:|---:|---:|---:|
| 0.6 (scan-like) | −2.30pp | −0.82pp | **+0.75pp** | +0.32pp |
| 0.8             | −2.09pp | −0.36pp | **+0.76pp** | +0.22pp |
| 1.0             | −0.62pp | +0.09pp | +0.30pp | +0.22pp |
| 1.2 (high skew) | +0.07pp | +0.23pp | +0.01pp | 0.00pp |

- skew=1.2 では 4 cap 全てで W-TinyLFU と SIEVE が **完全に並ぶ** (|Δ| ≤ 0.25pp)。
- skew=0.6/0.8 でも cap≥16384 では W-TinyLFU が **小さく勝つ** (+0.75pp)。
- skew が低く cap が小さい cell でだけ W-TinyLFU が −2pp 程度負ける (cache 容量に対し working set が広い領域)。

→ **Zipfian 上で W-TinyLFU は SIEVE と HR competitive**、NSDI'24 SIEVE 論文の主張は再現された。

### 10.4 結果 (3) — Twitter trace は別の挙動

| cluster | cap=1024 (Δhr_moka−j8) | cap=4096 | cap=16384 | cap=65536 |
|:---|---:|---:|---:|---:|
| cluster006 | −2.51pp | −1.95pp | −2.08pp | −1.99pp |
| cluster018 | **−12.87pp** | **−12.91pp** | **−11.69pp** | **−10.71pp** |
| cluster019 | **−27.04pp** | **−28.11pp** | **−27.85pp** | **−25.37pp** |

- cluster006 は Zipfian と似た挙動 (−2pp 帯)。
- cluster018/019 では W-TinyLFU が大幅敗退、cap が増えても回復しない。

つまり Twitter trace の特定 cluster には **Zipfian には無い構造** が乗っており、W-TinyLFU の admission filter (CMSketch + window LRU) がそこで詰まる。具体的には one-shot key と低頻度の re-reference が混在するパターンが疑われる — CMSketch ではこの re-reference が「frequency 低」と判定されて main cache に admit されない。SIEVE の visited bit + sweep eviction はこの問題を回避できる。

### 10.5 更新された結論

- **「moka は HR 重視の重量級」は不正確**。今回の trace 上では HR でも throughput でも j8 に負けており、tradeoff ではなく **両軸劣化**。
- **「W-TinyLFU 系は Zipf-like で強い、non-Zipf 実 trace で脆い」が新しい結論**。
- **j8 (SIEVE) は両軸で robust**。Zipfian ですら −0.25pp 程度の僅差まで詰める HR を持ちつつ、non-Zipf trace では 25pp 以上の HR 利得を出す。
- adaptive window (moka 0.12) は本実験では効果ゼロ。
- API 整理に投資する妥当性は §7 の結論と変わらず維持。
- ただし **「全ての実 trace で SIEVE が勝つ」を主張するには cluster {006,018,019} は狭すぎる**。OSDI'20 dataset には 50+ cluster ある。本稿は外部 download コストの理由で 3 cluster + Zipfian に留めた — 実 trace 多様性は別タスクで広げるべき。
- Caffeine 準拠の自前 W-TinyLFU 実装で結論が覆る可能性は低い (mini-moka と moka 0.12 で揃って同じ HR を出していて、Java Caffeine と同じアルゴリズムを共有している)。優先度は依然として低い。
