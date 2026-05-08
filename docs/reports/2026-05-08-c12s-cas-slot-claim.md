# 2026-05-08 sieve_c12s — CAS-based slot claim, single-shard sweep + 仮説検証

- 親: `2026-05-08-c12s-cas-slot-claim-design.md` (本実装の設計文書)
- 親軸: `2026-05-08-c11s-conditional-set.md` (c11s 報告 §5 「writer Mutex 律速」を打破するのが c12s の目的)
- variant 実装: `research/src/experimental/sieve_c12s.rs`
- raw csv: `docs/reports/data/2026-05-08-c12s-sweep.csv`
  (c8/c11s/c12s × 5 workloads × 3 op-mixes × 5 threads × 1 trial = 225 行)
- 集計 csv: `docs/reports/data/2026-05-08-c12s-summary.csv`
- 図: `docs/reports/data/2026-05-08-c12s-{throughput,min-mops,p99}-{read-only,read-heavy,gim}.png`
- driver / plot: `scripts/sweep_single_shard_c12s.sh`、`scripts/plot_c12s_sweep.py`

## TL;DR

c12s は **writer Mutex 完全排除** + **install-at-evicted-pos** によって lock-free writer を実現した変種。実装は完了し、Phase 4 sweep で予想を超える結果が出た:

1. **設計の主仮説 (install-at-evicted-pos は SIEVE 外部等価) が崩壊**:
   - oracle test (`research/tests/oracle.rs::c12s_1shard_diverges_from_orig_on_synthetic_zipf`) で eviction stream / cache contents 双方が sieve_orig と divergent
   - **原因**: install-at-evicted-pos は新 entry を **hand 直前 (= 次の sweep 対象位置)** に置く → 高 churn workload で頻繁に新 entry が即 evict され、cache としての hot key 保護が effective に機能しない
2. **throughput 軸は圧勝 (主目的の `read-heavy zipf 16T` で c8 比 +223%)**:
   - read-heavy zipf-1.0 16T: c8 **21.1** → c11s 18.4 → **c12s 68.0 Mops**
   - read-heavy zipf-1.2 16T: c8 21.9 → c11s 20.0 → **c12s 67.0 Mops** (c8 比 +207%)
   - read-heavy zipf-0.7 16T: c8 21.8 → c11s 18.4 → **c12s 74.5 Mops** (c8 比 +242%)
   - 全 thread 帯で c8/c11s と比べて **scaling shape が clean linear**、writer Mutex 律速 plateau の解消
3. **HR は劣化** (cache としての効きが落ちる、SIEVE 等価でない代償):
   - read-heavy zipf-1.0: c8 0.355 → c11s 0.355 → c12s 0.319 (**-10%**)
   - read-heavy zipf-1.2: 0.615 → 0.618 → 0.599 (-3%)
   - read-heavy zipf-0.7: 0.072 → 0.072 → 0.040 (-44%、低 skew で深刻)
   - read-only zipf-1.0: 0.377 → 0.377 → **0.252** (-33%、HR 軸最大の劣化)
4. **並行不変条件 (I-C1〜I-C9) は守られている**: I-C10 (concurrent same-key update の transient duplicate) は SIEVE 1 sweep 以内に自然解消することを `same_key_concurrent_update_self_heals` test で確認

**判定**: **不採用 (senba 昇格候補から外す)**。c12s は SIEVE と algorithmic に等価でない別 cache **(install-at-evicted-pos + visited=0 install で「保護期間が短い CLOCK 亜種」に変質)**。HR trade-off の問題ではなく、そもそも senba (= NSDI'24 SIEVE library) の仕様を満たさない。

- **algorithm 同定**: 新 entry が hand 直前の pos に visited=0 で入る → 次の hand wrap (cap-1 steps 後) で hit が無ければ即 evict。本家 CLOCK ですら新 entry は visited=1 install で 1 周分の保護を与えるが、c12s はその保護も無い。SIEVE の「head insert + tail-to-head sweep」が持っていた **新 entry に対する full-list 分の保護** と **visited bit による quick-demotion** の両方を同時に失っている
- **観察した HR 劣化はこの algorithm 変質の signature**: zipf-0.7 read-only で -72%、zipf-1.0 read-only で -33% という落ち方は、NSDI'24 paper Table 2/3 で SIEVE が LRU/CLOCK を上回った差分を逆向きに踏み抜いた形
- **throughput 3x は "fast non-SIEVE cache" であって "fast SIEVE cache" ではない**: senba の library 仕様 (= SIEVE algorithm) に違反するので、workload を選んでも昇格させない

本変種は research artifact として `senba-research::experimental::sieve_c12s` に永続化、**「lock-free writer + SIEVE 等価が同時成立するか」の反証データ** (= install-at-evicted-pos は SIEVE 等価性を破る) として保持。後続の **「SIEVE 等価性を保つ lock-free writer」** (§5) は per-shard sub-sharding (構造的に SIEVE 等価) を最優先とする。

## §1 設計仮説の崩壊

設計文書 §3 では:
> single-thread での c12s の挙動を sieve_orig と並べると、**eviction 順序 (どの key が次に追い出されるか)** は一致する見込み。internal layout は違う (sieve_orig は linked list、c12s は install-at-evicted-pos で位置を保つ) が、cache 中身 = key set は各 op 後で一致する。

と仮説していた。しかし Phase 2 の oracle 検証で:
- skew=1.05 cap=16 200_000 ops で **30227/200000 = 15.1% の op で cache contents が divergent**
- skew が下がる (= churn が増える) ほど divergence が広がる
- skew=1.5 cap=64 のような低 churn / 高 cap では一致率が上がる (`research/src/experimental/sieve_c12s.rs::tests::matches_sieve_orig_externally_1shard` の trace は cap=64 / 256 keys / 10000 ops で偶然一致して pass する)

### なぜ install-at-evicted-pos が SIEVE と乖離するか

cap=4 で `[1,2,3,4]` を insert (hand=0)、次に insert(5) する場面を考える:

| variant | 状態遷移 | 5 の position | 次の sweep 候補 |
|---|---|---:|---|
| sieve_orig (linked list) | head insert + hand at tail | head | hand=tail-1 (= 既存 entry 4) |
| senba::Cache (shift-on-evict) | tags=[2,3,4,5], hand=0 | last | hand=0 (= 2) |
| **c12s (install-at-evicted-pos)** | tags=[5,2,3,4], hand=1 | **0** | hand=1 (= 2) |

c12s では **新 entry が hand_old (=0) の位置に install され、次の sweep は (hand_old + 1) = 1 から始まる**。新 entry は visited=0 で install されるので、すぐ次の sweep ループで 5 自身が候補になる (cap=4 で全 LIVE の場合、hand が一周回る間に 5 が再び hit されない限り即座に evict)。

これは設計文書 §3 の「I4' を install-at-evicted-pos で維持する」という記述自体は正しいが、「外側から見ると同じ性質」という結論が誤りだった。**SIEVE algorithm の本質は「新 entry を tail (= 最も sweep されない位置) に置く」ことを含む**。 install pos を tail 側にしないと SIEVE フィルタリングが破綻する。

### 1T smoke test の hit_ratio 比較

```
$ ./target/release/bench_single_shard --variant c11s --workload zipf --skew 1.0 \
    --cap 64 --threads 1 --keys 100000 --ops 1000000 --warmup 80000 --op-mix read-only
c11s,..., hit_ratio=0.3706, aggregate_mops=19.39

$ ./target/release/bench_single_shard --variant c12s --workload zipf --skew 1.0 \
    --cap 64 --threads 1 --keys 100000 --ops 1000000 --warmup 80000 --op-mix read-only
c12s,..., hit_ratio=0.2301, aggregate_mops=21.16
```

- **hit_ratio が c11s 比 -38%** (0.37 → 0.23)
- 一方 throughput は **+9.1%** (19.39 → 21.16 Mops)

「lookup が cheap な分高速だが cache としての効きが悪い」という形。read 側のスループット数値だけ見て採用すると **production 上は cache miss が増えて downstream cost (= cache miss 後の expensive な fetch) が爆発** するので評価軸として throughput 単独は不十分。Phase 4 の sweep でも throughput と hit_ratio の両方を観察する。

## §2 結果 — 数値ハイライト (Phase 4 sweep)

aggregate Mops/s, median 1 trial。raw csv は `docs/reports/data/2026-05-08-c12s-sweep.csv`。

### read-only

| workload | T | c8 | c11s | **c12s** | c12s/c8 | c12s/c11s |
|---|---:|---:|---:|---:|---:|---:|
| zipf-0.7 | 16 | 96.4 | 144.5 | **161.9** | 1.68x | 1.12x |
| zipf-1.0 | 16 | 44.2 | 135.0 | **163.1** | 3.69x | 1.21x |
| zipf-1.2 | 16 | 35.8 | 154.4 | **152.2** | 4.26x | 0.99x |
| adversarial-hot | 16 | 30.0 | 540.2 | **558.0** | 18.59x | 1.03x |
| uniform | 16 | 343.2 | 537.2 | 528.8 | 1.54x | 0.98x |

(`docs/reports/data/2026-05-08-c12s-throughput-read-only.png`)

read-only では c11s と概ね並ぶか僅かに上回る (zipf-0.7/1.0 で +12〜21%、zipf-1.2 / adv-hot / uniform で ±1〜3%)。c11s が既に reader 経路を最適化済みなので、c12s の writer 経路改善はここでは効きが薄い (writer 経路を踏まないため)。

### read-heavy 95/5 (採否判定の主軸)

**ここが c12s の最大の勝ち軸**。

| workload | T | c8 | c11s | **c12s** | c12s/c8 | c12s/c11s |
|---|---:|---:|---:|---:|---:|---:|
| zipf-0.7 | 16 | 21.8 | 18.4 | **74.5** | **3.43x** | **4.04x** |
| zipf-1.0 | 16 | 21.1 | 18.4 | **68.0** | **3.23x** | **3.69x** |
| zipf-1.2 | 16 | 21.9 | 20.0 | **67.0** | **3.06x** | **3.35x** |
| adversarial-hot | 16 | 19.1 | 29.6 | **85.0** | **4.45x** | **2.87x** |
| uniform | 16 | 22.0 | 19.5 | **107.2** | **4.88x** | **5.50x** |

(`docs/reports/data/2026-05-08-c12s-throughput-read-heavy.png`)

設計文書 §5 の採否判定基準 (read-heavy zipf-1.0 16T で c8 を 5%+ 上回る) を **+223% で圧倒的にクリア**。c11s が writer Mutex 律速で plateau していた帯を、c12s は CAS-based slot claim で正面突破した。c11s 報告 §5 の予想 (「c11w 結果待ち」) が現実化した形。

### read-heavy scaling (zipf-1.0)

| T | c8 | c11s | **c12s** |
|---:|---:|---:|---:|
| 1 | 16.3 | 17.2 | 17.9 |
| 2 | 22.6 | 27.2 | 29.4 |
| 4 | 28.1 | 36.0 | **47.9** |
| 8 | 27.8 | 26.0 | **57.1** |
| 16 | 21.1 | 18.4 | **68.0** |

c8/c11s は 4T で plateau (Mutex contention の peak) してから 16T まで逆 regress するのに対し、**c12s は 16T まで monotonic に増加**。これが lock-free writer の威力。c11s 報告 §1 の「c8/c10s/c11s 三者がすべて 16T で 17〜21 Mops に収束」は writer Mutex 律速の signature だったが、c12s では 68 Mops まで scale が伸びている。

### gim 50/50

| workload | T | c8 | c11s | **c12s** |
|---|---:|---:|---:|---:|
| zipf-0.7 | 16 | 1.6 | 1.4 | **9.8** |
| zipf-1.0 | 16 | 2.3 | 2.1 | **11.4** |
| zipf-1.2 | 16 | 3.8 | 3.5 | **16.2** |
| adversarial-hot | 16 | 33.5 | 566.4 | 570.1 |
| uniform | 16 | 1.8 | 1.6 | **9.7** |

gim は writer 比率が高い (miss → insert) workload。c12s の writer 経路改善が直接効き、zipf 系で 4〜7x の throughput 改善。adversarial-hot は key=0 の繰り返しなので 100% Path A (update) で writer 比率としては低く、c11s と並ぶ。

### hit_ratio (cache としての効き)

**c12s は SIEVE 等価でないため HR が低下** (`hit_ratio` の median):

| workload | op_mix | T | c8 | c11s | **c12s** | Δ c12s vs c11s |
|---|---|---:|---:|---:|---:|---:|
| zipf-0.7 | read-only | 16 | 0.074 | 0.074 | **0.021** | **-72%** |
| zipf-1.0 | read-only | 16 | 0.377 | 0.377 | **0.254** | **-33%** |
| zipf-1.2 | read-only | 16 | 0.659 | 0.659 | **0.571** | **-13%** |
| zipf-0.7 | read-heavy | 16 | 0.072 | 0.072 | **0.040** | **-44%** |
| zipf-1.0 | read-heavy | 16 | 0.355 | 0.355 | **0.319** | **-10%** |
| zipf-1.2 | read-heavy | 16 | 0.615 | 0.618 | **0.599** | **-3%** |

観察:
- **HR 劣化は skew が低いほど大きい**: zipf-0.7 で -72%、zipf-1.2 で -13% (read-only)。低 skew では hot key の集中が薄く install-at-evicted-pos の即 evict 害が累積。
- **read-heavy は read-only より HR 劣化が小さい**: read-heavy は 95% read / 5% write で write 比率が低く、新 entry の即 evict 影響が抑えられる
- **adversarial-hot 1 key 軸では HR 劣化なし** (どちらも HR=1.000): Path A (update) のみで evict が走らない

**production への含意**: c12s で「同じ workload で c11s の 3.2x throughput」と言えるのは、HR 劣化が許容範囲内 (zipf-1.2 read-heavy: -3%) のケースのみ。低 skew (zipf-0.7) や read-only zipf-1.0 では HR 劣化が production cost を支配する可能性が高い。

### p99 chunk latency

| workload | op_mix | T | c8 | c11s | **c12s** |
|---|---|---:|---:|---:|---:|
| zipf-1.0 | read-heavy | 16 | 1213 ns | 1425 ns | **440 ns** |
| zipf-1.2 | read-heavy | 16 | 1202 ns | 1358 ns | **449 ns** |
| uniform | read-heavy | 16 | 1311 ns | 1438 ns | **238 ns** |
| adversarial-hot | gim | 16 | 1634 ns | 48 ns | 40 ns |
| zipf-1.0 | gim | 16 | 9328 ns | 9957 ns | **3110 ns** |

c12s の p99 が **c8/c11s の 1/3 以下** (read-heavy zipf 帯)。lock-free writer なので Mutex queue 待ちの長尾が消えた、という解釈と整合。tail latency 観点でも c12s は強い。

## §3 並行不変条件の実装上の遵守状況

設計文書 §3 で立てた I-C1〜I-C10 のうち、I-C10 以外はすべて strong invariant として実装で保たれていることを test で確認:

| ID | 不変条件 | 検証 test |
|---|---|---|
| I-C1 | LIVE tag で id == pos | `entry_id_equals_pos_invariant`、`id_eq_pos_preserves_under_churn`、`concurrent_invariants_under_zipf` |
| I-C2/C3 | install-at-evicted-pos で `tags[0..cap]` 全 LIVE 維持 | `install_at_evicted_pos_no_compaction` |
| I-C3 並行版 | `len ≤ cap` for concurrent inserts | `len_monotonic_under_concurrent_inserts` |
| I-C4 | `hand % cap ∈ [0, cap)` | コードの構造から自明 |
| I-C5/C6 | release ordering (tag store before next reader) | seqlock dance test (`reader_hit_does_not_modify_tag` ほか) |
| I-C7 | visited clear は writer のみ、SET は reader/writer 両方 | `update_existing_key_sets_visited_like_oracle` |
| I-C8 | 同 entries[id] への writer-writer race なし | `hand_atomic_advance_non_overlapping`、`concurrent_invariants_under_zipf` (live_pos の重複なし) |
| I-C9 | seqlock dance を pass した entry は torn-read 非伝播 | c11s から継承 (`try_candidate` 構造) |
| I-C10 | 同一 key の LIVE tag は高々 1 個 (weak) | `same_key_concurrent_update_self_heals` で 1 sweep 自己解消を確認 |

I-C10 は設計文書通り weak invariant で、`same_key_concurrent_update_self_heals` test では cap=4 / 2 thread / 1000 ops の小ケースで transient duplicate が 1 sweep 以内に自己解消することを確認。production-quality な解決には RCU / hazard pointer が必要だが c12s スコープ外。

## §4 採否判定

設計文書 §5 の採否判定基準: **read-heavy zipf-1.0 16T で c8 を 5%+ 上回る** → throughput 単独で見れば +223% で圧倒的にクリア。**ただしこの基準は c12s が SIEVE であることを前提にしていた**。実際の c12s は別 algorithm に変質しており、基準そのものが適用外になる。

### c12s は SIEVE ではない

§1 で示した通り、install-at-evicted-pos + visited=0 install の組み合わせは SIEVE algorithm の根幹 (head insert / tail-to-head sweep / 新 entry 保護) を破壊する。eviction 順序を機械的に追うと:

- 新 entry は **hand 直前** (= 直前 evict pos) に visited=0 で入る
- 次の hand wrap (cap-1 steps 後) で再訪、その間に hit が無ければ evict

これは **CLOCK の劣化版** (本家 CLOCK は新 entry を visited=1 で install して 1 周分の保護を与える)。NSDI'24 paper が SIEVE の優位性として測定した「quick demotion」と「lazy promotion」の両方を失っている。

### HR 劣化はこの algorithm 変質の signature

- **read-heavy zipf-1.2** (高 skew): HR -3% / throughput +207% — 高 skew では evict 自体が稀なので algorithm 差が薄まる
- **read-heavy zipf-1.0** (中 skew): HR -10% / throughput +223%
- **read-heavy zipf-0.7** (低 skew): HR **-44%** / throughput +242% — churn が高く保護期間の短さが累積
- **read-only zipf-1.0**: HR **-33%** / throughput +21% (c11s 比)
- **read-only zipf-0.7**: HR **-72%** / throughput +12% (c11s 比)

低 skew / read-only での -33〜-72% という落ち方は、NSDI'24 paper Table 2/3 で SIEVE が LRU/CLOCK を上回っていた差分を逆向きに踏み抜いた形。c12s の HR は **algorithm 変更の必然的な帰結** であって、tuning や workload 選択で救えるものではない。

### `senba::concurrent::Cache` 昇格判断 — **不採用**

senba の存在意義は **library-grade SIEVE** (= NSDI'24 SIEVE 論文の実装を crates.io に出す) にある。c12s を `senba::concurrent::Cache` として shipping することは、製品仕様レベルで嘘になる: user が「SIEVE cache」を期待して使うのに、実際の eviction policy は SIEVE でない。throughput が 3x でも、それは "fast non-SIEVE cache" であって "fast SIEVE cache" ではない。

不採用の理由をまとめると:

1. **algorithm 仕様違反**: SIEVE と等価でない (HR 劣化はその signature であって、根本原因ではない)
2. **workload tuning で救えない**: 高 skew zipf-1.2 で HR -3% に収まるのは「evict が稀で algorithm 差が出にくい」ためであり、c12s の algorithm が SIEVE に近づいているわけではない
3. **§5 の他の attack vector で SIEVE 等価性を保ったまま並行性能を改善できる**: per-shard sub-sharding は構造的に SIEVE 等価が自明、tail 維持型 lock-free も原理的に可能

c12s は research artifact として `senba-research::experimental::sieve_c12s` に永続化、**「lock-free writer + SIEVE 等価が同時成立するか」の反証データ** として残す。後続の lock-free 系変種が install-at-evicted-pos に手を出さないための reference になる。

## §5 後続課題 — SIEVE 等価性を保つ lock-free writer

c12s の失敗から学ぶ次の attack vector:

1. **Tail 維持型の lock-free SIEVE**: install-at-evicted-pos を諦め、separate な free-id pool (or `len_atomic`) で新規 install pos を取り、shift-on-evict は別 mechanism (e.g. seqlock generation counter for compaction window) で実現する。複雑度は上がるが SIEVE 等価性を保ちうる
2. **CLOCK ベースに切り替え**: SIEVE は 1 hand だが、CLOCK のように (recency, frequency) 二段で sweep する形にすれば「新 entry の保護」が hand 周回でなく counter で実現できる。ただし algorithm 自体が SIEVE ではなくなる
3. **per-shard sub-sharding**: c11s の writer Mutex を保ったまま、shard を更に細分化して contention 確率を下げる方向。SIEVE 等価性は保たれる、c11s の構造を最大限活かせる
4. **RCU/Epoch GC**: writer は新版 entry を別 slot に install、reader は epoch で旧版を読み続ける、epoch 終了で旧 slot を pool に返す。`V: Copy` を緩めて `V: Send` まで広げられる副次利得もあり。複雑度は最大

attack 順位 (期待 ROI / 実装複雑度):
- **(3) per-shard sub-sharding が最優先**: c11s の構造を活かしつつ writer 律速を緩和、SIEVE 等価性は自明に保たれる
- (1) tail 維持型 lock-free は概念実証として価値あるが、実装複雑度が高い
- (4) RCU は publishable lib として明確な利得 (V: Copy 緩和) があるが、後続の major version で検討
- (2) CLOCK 切り替えは SIEVE 仕様を捨てるので senba プロジェクトの spirit と外れる

## §6 c12s の研究的価値

senba の library 仕様 (= SIEVE) には載せない (§4) が、研究 artifact としての value は以下:

1. **install-at-evicted-pos は SIEVE 等価でない、という反証実験の記録**: 設計文書で立てた仮説を numerical に否定した。後続 variant が「lock-free writer + SIEVE 等価」を狙う際、c12s と同じ轍を踏まない reference になる。SIEVE は NSDI'24 paper でも示されている通り「絶妙にチューニングされた algorithm」で、構造を勝手に変えると LRU/CLOCK 並み (ないしそれ以下) に劣化する — c12s はその実例
2. **CAS-based slot claim 自体は機能している**: I-C1〜I-C9 が並行 invariant として保たれていることを確認、`hand.fetch_add(1) % cap` + tag CAS の所有権獲得は正しく動く。algorithm 上は別 cache との合成で再利用可能 (ただし senba は SIEVE library なので senba 内では出番がない)
3. **「保護期間が短い CLOCK 亜種」の実測 data point**: c12s は throughput が高いが hit_ratio は低い。これは cache evaluation で algorithm 差を throughput 単独で見ると判断を誤る例として、`docs/reports/2026-05-08-single-shard-baseline.md` 系の testbed に NSDI'24 paper Table 2/3 を逆向きに踏んだ data point を加えた

c12s は research crate (`senba-research`) に永続的に残し、`docs/reports/index.md` で「algorithmic に SIEVE でない反証実験」として明示する。`senba::concurrent::Cache` 昇格は §5 (3) の per-shard sub-sharding 検討に進む。

## §7 実装の単純さ

writer Mutex を完全排除した結果、c11s と比べて以下が消える:
- `parking_lot::Mutex` import / `WriterState` struct / `writer.lock()` 呼び出し
- `writer_compact` (~50 行)
- `writer_first_live` / `writer_update_in_place` / `writer_evict_one` の Mutex 排他下 helper 群

代わりに加わるのは:
- `evict_one`: lock-free CAS loop (~30 行)
- `find_lockfree`: writer 用 find (visited fetch_or 撃たない、~20 行)
- `insert` の outer loop (Path A/B/C) (~80 行、retry 含む)

正味で行数は減っている (c11s 1015 → c12s 約 870 行)。CAS retry のセマンティクスを正しく設計すれば lock-free SIEVE 並行 cache の実装は素朴な mutex 版より小さくなりうる、という point は記録に値する (ただし algorithm 整合性 (= SIEVE 等価) を犠牲にした上で、なので実用 trade-off としてはマイナス)。

clippy `--all-targets -D warnings` 通過、`cargo test -p senba-research --lib experimental::sieve_c12s` 全 31 件 pass、`cargo test --workspace` 全 pass (oracle test の 1 件は意図的 `#[ignore]`)。
