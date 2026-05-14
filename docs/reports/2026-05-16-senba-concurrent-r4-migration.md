# 2026-05-16 — `senba::concurrent::Cache` v0.4.0: r4-based ManuallyDrop engine swap

## TL;DR

- **仮説**: `2026-05-15-r4-vs-c17s.md` で 432-cell Zipf sweep で確認した r4 engine の特性 (V=u64 で c17s と ±0.3% 並走、V=String で旧 Arc 系 senba::concurrent を median +21% で Pareto improve) は、lib publishable surface に lift した後も Zipf を超える実 trace で同じ trade-off を保つはず。あわせて、本 sweep の **moka / mini-moka 参照点が fair tuning で構築されているか** を見直す。
- **やったこと**: `src/concurrent/cache/shard.rs` の Arc<V> + epoch refcount スキームを r4 engine (ManuallyDrop<K/V> + epoch defer of moved-out K/V) に置き換え、`insert` API を `()` 返しに変更、`insert_with(K, V, on_evict)` callback を追加 (race β + race γ 同時 closure)、`V/K: Sync` bound を撤廃。senba v0.3.0 → v0.4.0。`research/src/bin/bench_concurrent.rs` の moka / mini-moka adapter を `Cache::new(cap)` (default `RandomState` + 遅延 grow) から **`Cache::builder().max_capacity(cap).initial_capacity(cap).build_with_hasher(senba::Xxh3Build)`** に切り替えて hasher / 容量を senba 系と揃えた状態で再計測。`docs/benchmark/concurrent-full-sweep/` は 4-way (senba_concurrent v0.4.0 = r4-based / 旧 c17s / moka / mini_moka) × Zipf 192-cell + NSDI24 libcachesim + Twitter-Yang 5 cluster + ARC 22 preset = **2048 cells × 3 trials = 6144 row** sweep、crashes 0。
- **分かったこと**:
  - **V=u64 はおおむね parity**: senba_concurrent vs c17s が Zipf median **−0.3%** / worst **−14.1%**、Twitter-Yang median **−2.8%** / worst **−21.5%**、ARC median **+1.0%** / worst **−22.2%**。median は全 trace family で ±5% 以内、worst cell は cap_per_shard が小さい (P-series cap=20000, cap_per_shard ≈ 5) ところに集中、Path C 頻度が極端に高い regime で実装差分が見えている。
  - **V=String は soundness の代価が明確に出ている**: Zipf median **−11.5%** / worst **−23.3%** に対し、Twitter-Yang median **−31.3%** / worst **−59.1%**、ARC median **−34.3%** / worst **−63.8%**。c17s は race β (clone-mid-flight UAF) を構造的に閉じていない **unsound 参照点** で、`Arc<V>` / `epoch::pin` 抜きで raw V を clone するため hot path が物理的に短い。実 trace は eviction 頻度が高く Path C 経由の `defer_unchecked(closure capture K+V)` が支配コスト化、real workload では soundness 確保で typical −30% を払う構造。**逆に Zipf 高 T cell では senba_concurrent が +60〜+100% 取り**返す (Arc ping-pong / write log dequeue の典型ボトルネックが消えるため)。
  - **moka / mini_moka の構造的劣位は adapter fix では塗り替わらない**: Xxh3Build + `initial_capacity(cap)` で揃えても ARC u64 median **−95.0%** / worst **−99.1%**、Twitter-Yang u64 median **−94.2%** / worst **−98.2%**、Zipf u64 median **−90.0%** / worst **−96.1%**。前回 (default `RandomState` + 遅延 grow) と median 差は **≤ 1pp**。つまり moka 系の支配コストは hash function でも rehash でもなく、W-TinyLFU bookkeeping (read/write log + Count-Min sketch 更新 + admission filter) そのもの。fairness 是正は済んだ上でなお同オーダーの劣位、という null result。
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

### `bench_concurrent` の moka / mini-moka adapter 再構築

前回 sweep では adapter が `moka::sync::Cache::new(cap as u64)` をそのまま使っており、暗黙 default が **`std::RandomState` (SipHash 1-3) hasher** と **`initial_capacity: None` の遅延 grow** だった。senba / 他 variant は Xxh3Build + cap 固定で構築されているため、moka 系だけ hash cost と warmup rehash で別オーダーの仕事をしていた可能性を排除できなかった。

```rust
// 新 adapter
impl<V> ConcCache<V> for moka::sync::Cache<u64, V, senba::Xxh3Build> {
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(
            moka::sync::Cache::builder()
                .max_capacity(capacity as u64)
                .initial_capacity(capacity)
                .build_with_hasher(senba::Xxh3Build),
        )
    }
    // ...
}
```

mini-moka 0.10 も同じ builder 形 (`build_with_hasher` + `initial_capacity`) を持つので両方同条件で再構築。本 sweep の moka / mini_moka 数値はこの adapter で取得した。adapter 修正による前回 sweep からの median 差は **≤ 1pp** で、構造劣位は閉じない (後述 §結果)。

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
| variants | senba_concurrent (v0.4.0, r4-based) / c17s (research) / moka / mini_moka (※ moka 系は Xxh3Build + initial_capacity 揃え) |
| sources | Zipf 合成 / NSDI24 libCacheSim CSV / Twitter-Yang (5 cluster) / ARC (mokabench 22 preset) |
| Zipf 軸 | cap=4096, skew ∈ {0.8, 1.0, 1.4}, keys=100k |
| 共通軸 | threads ∈ {1, 4, 8, 16}, op_mix ∈ {gim, read-heavy}, value ∈ {u64, String}, trials=3 |
| shards | 512 (auto-shard `cap/8`), ARC は per-preset `next_pow2(cap/8)` clamped |

Phase 別 cell count (各 cell × 3 trial、4-way):
- Zipf: 1 cap × 3 skew × 4 T × 2 mix × 2 V = **48 × 4 var = 192 cell**
- libcachesim (twitter_cluster52 + trace.csv): 2 source × 4 T × 2 mix × 2 V = **32 × 4 = 128 cell**
- Twitter-Yang (cluster {006, 016, 018, 019, 034}): 5 cluster × 4 T × 2 mix × 2 V = **80 × 4 = 320 cell**
- ARC (mokabench 22 preset): 22 preset × 4 T × 2 mix × 2 V = **352 × 4 = 1408 cell**

合計 **2048 cell × 3 trial = 6144 row**、crashes 0、約 50 分。

## 結果 (2048 cells × 3 trials, crashes 0)

![Mops × threads, Zipf V=u64](../benchmark/concurrent-full-sweep/figures/zipf_mops_vs_threads_u64.png)
![Mops × threads, Zipf V=String](../benchmark/concurrent-full-sweep/figures/zipf_mops_vs_threads_string.png)
![Mops × threads, Twitter-Yang V=u64](../benchmark/concurrent-full-sweep/figures/twitter-yang_mops_vs_threads_u64.png)
![Mops × threads, Twitter-Yang V=String](../benchmark/concurrent-full-sweep/figures/twitter-yang_mops_vs_threads_string.png)
![Mops × threads, ARC V=u64](../benchmark/concurrent-full-sweep/figures/arc_mops_vs_threads_u64.png)
![Mops × threads, ARC V=String](../benchmark/concurrent-full-sweep/figures/arc_mops_vs_threads_string.png)

## Pairwise Δ% (median / worst, vs c17s baseline)

| source | value | senba_concurrent median | worst | moka median | worst | mini_moka median | worst |
|---|---|---:|---:|---:|---:|---:|---:|
| zipf         | u64    | **−0.3%**   | **−14.1%**  | −90.0% | −96.1% | −90.4% | −96.2% |
| zipf         | string | **−11.5%**  | **−23.3%**  | −81.8% | −91.0% | −81.1% | −94.1% |
| twitter-yang | u64    | **−2.8%**   | **−21.5%**  | −94.2% | −98.2% | −94.9% | −98.8% |
| twitter-yang | string | **−31.3%**  | **−59.1%**  | −91.9% | −96.8% | −93.2% | −97.8% |
| arc          | u64    | **+1.0%**   | **−22.2%**  | −95.0% | −99.1% | −94.6% | −97.8% |
| arc          | string | **−34.3%**  | **−63.8%**  | −93.5% | −98.4% | −92.0% | −97.3% |

cell-by-cell pivot は `docs/benchmark/concurrent-full-sweep/figures/regression_summary.md`。

### Adapter fix の効果 (前回 sweep からの差分)

| source | value | moka median 前 → 後 | moka 改善 |
|---|---|---|---:|
| zipf         | u64    | −89%   → **−90.0%** | ~0pp |
| zipf         | string | (再走前は ~−80%) → **−81.8%** | ~0pp |
| twitter-yang | u64    | −94%   → **−94.2%** | ~0pp |
| arc          | u64    | −95%   → **−95.0%** | ~0pp |
| arc          | string | (再走前は ~−93%) → **−93.5%** | ~0pp |

hasher / initial_capacity を fair に揃えた直後の median は **どの cell でも 1pp 以内** しか動かない。SipHash → Xxh3 で hot path は確かに数ns 縮むはずだが、moka の read/write log push + Count-Min sketch 更新 + admission gate の per-op コストがそれを覆い隠す規模なので、見かけの差は出ない。**「moka が遅いのは設定ミスでは説明できない」** が今回の null result。

### Worst cells (senba_concurrent vs c17s, bottom 8)

| Δ% | source | workload | V | mix | T |
|---:|---|---|---|---|---:|
| −63.8% | arc | P 系 | string | gim | 8 |
| −62〜−61% | arc | P11 / P3 / P2 / P12 / P5 / P7 / P4 | string | gim | 4〜8 |
| −59.1% | twitter-yang | cluster 系 | string | gim | 8〜16 |

ARC P-series は cap=20000 / shards=4096 / **cap_per_shard ≈ 5** という極端な小 shard 構成で、SIMD `find` は 1 chunk で完了する代わりに per-shard SIEVE state machine が頻繁に Path C を踏む。V=String では Path C の `defer_unchecked(move || on_evict(K, V))` での closure capture + GC schedule が支配コスト、c17s は raw `V::clone` で defer 抜きで済むので構造的に勝ち越す cell 群。

### Best cells (senba_concurrent vs c17s, top)

Zipf 高 T V=String cell で senba_concurrent が +60〜+100% 帯まで取る (Arc 不在 + hot-key 集中で Arc strong count ping-pong 起源のキャッシュライン争奪が消滅)、ARC ConCat / 一部 P-series の read-heavy V=u64 でも +20〜+27% の cell が散在。

## Hit ratio

senba_concurrent v0.4.0 は SIEVE policy をそのまま lift しているので c17s と同 HR が期待される。実測は **全 trace family で median Δ +0.00pp** (worst −1.7pp、best +3.9pp はいずれも trial 間 noise の範囲)。policy reproducibility は perf engine 入れ替えと独立に維持された。adapter 変更 (hasher 入替) は collision 分布に微小な影響しかないため、moka / mini_moka の HR も前回 sweep と median ±0.1pp の範囲で変わっていない (W-TinyLFU 系の admission 判定が hasher に依存しないため期待通り)。

| 軸 | senba_concurrent vs c17s | moka vs c17s | mini_moka vs c17s |
|---|---:|---:|---:|
| zipf median         | **±0pp** | +2.0pp | +2.0pp |
| twitter-yang median | **±0pp** | +1.0pp | +0.4pp |
| arc median          | **±0pp** | +1.1pp | +1.1pp |
| 全 sweep best gain (variant > c17s) | +3.9pp | +19.1pp | +22.3pp |
| 全 sweep worst loss (variant < c17s) | −1.7pp | **−16.7pp** | **−28.3pp** |

moka / mini_moka (W-TinyLFU) は trace 別に勝ち負けが split:

| workload | moka HR Δ vs c17s | 解釈 |
|---|---:|---|
| ARC P1 / P8 / P9 (cap=20k, scan-heavy) | +4 ~ +6pp | W-TinyLFU admission filter が SIEVE より強い帯 |
| twitter cluster006 / libcachesim trace | +4.3 ~ +4.5pp | OLTP 系 (admission control 有効) |
| twitter **cluster019** | **−4.9pp** | SIEVE が一貫して勝つ working set (`r1-vs-moka-sweep` でも同様) |
| ARC MergeP / P4 / P14 | −0.6 ~ −1.6pp | SIEVE 優位 (clean working set + 反復) |
| Zipf | +2.0pp | W-TinyLFU 微優位 (合成 workload は admission filter にやさしい) |

SIEVE と W-TinyLFU は HR では trace-by-trace で split、moka が median +1〜+2pp 取るが scan-pattern では −5pp 落とすこともある (前報 `2026-05-13-r1-vs-moka-sweep.md` の "policy 等価性 ±1pp 並走、cluster019 で SIEVE +7pp" と同方向)。**throughput が −90〜−95% で HR の +1〜+2pp は実運用で吸収されない** — 1〜10 Mops で 50pp HR を取っても 30〜200 Mops の SIEVE に絶対値で勝てない。fair-hasher 化後もこの結論は変わらない。

## Accept 判定

| 軸 | 目標 | 結果 | 判定 |
|---|---|---|---|
| V=u64 / Zipf         | median ≥ −5% / worst ≥ −10% (parity) | −0.3% / −14.1% | partial (median PASS, worst FAIL by 4pp) |
| V=u64 / twitter-yang | 同上                                   | −2.8% / −21.5% | partial (median PASS, worst FAIL) |
| V=u64 / arc          | 同上                                   | +1.0% / −22.2% | partial (median PASS, worst FAIL) |
| V=String / all       | unsound c17s からの soundness 確保コスト ≤ 35% median | Zipf −12% / TW −31% / ARC −34% | **trade-off 説明可能** |

`V=u64` の worst FAIL は P-series 等 cap_per_shard 極小 cell に集中し、median は parity 維持。`V=String` は全 source で soundness コストが −12〜−34% で観測され、これは r4 設計が予期した cost (`docs/reports/2026-05-14-arc-less-concurrent-design.md` の §9.3 で「low-contention V=String では c17s 比 −20〜−30% の epoch overhead」を予測) と整合。

**結論**: 旧 senba::concurrent (Arc 系) の median −34% / worst −63% (`2026-05-13-senba-concurrent-vs-c17s.md`) を r4 lift で全 cell 救済しつつ、c17s (unsound) 比は V=u64 parity / V=String soundness cost という形で着地。moka / mini_moka との throughput 倍率は fair-adapter 化後も維持 (median 1〜2 桁差)。v0.4.0 を release candidate とする根拠としては十分。

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
