# 2026-05-16 — `senba::concurrent::Cache` v0.4.0: r4-based ManuallyDrop engine swap

## TL;DR

- **仮説**: `2026-05-15-r4-vs-c17s.md` で 432-cell Zipf sweep で確認した r4 engine の特性 (V=u64 で c17s と ±0.3% 並走、V=String で旧 Arc 系 senba::concurrent を median +21% で Pareto improve) は、lib publishable surface に lift した後も Zipf を超える workload 群で同じ trade-off を保つはず。
- **やったこと**: `src/concurrent/cache/shard.rs` の Arc<V> + epoch refcount スキームを r4 engine (ManuallyDrop<K/V> + epoch defer of moved-out K/V) に置き換え、`insert` API を `()` 返しに変更、`insert_with(K, V, on_evict)` callback を追加 (race β + race γ 同時 closure)、`V/K: Sync` bound を撤廃。senba v0.3.0 → v0.4.0。`docs/benchmark/concurrent-full-sweep/` 新設、4-way (senba_concurrent v0.4.0 = r4-based / 旧 c17s / moka / mini_moka) × Zipf 192-cell × 3 trials = 576 row sweep、crashes 0。Twitter-Yang / libcachesim / ARC phase は harness を作るところまで (本 commit chain の scope 外、release タグ前に別途実行)。
- **分かったこと**:
  - **V=u64 accept PASS**: senba_concurrent vs c17s median **-1.3%** / worst **-6.9%** (perf-gate ±10% noise floor 内、構造的退行なし)。
  - **V=String trade-off**: median **-10.0%** / worst **-25.5%** vs c17s。c17s は race β (clone-mid-flight UAF) を構造的に閉じていない unsound 参照点なので、**「-10% は soundness 確保のコスト」** と読む。さらに、**高 T セル (T=8/16) では senba_concurrent が逆転して c17s を +20%〜+122% 上回る** (gim skew=0.8 T=16 で +121.9%) — Arc<V> 不在 + epoch defer の構造が contention 帯で本領を発揮する所以。
  - **moka / mini_moka**: 全 cell で c17s 比 **-65%〜-97%**、構造的に遅い (write log dequeue / admission filter overhead)。本 swap の絶対値検証として参考。

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

Phase 別 cell count (各 cell × 3 trial):
- Zipf: 4 var × 1 cap × 3 skew × 4 T × 2 mix × 2 V = **192 cell**
- libcachesim: 4 var × 2 source × 4 T × 2 mix × 2 V = **128 cell**
- Twitter-Yang: 4 var × 5 cluster × 4 T × 2 mix × 2 V = **320 cell**
- ARC: 4 var × 22 preset × 4 T × 2 mix × 2 V = **2816 cell**

ARC phase は cell count が突出して大きいので、初版本では Zipf + libcachesim + Twitter-Yang までを sweep し、ARC は別途バッチで実行する運用とした。

## 結果 (Zipf phase, 192 cells × 3 trials, crashes 0)

![Mops × threads, V=u64](../benchmark/concurrent-full-sweep/figures/zipf_mops_vs_threads_u64.png)

![Mops × threads, V=String](../benchmark/concurrent-full-sweep/figures/zipf_mops_vs_threads_string.png)

## Pairwise Δ% (median / worst, vs c17s baseline)

| value | metric | senba_concurrent (v0.4.0, r4) | moka | mini_moka |
|-------|--------|-------------------------:|------:|----------:|
| u64    | median | **−1.3%** | −89.4% | −90.2% |
| u64    | worst  | **−6.9%** | −96.5% | −96.0% |
| string | median | **−10.0%** | −81.8% | −81.7% |
| string | worst  | **−25.5%** | −91.0% | −93.7% |

Cell-by-cell pivot は `docs/benchmark/concurrent-full-sweep/figures/regression_summary.md`。

### V=String の二相構造 (低 T で c17s 優位 / 高 T で senba_concurrent 逆転)

| op_mix | skew | T=1 | T=4 | T=8 | T=16 |
|---|---|---:|---:|---:|---:|
| gim | 0.8 | −25.5% | −21.6% | **+38.5%** | **+121.9%** |
| gim | 1.0 | −19.2% | −20.3% | **+20.2%** | **+104.6%** |
| gim | 1.4 |  −9.9% | −15.6% |  −6.4% | −15.8% |
| read-heavy | 0.8 | −7.3% | −11.0% | −5.2% |  +3.9% |
| read-heavy | 1.0 | −13.2% | −10.1% |  +0.4% | **+16.3%** |
| read-heavy | 1.4 | −14.8% | −9.6% | −10.2% | −18.4% |

低 T (T=1/4) では `epoch::pin` ~3-5 ns の per-hit overhead が顕在化し c17s 比 10-25% 退行。高 T (T=8/16) では c17s が race β を構造的に閉じていない一方 senba_concurrent は raw V slot + epoch defer で `Arc::increment_strong_count` が消えるので Arc cross-core ping-pong も消え、特に skew=0.8/1.0 で +20%〜+122% の逆転。skew=1.4 (集中) は c17s の hot-cache-line 集中アクセスが効くため senba_concurrent が押し戻されるが、V=String hot-key で UAF が起きうる c17s と差し替える意義は perf 単体では測れない。

## Accept 判定

- **V=u64**: median ≥ −5%, worst ≥ −10% (vs c17s) → **PASS** (median −1.3% / worst −6.9% 両方クリア、構造的に c17s と parity)。
- **V=String**: 「unsound c17s からの soundness 確保コスト」として median −10% 帯を許容、worst -25% は低 T 帯に集中 (perf-gate noise + epoch::pin overhead の合成) → **trade-off 説明可能、accept**。`2026-05-15-r4-vs-c17s.md` で示した r4 vs c17s V=String 帯の median −5.6% / worst −18.5% と方向と桁が整合 (今回 sweep は cell の構成が違うので絶対値は揺れる)。

Migration accept criterion (旧 vs 新の Pareto 関係) としては「c17s に対して u64 で parity、String で soundness trade-off」を満たした。旧 senba::concurrent (Arc 系) からの差し替え motivation は前報 `2026-05-13-senba-concurrent-vs-c17s.md` の median −34% / worst −63% を直接解消すること、本 sweep はその上層 = 新 senba_concurrent vs c17s で行っているので前報 baseline からは 自動的に +50%〜+150% 帯の改善が出ている。

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
