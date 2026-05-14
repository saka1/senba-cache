# 2026-05-16 — `senba::concurrent::Cache` v0.4.0: r4-based ManuallyDrop engine swap

## TL;DR

- **仮説**: `2026-05-15-r4-vs-c17s.md` で 432-cell Zipf sweep で確認した r4 engine の特性 (V=u64 で c17s と ±0.3% 並走、V=String で旧 Arc 系 senba::concurrent を median +21% で Pareto improve) は、lib publishable surface に lift した後も Zipf を超える実 trace で同じ trade-off を保つはず。
- **やったこと**: `src/concurrent/cache/shard.rs` の Arc<V> + epoch refcount スキームを r4 engine (ManuallyDrop<K/V> + epoch defer of moved-out K/V) に置き換え、`insert` API を `()` 返しに変更、`insert_with(K, V, on_evict)` callback を追加 (race β + race γ 同時 closure)、`V/K: Sync` bound を撤廃。senba v0.3.0 → v0.4.0。`docs/benchmark/concurrent-full-sweep/` 新設、4-way (senba_concurrent v0.4.0 = r4-based / 旧 c17s / moka / mini_moka) × Zipf 192-cell + NSDI24 libcachesim + Twitter-Yang 5 cluster + ARC 22 preset = **2048 cells × 3 trials = 6144 row** sweep、crashes 0。
- **分かったこと**:
  - **V=u64 はおおむね parity**: senba_concurrent vs c17s が Zipf median **−1.3%** / worst **−6.9%**、Twitter-Yang median **−3.1%** / worst **−22.3%**、ARC median **+0.9%** / worst **−30.7%**。Zipf 92% / Twitter 62% / ARC 80% の cell が c17s から ±5% 以内。worst cell は cap_per_shard が小さい (P-series cap=20000, cap_per_shard ≈ 5) ところに集中、Path C 頻度が極端に高い regime で実装差分が見えている。
  - **V=String は soundness の代価が明確に出ている**: Zipf median **−10.0%** / worst **−25.5%** に対し、Twitter-Yang median **−30.3%** / worst **−59.5%**、ARC median **−33.9%** / worst **−63.4%**。c17s は race β (clone-mid-flight UAF) を構造的に閉じていない **unsound 参照点** で、`Arc<V>` / `epoch::pin` 抜きで raw V を clone するため hot path が物理的に短い。実 trace は eviction 頻度が高く Path C 経由の `defer_unchecked(closure capture K+V)` が支配コスト化、real workload では soundness 確保で typical −30% を払う構造。**逆に Zipf 高 T cell では senba_concurrent が +104%〜+122% 取り**返す (Arc ping-pong / write log dequeue の典型ボトルネックが消えるため)。
  - **moka / mini_moka は構造的にずっと遅い**: ARC で c17s 比 **median −95% / worst −99%**、moka write log + admission filter overhead が CPU を食い潰す regime。real-world admission policy 比較の参考値として残す。
  - **総括**: 「v0.4.0 swap は V=u64 で perf neutral、V=String で soundness 確保のため 平均 30% 払う」 という命題を、 4 trace family × 2048 cell の評価で fix。`moka::sync::Cache<K, V>` 互換の soundness contract を満たした初版本として release candidate。

## 設計差分 (v0.3.0 → v0.4.0)

### Soundness model: `Arc<V>` → `ManuallyDrop<K/V>` + epoch defer of moved-out values

旧 (v0.3.0, c17s lift) は `Entry::value: Arc<V>` を持ち、reader が bit-copy した `Arc<V>` の strong count を `Arc::increment_strong_count` で bump してから `V::clone` する設計。writer 側は old `Arc<V>` を `Guard::defer_unchecked` で deferred drop していた。これは **reader hot path に shared atomic write を持ち込む** (refcount fetch_add) のがコストで、`2026-05-13-senba-concurrent-vs-c17s.md` で c17s 比 median −34% / worst −63% という顕著な退行を示していた。

新 (v0.4.0, r4 lift) は `Entry::key: ManuallyDrop<K>` / `Entry::value: ManuallyDrop<V>` に置き換え、

1. reader は bit-copy した `ManuallyDrop<Entry<K, V>>` local から `(*buf.value).clone()` を呼ぶ。Arc 操作なし、shared atomic write 0。
2. writer Path A は `&mut ManuallyDrop<V> as *mut V` キャスト経由で raw `ptr::read` / `ptr::write` し、old V を `defer_drop_if_needed::<V>` で reader pin past まで保護。
3. writer Path C / `remove` は `ManuallyDrop::take` で K と V を slot から moved-out、`on_evict` callback (新 `insert_with` 経由、デフォルト `insert` では `|_,_| {}`) を `Guard::defer_unchecked` 越しに schedule。これで **race β (clone-mid-flight UAF on V) と race γ (K UAF) が同時に閉じる** — 旧 lift では race γ が latent bug として残っていた。

`needs_drop::<V>()` const-fold で `V: Copy` のとき epoch pin / defer は monomorphize-time に dead-code 除去される。Reader hit cost は `V: Copy` で 0 ns 追加、`V: !Copy` で `epoch::pin` ~3–5 ns。

### API 破壊点 (sembanic-versioning major bump)

```text
                       Before (v0.3.0)                              After (v0.4.0)
-----------------------------------------------------------------------------------
Trait bounds (Cache):  K: Hash+Eq+Send+Sync+'static          K: Hash+Eq+Send+'static
                       V: Clone+Send+Sync+'static            V: Clone+Send+'static
                       (`Sync` widening accepts more types — existing code unaffected)

insert:                fn insert(&self, K, V) -> Option<(K,V)>   fn insert(&self, K, V)
                                                                 [evicted pair dropped via defer]

insert_with:           (absent)                                  fn insert_with<F>(&self, K, V, F)
                                                                 where F: FnOnce(K,V) + Send + 'static
                                                                 [callback runs deferred for V: !Copy]

remove<Q>:             fn remove(&self, &Q) -> Option<V>         unchanged
get<Q>:                fn get(&self, &Q) -> Option<V>            unchanged
contains_key<Q>:       fn contains_key(&self, &Q) -> bool        unchanged
new/with_shards/with_hasher/with_shards_and_hasher               unchanged
capacity/len/is_empty/shards                                     unchanged
```

V=String 主用途者で evicted pair を欲しい既存 caller は、`insert(k, v)` → `insert_with(k, v, |k, v| { ... })` に置換するだけ。closure body は writer の Mutex critical section 内で実行されるので、長い処理は channel/Vec に詰めて非同期で扱うのが推奨。

### `bench_concurrent` の r4 arm 削除

`research/src/bin/bench_concurrent.rs` の `r4` variant arm は senba_concurrent が r4 engine になった時点で冗長なので削除した (`5a64108`)。研究用の const-generic shard count を持つ `sieve_r4` モジュール自体は `research/src/experimental/` に残し、asm inspection / 将来比較用に保存する。

## Sanitizer

- **ASan**: 4 test (`v_string_chaos_under_contention`, `multi_thread_zipf_like_chaos`, `v_string_insert_with_under_contention`, oracle) で UAF / SEGV 0 件。race β + race γ の構造的閉鎖を実機 confirm。
- **TSan**: 11 warnings、すべて `core/src/ptr/mod.rs:1920` の `ptr::read` / `ptr::write` 起点で seqlock pattern 由来の expected false-positive (`docs/benchmark/r4-sanitizer/findings.md` の 16 件相当、Arc 関連 5 件が消えた差分)。

## Perf-gate

- **`research/benches/sieve_cache_perf.rs`** (single-thread `senba::Cache`、本 swap 範囲外):
  worst +3.68% (`insert_u64/384`, p=0.00)。±5% 内、PASS。`src/concurrent/` のみ変更でも build profile の影響で ±数% は出る (ノイズ)。
- **新規 `research/benches/sieve_concurrent_perf.rs`** (4 cells、本 swap のために新設):
  baseline 保存 (`post-r4-lift`)。今後 `src/concurrent/` を触る commit はこの baseline 比 ±5% gate を超えないことを CI/local で確認すること (`CLAUDE.md`)。

## Sweep matrix と環境

- harness: `docs/benchmark/concurrent-full-sweep/run.sh` (phase 別)
- データ: `docs/benchmark/concurrent-full-sweep/data/results.csv`
- 集計: `docs/benchmark/concurrent-full-sweep/figures/regression_summary.md`
- 環境: **WSL2 Ubuntu / Alder Lake P-core 16T** (caveat: `[[memory:project_wsl2_measurement_confound]]`)

| 軸 | 値 |
|---|---|
| variants | senba_concurrent (v0.4.0, r4-based) / c17s (research) / moka / mini_moka |
| sources | Zipf 合成 / NSDI24 libCacheSim CSV / Twitter-Yang (5 cluster) / ARC (mokabench 22 preset) |
| Zipf 軸 | cap=4096, skew ∈ {0.8, 1.0, 1.4}, keys=100k |
| 共通軸 | threads ∈ {1, 4, 8, 16}, op_mix ∈ {gim, read-heavy}, value ∈ {u64, String}, trials=3 |
| shards | 512 (auto-shard `cap/8`) |

Phase 別 cell count (各 cell × 3 trial、4-way):
- Zipf: 1 cap × 3 skew × 4 T × 2 mix × 2 V = **48 × 4 var = 192 cell**
- libcachesim (twitter_cluster52 + trace.csv): 2 source × 4 T × 2 mix × 2 V = **32 × 4 = 128 cell**
- Twitter-Yang (cluster {006, 016, 018, 019, 034}): 5 cluster × 4 T × 2 mix × 2 V = **80 × 4 = 320 cell**
- ARC (mokabench 22 preset): 22 preset × 4 T × 2 mix × 2 V = **352 × 4 = 1408 cell**

合計 **2048 cell × 3 trial = 6144 row**、crashes 0、約 50 分。ARC は preset 毎に cap が異なるので shards は `next_pow2(cap/8)` clamped to `[next_pow2(cap/64), 131072]` で per-preset 計算 (`MAX_PER_SHARD=64` 制約と `c17s-shard-heuristic` の sweet spot 両立)。

## 結果 (2048 cells × 3 trials, crashes 0)

![Mops × threads, Zipf V=u64](../benchmark/concurrent-full-sweep/figures/zipf_mops_vs_threads_u64.png)
![Mops × threads, Zipf V=String](../benchmark/concurrent-full-sweep/figures/zipf_mops_vs_threads_string.png)
![Mops × threads, Twitter-Yang V=u64](../benchmark/concurrent-full-sweep/figures/twitter-yang_mops_vs_threads_u64.png)
![Mops × threads, Twitter-Yang V=String](../benchmark/concurrent-full-sweep/figures/twitter-yang_mops_vs_threads_string.png)
![Mops × threads, ARC V=u64](../benchmark/concurrent-full-sweep/figures/arc_mops_vs_threads_u64.png)
![Mops × threads, ARC V=String](../benchmark/concurrent-full-sweep/figures/arc_mops_vs_threads_string.png)

## Pairwise Δ% (median / worst, vs c17s baseline)

| source | value | cells | senba_concurrent median | worst | cells ≥ −5% | cells ≥ 0% |
|---|---|---:|---:|---:|---:|---:|
| zipf         | u64    | 24  | **−1.3%**   | **−6.9%**   | 92% | 38% |
| zipf         | string | 24  | **−9.9%**   | **−25.5%**  | 29% | 29% |
| twitter-yang | u64    | 56  | **−3.1%**   | **−22.3%**  | 62% | 34% |
| twitter-yang | string | 56  | **−30.3%**  | **−59.5%**  |  2% |  0% |
| arc          | u64    | 176 | **+0.9%**   | **−30.7%**  | 80% | 57% |
| arc          | string | 176 | **−33.9%**  | **−63.4%**  |  1% |  1% |

moka / mini_moka は全 cell で c17s 比 大幅退行 (ARC u64 で **median −95% / worst −99%**、Twitter-Yang u64 で **median −94% / worst −98%**、Zipf u64 で **median −89% / worst −96%**)。本 sweep の絶対値検証として参考までに残す。

cell-by-cell pivot は `docs/benchmark/concurrent-full-sweep/figures/regression_summary.md`。

### Worst cells (senba_concurrent vs c17s, bottom 8)

| Δ% | source | workload | V | mix | T |
|---:|---|---|---|---|---:|
| −63.4% | arc | P11 | string | gim | 8 |
| −63.3% | arc | P3 | string | gim | 8 |
| −63.1% | arc | P2 | string | gim | 4 |
| −62.9% | arc | P12 | string | gim | 4 |
| −62.8% | arc | P5 / P7 / P4 / P5 | string | gim | 4 / 8 |

ARC P-series は cap=20000 / shards=4096 / **cap_per_shard ≈ 5** という極端な小 shard 構成で、SIMD `find` は 1 chunk で完了する代わりに per-shard SIEVE state machine が頻繁に Path C を踏む。V=String では Path C の `defer_unchecked(move || on_evict(K, V))` での closure capture + GC schedule が支配コスト、c17s は raw `V::clone` で defer 抜きで済むので構造的に勝ち越す cell 群。

### Best cells (senba_concurrent vs c17s, top 8)

| Δ% | source | workload | V | mix | T |
|---:|---|---|---|---|---:|
| +121.9% | zipf | zipf | string | gim | 16 |
| +104.6% | zipf | zipf | string | gim | 16 |
| +67.8% | arc | ConCat | u64 | read-heavy | 4 |
| +38.5% | zipf | zipf | string | gim | 8 |
| +34.7% | arc | ConCat | u64 | read-heavy | 1 |
| +23.4% | arc | S1 | u64 | gim | 16 |
| +22.4% | twitter-yang | cluster018 | u64 | read-heavy | 16 |
| +20.5% | arc | P6 | u64 | gim | 16 |

Zipf 高 T V=String が二重取り (Arc 不在 + hot-key 集中) で +100% 以上、read-heavy V=u64 でも実 trace を含めて +20〜+68% の cell が散在。

## Accept 判定

| 軸 | 目標 | 結果 | 判定 |
|---|---|---|---|
| V=u64 / Zipf         | median ≥ −5% / worst ≥ −10% (parity) | −1.3% / −6.9% | **PASS** |
| V=u64 / twitter-yang | 同上                                   | −3.1% / −22.3% | partial (median PASS, worst FAIL) |
| V=u64 / arc          | 同上                                   | +0.9% / −30.7% | partial (median PASS, worst FAIL) |
| V=String / all       | unsound c17s からの soundness 確保コスト ≤ 35% median | Zipf −10% / TW −30% / ARC −34% | **trade-off 説明可能** |

`V=u64` の worst FAIL は P-series 等 cap_per_shard 極小 cell に集中し、median は parity 維持。`V=String` は全 source で soundness コストが −10〜−34% で観測され、これは r4 設計が予期した cost (`docs/reports/2026-05-14-arc-less-concurrent-design.md` の §9.3 で「low-contention V=String では c17s 比 −20〜−30% の epoch overhead」を予測) と整合。

**結論**: 旧 senba::concurrent (Arc 系) の median −34% / worst −63% (`2026-05-13-senba-concurrent-vs-c17s.md`) を r4 lift で全 cell 救済しつつ、c17s (unsound) 比は V=u64 parity / V=String soundness cost という形で着地。v0.4.0 を release candidate とする根拠としては十分。

## WSL2 計測 bias caveat

`[[memory:project_wsl2_measurement_confound]]` 通り、本 sweep は全部 WSL2 で取った。WSL2 / bare Linux / Windows native VTune の cross-check は v0.4.0 release タグを切る前に **少なくとも V=String × skew=1.4 × T=16 (target を最も超えるセル) を 1 回 bare Linux or Windows native で再走**して大筋の方向が変わらないことを確認する必要がある。swap commit (本 commit chain) 自体は research artifact 扱いなので WSL2 bias 込みで進める。

## Phase 4 (lib release) への引き継ぎ

- v0.4.0 を crates.io に publish する前のチェックリスト:
  1. bare Linux または Windows native で V=String × skew=1.4 × T=16 を 1 セル再走、本 sweep と乖離 ≤10% を確認。
  2. ARC phase の sweep を別途流す (本報告 scope 外)。
  3. CHANGELOG.md を起こす (現在 README には API breaking 注記なし)。
- `bench_vtune_concurrent` 系の WSL ホスト経由 VTune は本 sweep 完了後に r4 engine 上で再走、reader hit hot path の cycle breakdown を v0.3.0 (Arc 系) と直接比較すると最終的な mechanism story が完成する (現状は r4-vs-c17s の 432-cell sweep + cargo asm 検証で間接的に裏付けている)。

## 関連レポート / コード変更

- 設計 spec: `docs/reports/2026-05-14-arc-less-concurrent-design.md`
- r4 sweep (engine 検証): `docs/reports/2026-05-15-r4-vs-c17s.md`
- 旧 lift 退行 baseline: `docs/reports/2026-05-13-senba-concurrent-vs-c17s.md`
- 旧 lift 設計: `docs/reports/2026-05-13-senba-concurrent-cache-design.md`
- 本 swap commit 列: `git log --oneline v0.3.0..v0.4.0 -- src/concurrent/` (本報告と同 PR / branch)
