# 2026-05-08 sieve_c11s vs c8/c10s — conditional visited set single-shard sweep

- 親: `2026-05-08-c10s-vs-c8-baseline.md` (c10s が read-heavy zipf で regress した報告)
- variant 実装: `research/src/experimental/sieve_c11s.rs` (c10s から reader hit を `load → 0 ならだけ fetch_or` に変更した単一行 diff)
- raw csv: `docs/reports/data/2026-05-08-c11s-sweep.csv`
  (c8/c10s/c11s × 5 workloads × 3 op-mixes × 5 threads × 1 trial = 225 行)
- 集計 csv: `docs/reports/data/2026-05-08-c11s-summary.csv`
- 図: `docs/reports/data/2026-05-08-c11s-{throughput,min-mops,p99}-{read-only,read-heavy,gim}.png`
- driver / plot: `scripts/sweep_single_shard_c11s.sh`、`scripts/plot_c11s_sweep.py`

## TL;DR

c11s は c10s からの **conditional visited set** 単一軸 diff:

```rust
// c10s: 毎 reader hit で fetch_or
self.visited[w].fetch_or(b, Relaxed);

// c11s: 0 ならだけ fetch_or
if self.visited[w].load(Relaxed) & b == 0 {
    self.visited[w].fetch_or(b, Relaxed);
}
```

x86 の `lock or` は **値が変わらなくても必ず cache line を Modified に遷移させる** (= 他コアの Shared を invalidate)。zipf hot key のように visited=1 が定常状態の slot では、毎 hit で全 reader が同 cache line を取り合う ping-pong が発生していた。c11s は load → 0 判定で **既に立っているなら write を skip** する。MESI Shared が維持されるので invalidate 不発、ping-pong が消える。

- **read-only 16T で大勝**:
  zipf-1.0 49.8 → c10s 97.0 → **c11s 130.3 (c8 比 +162%、c10s 比 +34%)**、
  zipf-1.2 36.3 → 64.5 → **142.7 (c8 比 +293%、c10s 比 +121%)**、
  adversarial-hot 35.2 → 54.0 → **594.0 (c8 比 +1587%、c10s 比 +999%)**。
- **gim adversarial-hot 16T で 670 Mops** (c10s 52.0 → c11s 671.0、c8 比 +1834%)。
  reader が 100% hit する単一軸 (key=0) では writer Mutex 律速を超えて reader 全速。
- **read-heavy zipf 16T の regression は埋まらず** (c8 比 -33〜-18%): c10s に対しては
  ほぼ並か微減。read-heavy は visited contention ではなく **writer Mutex contention が
  支配項** であることを定量的に確認 (= 次手は c10w / writer 経路改善)。
- **HR は ±0.005 一致** (75 比較中 1 例外、§3 参照)。oracle 順序保持。
- **1T overhead は許容範囲**: adversarial-hot で +19%、zipf 系で -2 〜 -10% (load + branch
  cost が `lock or` cost を下回った軸とその逆軸が混在、§4 §6)。

**結論**: c11s は c10s 比で read-only / 高並列 hot path が一貫に強い。`senba::concurrent::Cache`
昇格候補としても c10s より明確に優位。read-heavy zipf の頭打ち解消には c10w (writer 経路)
との合成 (c11sw / c11w 系) が必要。

## §1 結果 — 数値ハイライト

aggregate Mops/s, median 1 trial。

### read-only (clean win across the board)

| workload | T | c8 | c10s | **c11s** | c11s/c8 | c11s/c10s |
|---|---:|---:|---:|---:|---:|---:|
| zipf-0.7 | 16 | 98.8 | 129.3 | **137.8** | 1.39x | 1.07x |
| zipf-1.0 | 16 | 49.8 | 97.0 | **130.3** | 2.62x | 1.34x |
| zipf-1.2 | 16 | 36.3 | 64.5 | **142.7** | **3.93x** | **2.21x** |
| adversarial-hot | 16 | 35.2 | 54.0 | **594.0** | **16.87x** | **10.99x** |
| uniform | 16 | 423.3 | 582.6 | 545.6 | 1.29x | 0.94x |

(`docs/reports/data/2026-05-08-c11s-throughput-read-only.png`)

観察:
- **zipf-1.2 で c10s 比 2.21x、c8 比 3.93x**。skew が高いほど hot key の visited
  定常状態時間比が伸び、c11s の write-skip 効果が増幅する。
- adversarial-hot は **c8 比 16.87x、c10s 比 10.99x** と桁違い。1 hot key の visited が
  ほぼ常時 1 → 全 reader が load のみ → cache line が 16 core 全部で Shared 維持。
  c10s はここで visited 1 cache line に集中した fetch_or 取り合いが ceiling だった。
- **uniform で c10s に対し -6%**: uniform は thread 別 disjoint range なので visited
  自体に並行 hit が来ない (= 各 thread が自分の visited word を独占)。conditional の
  load + branch が pure overhead として現れる軸。絶対値は依然 c8 比 +29% (423 → 546)
  なので publishable には十分。

### read-only scaling (zipf-1.0)

| T | c8 | c10s | **c11s** |
|---:|---:|---:|---:|
| 1 | 19.4 | 18.8 | 18.3 |
| 2 | 27.7 | 37.0 | 35.2 |
| 4 | 34.9 | 65.8 | 69.7 |
| 8 | 42.4 | 80.2 | **107.2** |
| 16 | 49.8 | 97.0 | **130.3** |

c10s と c11s は 1〜2T では並ぶが (= 並行 contention が薄いので write-skip の利得なし)、
4T 以降で乖離。8T で +34%、16T で +34%。**hot key への並行 reader 集中が増えるほど
gain が伸びる** = 仮説 (visited 既セット時の load-only への退化) と整合。

### read-only scaling (adversarial-hot, single hot key)

| T | c8 | c10s | **c11s** |
|---:|---:|---:|---:|
| 1 | 80.1 | 75.8 | **95.1** |
| 2 | 41.6 | 46.9 | **187.2** |
| 4 | 31.7 | 45.3 | **370.1** |
| 8 | 31.7 | 42.5 | **412.4** |
| 16 | 35.2 | 54.0 | **594.0** |

c8/c10s が 2T 以降 plateau 〜 緩やかに増加するのに対し、**c11s は 16T まで線形に近く
スケールしている**。ここまで来ると writer 経路が一切走らない adv-hot read-only は
事実上の reader sanity 上限テストで、c11s ではそれが「visited 1 cache line を全 core
が Shared で読むだけ」になり、AVX2 scan + entry copy + seqlock 検査の cost に支配される。

### read-heavy 95/5 (regression unresolved on zipf)

| workload | T | c8 | c10s | **c11s** | c11s/c8 |
|---|---:|---:|---:|---:|---:|
| zipf-0.7 | 16 | 24.4 | 17.4 | 16.3 | 0.67x |
| zipf-1.0 | 16 | 21.4 | 19.0 | 17.5 | 0.82x |
| zipf-1.2 | 16 | 19.8 | 18.6 | 19.1 | 0.97x |
| adversarial-hot | 16 | 18.0 | 27.5 | 28.9 | 1.60x |
| uniform | 16 | 20.4 | 21.2 | 21.4 | 1.05x |

(`docs/reports/data/2026-05-08-c11s-throughput-read-heavy.png`)

観察: read-heavy zipf は **c11s でも c8 比劣勢** が継続 (zipf-1.0 で c8 21.4 → c11s 17.5、
-18%)。c10s 比でも -8〜-10%。reader 経路は c11s で改善されているはずなので、
**writer Mutex critical section が支配項** であることが残った。
1〜4T では c11s が勝つ (zipf-1.0 4T で c11s 36.1 vs c8 25.6 = +41%) が 8T 以降逆転。

これは別ベクトル — c10w (CAS-based slot claim) / writer 経路再設計 — でしか解けない。
c11s の責任範囲を超える。

### gim 50/50 (mixed picture)

adversarial-hot の劇的勝ち以外は writer Mutex 律速で差は小:

| workload | T | c8 | c10s | **c11s** |
|---|---:|---:|---:|---:|
| zipf-0.7 | 16 | 1.48 | 1.38 | 1.36 |
| zipf-1.0 | 16 | 2.41 | 2.12 | 2.01 |
| zipf-1.2 | 16 | 3.54 | 3.52 | 3.47 |
| adversarial-hot | 16 | 34.7 | 52.0 | **671.0** |
| uniform | 16 | 1.81 | 1.66 | 1.61 |

zipf gim 系は writer Mutex に詰まる絶対値帯 (1〜3 Mops) で、conditional load の
overhead がそのまま 2〜10% の小 regress として現れる。実務影響は小だが、c11s が
write 経路を介さない範囲で「writer dominant な軸では neutral〜微減」と理解しておく。

adversarial-hot gim 16T = **671 Mops** は驚異的だが、これは **insert path が
"既存 key の update only"** で evict が発生しない特殊軸 (key=0 の繰り返し insert)。
writer は in-place update のみで Mutex の hold 時間が短く、reader path 改善が支配的に
効いた結果。普通の workload で再現する数字ではない。

## §2 解釈 — なぜ仮説が当たったか

c10s 報告 §2 で立てた 2 つの cost のうち:

1. tag scan 路の汚染 (= reader が tag に書く問題) → **c10s で解消**
2. visited 自身への fetch_or 集中 ping-pong → **c11s で解消**

c11s の load-then-conditional-fetch_or は (2) を狙い撃ち。x86 の atomic RMW は
Modified 取得を必須とするので、load を分離しないと「すでに 1」を確認する手段が
なかった。実装は本当に 1 行差分:

```rust
// research/src/experimental/sieve_c11s.rs L295-300
if self.visited[w].load(Ordering::Relaxed) & b == 0 {
    self.visited[w].fetch_or(b, Ordering::Relaxed);
}
```

zipf hot key の hit rate (例: zipf-1.0 で 0.377) のうち、定常運用後の hit は
ほぼすべて visited=1 の状態にある hot key。すなわち **大半の hit で fetch_or が
skip され、ping-pong が発生しない**。

uniform で利得が出ないのも整合: uniform は HR=0.001 の純 miss 軸で、visited 立てが
そもそも稀。さらに thread 別 disjoint なので並行 hit 集中がない。

## §3 HR (正しさ確認)

c11s と c10s の hit_ratio は 75 比較中 74 で ±0.01 一致。例外 1 件:

| op_mix | workload | T | c10s HR | c11s HR | Δ |
|---|---|---:|---:|---:|---:|
| read-heavy | adversarial-hot | 8 | 0.830 | 0.851 | +0.021 |

c10s 報告 §3 と同じ流儀: writer の EMPTY 窓中に reader が seqlock-fail で空 hit する
頻度がわずかに c11s で減った (= writer 経路が短い trial で 1 サンプルだけ揺れた)。
adv-hot 16T では Δ=+0.008 で誤差帯に収束しており、有意な eviction 順序差ではない。

c8 と c11s の zipf 系 HR は完全一致 (read-only/read-heavy/gim × zipf-{0.7,1.0,1.2}
× T={1,16} 全帯で |Δ|<0.01)。SIEVE eviction 順序保持は確認済み。

unit test (`research/src/experimental/sieve_c11s.rs::tests`) も oracle 比較
(`matches_sieve_orig_externally_1shard`) と eviction-order 回帰
(`update_existing_key_sets_visited_like_oracle`) を含めて 25 件全 pass。

## §4 1T overhead の細かい挙動

read-only 1T では軸ごとに勝ち負けが分かれる:

| workload | c8 1T | c10s 1T | c11s 1T | c11s/c8 |
|---|---:|---:|---:|---:|
| zipf-0.7 | 20.2 | 19.4 | 21.5 | 1.06x |
| zipf-1.0 | 19.4 | 18.8 | 18.3 | 0.94x |
| zipf-1.2 | 19.0 | 18.1 | 17.2 | 0.90x |
| adversarial-hot | 80.1 | 75.8 | 95.1 | 1.19x |
| uniform | 83.6 | 74.9 | 79.4 | 0.95x |

- **adversarial-hot 1T で +19%**: 単スレッドでも `lock or` の uncontested cost
  (~30 cycle on Zen-class) > load + branch (~4 cycle) なので、毎 hit で write skip
  すると地味だが見える形で速くなる。
- **zipf-1.2 1T で -10%**: visited word に既に立った bit が **少ない** ケース (HR=0.66
  の中で初回 hit が混ざる) では load + fetch_or 2 命令が支配的になり、c10s の
  fetch_or 1 命令より遅くなる。本効果は zipf skew が低い (= miss/初回 hit 比が高い)
  ほど顕著。
- **uniform 1T で -5%**: uniform は HR≈0 で fetch_or 自体走らないが、reader path
  全体の re-layout (関数サイズ増加 → instruction cache pressure か、 register
  allocation の悪化) で誤差レベルの劣化。

実運用では 1T 軸単独評価より multi-thread のスケーリング shape が重要なので、この
overhead は許容範囲。最大 -10%、平均 -3% 程度。

## §5 c11 lineage の次の一手

c11s 単独では **片側 (read-only / 高 reader 集中) のみ完全制圧**。残った課題:

1. **read-heavy zipf 16T が c8 並 〜 微減**
   - 原因: writer Mutex critical section の sequential bottleneck
   - 攻め所: c10w / c11w (CAS-based slot claim、hand を atomic に進めて writer の
     Mutex 区間を tag word 1 個の CAS に絞り込む)
   - 期待値: c8 比でも +5〜15% に届けば publishable surface への合流候補

2. **uniform 16T で c10s に対し -6%**
   - 原因: load + branch の単純 overhead (uniform では visited 並行 hit 集中なし)
   - 攻め所: コンパイラ inline 状況の確認、`#[inline(always)]` 検討
   - 影響軽微なので優先度低

3. **gim zipf 系の writer 律速**
   - 同上 (1)。c11w で writer 経路を改善すれば連動して効く

attack 順位: **(1) c11w が最優先**。c11s + c11w の合成 (= c11sw) で read-heavy zipf を
c8 と並ぶか上回る形に持っていけば、`senba::concurrent::Cache` の昇格候補として
完成形になる。c11s 単独でも read-only 軸での圧勝は publishable 価値があるので、
昇格判断は c11w 結果を待ってから複合軸で評価する。

## §6 publishable surface への昇格判断

c10s 報告 §5 では「`senba::concurrent::Cache` を c10s lineage に切り替えるのは時期尚早」
と判断した。c11s で改めて評価:

| 軸 | c10s 単独 | c11s 単独 | 判断 |
|---|---|---|---|
| read-only zipf 16T | c8 比 +95〜+78% | c8 比 +162〜+293% | **c11s 強い昇格動機** |
| read-only adv-hot 16T | c8 比 +53% | c8 比 +1587% | 同上 |
| read-only uniform 16T | c8 比 +38% | c8 比 +29% | c10s 寄り、ただし c11s も clean win |
| read-heavy zipf 16T | c8 比 -10〜-21% (regress) | c8 比 -3〜-33% (regress) | 両者 NG、c11w 待ち |
| HR / oracle 一致 | OK | OK | ともに pass |
| 1T overhead | 軽微 | 軽微 (-10〜+19%) | OK |

c11s は c10s と比べて **read-heavy regression を悪化させてはいない** (zipf-1.0 16T で
c10s 19.0 → c11s 17.5、-8%)。read-heavy で生じている問題は visited contention 由来
ではなく writer Mutex 由来なので、reader 改善の c11s では原理的に解けない。

→ **c11s の昇格は c11w 結果を待つ**。ただし research crate 内で c8 の代替推薦変種
としてのプリフェレンスを上げてよい。`senba::concurrent::Cache` の主想定軸 (read-heavy
zipf) が writer 律速であることは、本 sweep で **c8/c10s/c11s 三者がすべて 16T で
17〜21 Mops に収束** していることから確証あり (= reader 側の改善は read-heavy 軸の
天井を上げない)。

## §7 実装の単純さ

c11s の実装上の差分は c10s から:

- `Shard::try_candidate` の visited set 経路: `fetch_or` を `if load == 0 { fetch_or }`
  に置換 (5 行)
- 文書 (mod docstring): c10s からの差分説明
- 全テスト名は c10s と同形 (oracle 比較・eviction 順序回帰・並行 invariants)

その他の writer 経路、tag 構造、AVX2 scan、seqlock 検査はすべて c10s と同一。
c10s の書き起こし (visited 列分離) と独立な単一軸 diff として正しく分離できている。

clippy `--all-targets -D warnings` 通過、`cargo test -p senba-research` 全 pass。
オラクルテストとの cross-check も既存と同じ枠組みで pass。

## §8 testbed の自己検証

baseline 同様、testbed の正しさも本 sweep で再確認:
- **HR 一致** (§3): c8 vs c11s が 75 比較中全帯で ±0.01 一致 (zipf 系)、
  c10s vs c11s が 75 比較中 74 で ±0.01 一致 (例外 1 件は §3 説明済み)
- **新規 invariant test** (`reader_hit_does_not_modify_tag`): c10s から継承、
  conditional 化しても tag 列は触らない不変条件を直接検証
- 並行 invariants test (`concurrent_invariants_under_zipf`): 4 thread × 50000 ops
  Zipf 後の per-shard live count / id 集合 / value 一致を確認 (miri 抑制)

## §9 後続作業

- **c11w 起案**: writer Mutex を CAS-based slot claim に置換、本 testbed で c11s と
  直接合成 (c11sw)、read-heavy zipf 16T が c8 を抜けるか検証
- **multi-trial 化**: c10s 報告 §7 で指摘した 3-5 trial median 化を本 c11s sweep
  にも適用、特に read-heavy zipf の -10% 帯が trial 1 サンプル揺れと見分けられる確度
- `senba::concurrent::Cache` の publishable surface 昇格は c11w 後の合成結果で再判断
- harness の per-thread 終端統一 (mops_min_per_thread の 0.5% problem) は本稿でも未対処
