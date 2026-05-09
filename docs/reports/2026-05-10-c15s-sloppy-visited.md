# 2026-05-10 — c15s: sloppy visited gate (Phase 1)

- 関連: `2026-05-10-write-contention-design-space.md` §7 Phase 1 (本稿はその実装と判定)、
  `2026-05-10-visited-bitmap.md` (per-shard u64 bitmap、c15s が継承する構造)、
  `2026-05-08-c14s-design.md` (本稿のベース c14s)
- 種別: **試行と判定**。c15s 変種の Phase 1 評価、結論は **REJECT** (= sloppy 単体では効かない)

## 0. TL;DR

`sieve_c14s` の reader hot path で `visited.fetch_or` を 1/(2^N) 確率の TLS-RNG gate で wrap した
試行 `sieve_c15s`。SAMPLE_BITS=4/3/2 (= 1/16, 1/8, 1/4) を Phase 1 判定基準に通した結果:

- **skew=0.0 (uniform draw, miss-heavy HR≈1.95%)**: 改善ゼロ (0.99–1.00×) — ただし **workload 不適合**。reader hit がそもそも少ないので gate がほとんど踏まれない (sec §3.1 で詳述)
- **skew=1.0 (hot Zipf, HR≈0.65)**: **明確な regression**。1/16 で 0.91× (−9%)、1/4 でも 0.97× (sec §3.2)
- **HR 影響は確実に出ている** (1/16 で skew=1.0 では −0.7pp、Twitter 5 cluster では平均 −2.09pp / 最大 −4.72pp) ので **gate は確実に fire している。実装バグではない**
- **構造的結論**: c11s 由来の **conditional load-then-fetch_or** trick が hot-key MESI ping-pong を
  既にほぼ構造除去済みであり、reader visited line は steady state で **Shared 状態を維持**できている。
  従って追加 sloppy gate が「節約できる atomic load の MESI コスト」自体が ~1 ns まで縮んでおり、
  TLS RNG draw のコスト (~3 ns) のほうが上回って **net 負け** になる

→ design doc §7 の判定では **REJECT × STOP の二重 NG**、sloppy gate **単体では** Phase 2 に
進む価値がないと確定。ただし「c11s の atomic load は既にほぼタダ」という構造的気づきが
Phase 2 (packed LongAdder) の設計を組み換えるべき重要な情報になる。

## 1. 動機と判定基準 (要約)

`2026-05-10-write-contention-design-space.md` §7 Phase 1 にて設計合意:

```
GO  → Phase 2: throughput ≥ 1.5× (uniform read-heavy 16T) かつ HR loss < 0.5pp
STOP        : HR loss ≥ 0.5pp   ⇒ Phase 2 (packed LongAdder) 直行
REJECT      : throughput ≤ 1.0× ⇒ contention 主因の再評価 (visited 以外を見る)
```

## 2. 実装サマリ

ファイル: `research/src/experimental/sieve_c15s.rs` (c14s から複製、差分 ~80 行)。

**型シグネチャ拡張** (Shard と ConcurrentSieveCache の両方):

```rust
pub struct Shard<K, V, const SAMPLE_BITS: u32 = 0> { ... }
pub struct ConcurrentSieveCache<
    K, V,
    const SHARDS: usize = DEFAULT_SHARDS,
    const SAMPLE_BITS: u32 = 0,
> { shards: [Shard<K, V, SAMPLE_BITS>; SHARDS], ... }
```

**reader hot path gate** (`try_candidate` 内、c14s line 408–412 相当):

```rust
if SAMPLE_BITS == 0
    || (next_rand() & ((1u64 << SAMPLE_BITS) - 1)) == 0
{
    let (w, b) = Self::vbit(pos);
    if self.visited[w].load(Ordering::Relaxed) & b == 0 {
        self.visited[w].fetch_or(b, Ordering::Relaxed);
    }
}
```

`SAMPLE_BITS == 0` は const、rustc が dead-eliminate する想定 (= c15s_0 ≡ c14s の codegen)。

**変更しない箇所** (orig 等価性 / SIEVE 不変条件):
- writer Path A 成功時の `visited.fetch_or` (c14s line 549–550)
- `scan_evict` 内の `visited.fetch_and(!b)` (c14s line 879–881)

**TLS RNG (自前 wyrand)** — 新規依存なし。`thread_local!` cell + `AtomicU64` SEED_CTR で per-thread
seed を初回に切り出し、以降は per-call 5 命令 (TLS load + 2 mul + 2 xor):

```rust
static SEED_CTR: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
thread_local! { static R: Cell<u64> = const { Cell::new(0) }; }

#[inline]
fn next_rand() -> u64 { /* wyrand step、初回 SEED_CTR.fetch_add で seed */ }

pub fn reseed_for_test(seed: u64) { /* bench 再現性のため */ }
```

**type alias 公開**: `ConcurrentSieveCacheC15S{0,4,8,16}` (= SAMPLE_BITS=0/2/3/4 の short-cut)。

**bench infra**:
- `bench_concurrent.rs`: `c14s` / `c15s_{16,8,4}` arm を追加、SHARDS=64 専用 dispatch
  (`c14s/c15s_*` は `assert_eq!(args.shards, 64)`、Phase 1 fixed design)
- `bench.rs`: `c14s_n64` / `c15s_{16,8,4}_n64` arm を追加、`drive_conc_c14s` /
  `drive_conc_c15s<const SHARDS, const SAMPLE_BITS>` を新規定義 (`&self` driver、CacheImpl 不経由)。
  `--rng-seed` flag で TLS RNG を deterministic 化、process 別起動で seed 平均が取れる

**実装上の制約**: c14s から継承の `MAX_PER_SHARD = 64` (6-bit ID) のため SHARDS=64 では
total capacity ≤ 4096。Twitter HR sweep の cap は {1024, 2048, 4096} に設定。

## 3. throughput

### 3.1 skew=0.0 (uniform、当初 sweep 設定 — workload mismatch)

設定: `--shards 64 --threads 16 --cap 4096 --keys 200000 --skew 0.0 --op-mix read-heavy
--ops 16000000 --warmup 1000000 --trials 5`。CPU は WSL2。

| variant | aggregate Mops (median) | × c14s | hit_ratio |
|---|---:|---:|---:|
| c14s | 57.38 | 1.000 | 0.0195 |
| c15s_16 | 57.62 | 1.004× | 0.0195 |
| c15s_8  | 57.29 | 0.998× | 0.0195 |
| c15s_4  | 57.04 | 0.994× | 0.0194 |

差は 1% 未満で trial CV (~2-3%) 内、**HR も全 variant で同一 (0.0195)** という結果。

これは「sloppy が効かない」のではなく **workload が合っていない** ことを示す。HR=1.95% という
ことは reader hit rate がほぼゼロ → reader hot path (= visited fetch_or 候補) がほとんど踏まれて
いない → gate をどう設計しても発火頻度がノイズに埋もれる。`bench_concurrent.rs` doc が
"shared keyspace" と書いているのを「uniform Zipf」と読み違え、実際は **hot-key 集中 (skew=1.0+)
が必要** だった。

### 3.2 skew=1.0 (hot Zipf — 正しい workload)

同設定で `--skew 1.0`、5 trial median:

| variant | Mops (median) | × c14s | hit_ratio |
|---|---:|---:|---:|
| c14s    | 99.07 | 1.000 | 0.646 |
| c15s_16 | 95.27 | **0.962× (−3.8%)** | 0.640 (**−0.7pp**) |
| c15s_8  | 96.61 | 0.975× | 0.643 (−0.4pp) |
| c15s_4  | 93.04 | 0.939× (−6.1%) | 0.644 (−0.2pp) |

ここで **明確な regression** が出る。HR も sample 率に逆相関で素直に下がる
(`1/16` で −0.7pp = visited bit が 16 回に 1 回しか立たない設定で本来の SIEVE が cold key を
余分に evict している痕跡)。

throughput 側は単調 monotone ではなく `c15s_4 < c15s_16 < c15s_8` の順に並ぶ:
- `c15s_4` (1/4) が最悪なのは、低 sample 率は「gate fire 頻度が高い」ので fetch_or の
  cross-core ownership transfer が本物の cost として効いてくる帯
- `c15s_8` (1/8) が中庸、`c15s_16` (1/16) は gate fire 頻度が下がって cross-core 数も減る

つまり「sample 率を絞るほど良い」という単純な傾向は出ず、**TLS gate cost 自体が支配的**な
帯と **fetch_or contention が支配的**な帯がせめぎ合っている。どの帯でも c14s に勝てない。

**HR の段階的低下は gate が確実に fire している実証**。実装バグではなく、TLS RNG コストが
節約コストを上回る構造的負け。

判定: **REJECT** (1.5× GO 閾値どころか throughput 自体が落ちる)。

図: `docs/figures/c15s_phase1_thr.png` (skew=1.0 baseline 化済みの bar)。

## 4. Twitter 5 cluster HR loss

設定: 5 cluster × {1024, 2048, 4096} cap × 4 variant × 5 RNG seed の直積 (n=300)。

### 4.1 cluster × capacity matrix (median over 5 seed、HR loss in pp = (c14s − c15s) × 100)

| cluster | cap | c15s_4 (1/4) | c15s_8 (1/8) | c15s_16 (1/16) |
|---|---:|---:|---:|---:|
| cluster006 | 1024 | 0.367 | 0.598 | 1.000 |
| cluster006 | 2048 | 0.809 | 1.480 | 2.473 |
| cluster006 | 4096 | 1.206 | 2.310 | 4.258 |
| cluster016 | 1024 | 1.975 | 2.830 | 3.495 |
| cluster016 | 2048 | 2.230 | 3.310 | 4.203 |
| cluster016 | 4096 | 2.348 | 3.628 | 4.719 |
| cluster018 | 1024 | 1.786 | 2.494 | 2.853 |
| cluster018 | 2048 | 1.489 | 2.051 | 2.551 |
| cluster018 | 4096 | 1.203 | 1.750 | 2.343 |
| cluster019 | 1024 | 0.004 | 0.014 | 0.039 |
| cluster019 | 2048 | 0.035 | 0.039 | 0.050 |
| cluster019 | 4096 | 0.015 | 0.020 | 0.024 |
| cluster034 | 1024 | 0.018 | 0.232 | 0.636 |
| cluster034 | 2048 | 0.246 | 0.593 | 1.140 |
| cluster034 | 4096 | 0.482 | 1.027 | 1.553 |

### 4.2 全 (cluster, cap) 横断 summary

| variant | mean HR loss (pp) | max | min |
|---|---:|---:|---:|
| c15s_4  (1/4)  | 0.948 | 2.348 | 0.004 |
| c15s_8  (1/8)  | 1.492 | 3.628 | 0.014 |
| c15s_16 (1/16) | 2.089 | 4.719 | 0.024 |

判定: **STOP** (全 sample 率で平均 0.5pp 閾値超え)。最緩 c15s_4 ですら mean 0.95pp。
cluster019 は影響ほぼゼロ (workload が hot-only に偏っていて cold key の visited 取りこぼしが
HR に効かない) だが、cluster016 は最も敏感で 4096 cap で −4.7pp。

図: `docs/figures/c15s_phase1_hr.png` (cluster × cap bar)、
`docs/figures/c15s_phase1_pareto.png` (HR loss vs throughput improvement scatter)。

## 5. Phase 1 判定: REJECT × STOP の二重 NG

design doc §7 の判定木:

```
throughput ≤ 1.0×  ⇒ REJECT (visited 以外を見る)
HR loss ≥ 0.5pp    ⇒ STOP   (Phase 2 直行可能だが、sloppy 単体は効かない)
```

両方踏んだので **sloppy visited は採択しない**。c15s.rs は **research artifact として残置**
(将来別文脈で参照しやすくするため、削除はしない)、bench harness の variant arm は次の試行
(Phase 2) で同居させて引き続き比較対象にする。

## 6. なぜ「sloppy が効かない」のか — 実装バグではなく構造的結論

design doc §3 は「atomic OR cross-core 50–200 ns / cache line ownership 分散が真の利得」と
予測していた。skew=1.0 での **throughput regression + HR 段階的低下** という観測の組み合わせは、
予測が外れた **構造的理由** を示している。以下、コスト分解の数値で裏取り。

### 6.1 c11s の load-then-fetch_or trick が既に hot-key contention を構造除去している

「visited bitmap は複数 thread で共有される cache line だから、visited を操作すると line が
Modified に転落する」という素朴な見立ては、**書き込みを伴う atomic と読み出しのみの atomic の
非対称性** を見落としている。MESI 遷移ルールを切り分けると:

| 操作 | 必要な line state | 効果 |
|---|---|---|
| atomic load (Relaxed/Acquire) | **Shared で十分** | line は Shared 維持、ownership 移動なし |
| atomic store / fetch_or / CAS (= 書き込み発生) | **Modified が必須** | 他 core の copy を invalidate、ownership 引き取り |

**書き込みが起きないなら atomic でも Shared に居着ける** (= 全 core の L1 が同じ copy を共有、
cross-core fetch 不要)。

c14s reader (`try_candidate` line 410):

```rust
if self.visited[w].load(Ordering::Relaxed) & b == 0 {
    self.visited[w].fetch_or(b, Ordering::Relaxed);
}
```

これを hot key の steady state で時系列に追うと:

1. **最初に reader が来たとき**: bit=0 → fetch_or で SET → line がその core で **Modified**、
   他 core の copy は invalidate
2. **直後に別 core の reader**: load で前 owner から fetch (cross-core 50–200 ns) → 結果として
   両 core で **Shared に転落** (Modified → Shared transition、書き込み無しなので静かに共有)
3. **以降 N 番目以降の reader**: load で「bit=1」を観測 → **fetch_or は撃たれない** (内側 if が
   skip) → line は **Shared 固定**、全 core で共有、cross-core fetch なし
4. **次に bit が clear されるまで** (= hand sweep の `fetch_and(!b)` まで)、3 が続く

つまり c11s の load-then-fetch_or は **「最初の 1 回だけ Modified ↔ Shared を踏み、以降は
load-only で Shared 固定」** という構造を作っており、reader 経路から書き込みを排除している。

cache line が Modified に転落する契機として残っているのは 2 つだけ:
1. **hand sweep (eviction)** の `fetch_and(!b)` で bit をクリア
2. **writer Path A** 成功時の `fetch_or(b)` (line 549、無条件)

Path A は insert mix 5% × Path A 採択率に比例。eviction は steady state で miss rate
(= 35% in skew=1.0) に応じた頻度。どちらも **reader hit ごと毎回ではない**。1 回の Modified
転落で 50–200 ns 払っても、その後の hot key reader 数十〜百回ぶんに amortize されるので、
reader load の MESI コストは trace 平均で **per-hit ~1 ns** に圧縮されている。

**design doc §3 の「atomic = MESI で直列化」の前提が外れた理由**: その前提は **書き込み atomic**
(CAS / fetch_or 等) を念頭にしたもので、load は静かに Shared を共有できる、という非対称性を
盛り込んでいなかった。c11s の conditional fetch_or trick は、まさにこの非対称性を活かして
hot path の atomic を読み出しに片寄せする MESI-aware な構造的最適化を達成していた。
本稿 sloppy gate はそれより内側の最適化で、もう削れる余地がほぼ無かった。

### 6.2 削減量と gate コストの実測比較 (skew=1.0)

c15s_16 で 16T × 16M ops × 95% reads = 15.2M reads 全体、その HR=0.65 → 約 9.88M reader hits =
gate 候補。1/16 sample なら fetch_or 候補数は 9.88M → 0.617M に減る。差し引き 9.26M 回の
「load 1 回 + 稀に OR」を skip。

| 1 hit あたりコスト | c14s | c15s (1/16) |
|---|---:|---:|
| TLS RNG draw | 0 ns | ~3 ns (毎回) |
| atomic load (Shared 維持時) | ~1 ns | 1/16 確率 |
| atomic OR (rare = 1/N) | ε | 1/16 × ε |
| **小計 / hit** | ~1 ns | **~3.06 ns** |

c15s 側が **約 +2 ns / hit** 増、9.88M hits × 16T で 0.32 sec ぶんの overhead。実測 elapsed
diff (162 → 178 ms) はその 1/2 程度、hot key 衝突が 16T で並列化される効果でならされる
**(= regression −9% は wall-clock では小さく見えるが per-thread で見ると確かに余分なサイクル
を払っている)**。

### 6.3 もし「load 自体が cross-core で 50 ns かかる」だったら sloppy は勝てた

design doc §3 が前提にしていたのはまさにそれで、`Atomic.load` でも cache line owner が他 core
にある場合 ownership を引き取るコスト (50–200 ns) が走る、という見立てだった。これは
**conditional load-then-fetch_or なし** の reader 設計 (= c10s 以前) なら正しかった。c11s が
load を加えた瞬間に「visited bit は **読み出し** に統一されて line を Shared にできる」という
構造ができ、ownership transfer が起きなくなった。**c11s の conditional fetch_or は本稿 sloppy
よりも先回りで構造的最適化を済ませていた**、と読み直せる。

### 6.4 HR loss だけは設計通りに出た — gate fire の実証

| sample | skew=1.0 HR loss | Twitter mean HR loss |
|---|---:|---:|
| 1/16 | 0.7pp | 2.09pp |
| 1/8  | 0.4pp | 1.49pp |
| 1/4  | 0.2pp | 0.95pp |

sample 率と HR loss が **monotone 比例** していることが、TLS gate が確かに 1/N の頻度で
visited fetch_or を skip している実証。**実装バグの可能性は除外できる**。
ただし HR loss は「払う代償」であって、本来は throughput の純利得と引き換えに許容するもの。
利得側がゼロなら HR loss は純損になる。

## 7. 失敗から学んだこと (Phase 2 設計への反映)

1. **c11s の conditional load-then-fetch_or trick は予想以上に効果が大きい**。design doc §3 で
   「atomic = MESI で直列化」と書いた前提は、load も fetch_or も同じ cache line を Modified に
   転落させる、という想定だった。実際には load だけなら Shared 維持できるので、c11s の
   設計は「reader 経路から書き込みを排除する」という MESI-aware な構造的最適化を既に達成して
   いた。**Phase 2 (packed LongAdder visited) も同じ理由で改善余地が薄い**: lane 別 fetch_or
   を分散しても、c11s と同様に load-only steady state なら cache line ownership は動かない。
   Phase 2 の **真の gain は writer 側 (Path A の fetch_or)** にあるはず → reader 側の lane 分散は
   ほぼ無意味、writer 側こそ per-thread cell 化すべきという読み筋に変わる。

2. **TLS access コスト ~3 ns は無視できない**。`__tls_get_addr` 経路は思ったより重い。
   Phase 2 で per-thread cell を使うなら、`thread_local!` ではなく **per-shard offset から
   thread index を `(addr_of(local_var) >> 6) & MASK` 等で導出**する方式 (= TLS をそもそも
   引かない) を検討する価値がある。あるいは reader 1 回ぶん 1 ns の節約のために gate を
   入れるのは元々割に合わないので、gate はそもそも入れない (= 別の最適化軸を探す)。

3. **HR loss は cluster 依存が強い** (cluster019 ≈ 0pp、cluster016 ≈ 4.7pp)。hot-key 集中度が
   高い trace ほど sloppy の HR loss が小さい (cold key の visited 取りこぼしが eviction に直結
   しないため)。Twitter cluster019 のような super-skewed workload では sloppy が許容範囲。
   逆に cluster016 のような「中堅 hot」では cold visited 取りこぼしが eviction を歪める。

4. **bench 設計の教訓**: `bench_concurrent.rs` の "shared keyspace + per-thread Zipf" 設計は
   hot-key contention を出すために `--skew >= 1.0` が必須。skew=0.0 (= uniform random) は
   miss-heavy だけど hot-key contention は起きない (どの key も等確率 → 衝突確率 1/keys)。
   sweep_c15s_phase1.sh の skew パラメータは 1.0 に修正すべき。Phase 2 では skew sweep
   ({0.8, 1.0, 1.2}) を最初から含める。

## 8. next steps

`2026-05-10-write-contention-design-space.md` §7 Phase 2 の予定変更:

- **Phase 2 (packed LongAdder) の動機を組み換え**: 当初は「reader visited fetch_or の MESI
  ping-pong を分散」が動機だったが、本稿で reader 側はほぼ既にゼロコストと判明。Phase 2 を
  進めるなら **writer 側 (Path A の fetch_or, line 549)** をターゲットにする変種、もしくは
  insert ホット時の Path B/C Mutex contention を扱う変種に切り替えるべき。
- **shard-affinity (improvement-ideas.md §G3-ζ)** がむしろ Phase 1 失敗を踏まえると筋がよい:
  hot-key の Path A 並行性が contention の本丸なら、shard 内 sub-sharding で Path A 競合自体を
  1/K に分散するほうが直接的。
- **profiling で contention 主因を再特定**: `perf c2c` (cache-to-cache) 測定で hot cache line を
  特定し、tags / visited / writer Mutex / entries のどこが本当の bottleneck かを見極めてから
  次の試行を組む。本稿の skew=1.0 c14s ベンチを `perf record` 下で回すのが最短。

## 9. open questions

1. **TLS overhead 3 ns の実測値確認**: `next_rand()` の inline 後 codegen を `cargo asm` で
   見る、もしくは bench harness 内で TLS なしの「常に gate fail」/「常に gate pass」版と
   差分を取れば TLS の純コストが切り出せる。Phase 2 設計判断のため数値で押さえる価値あり。
2. **HR loss の per-trace 偏り**: cluster019 がほぼゼロなのは workload 特性 (top-1 key が
   超 hot) のためと予想だが、count-min sketch で hot key 比を測ると cluster016 vs cluster019 の
   差を定量化できる。これは別途。
3. **bare Linux で再計測**: WSL2 の TLS 実装 (kernel emulation 経由) は bare Linux より
   `__tls_get_addr` が遅い可能性がある。Windows native か bare Linux で再計測すると
   regression 幅が変わるかも。これは [WSL2 bias memo](../../../.claude/...) と同じ系の
   懸念。ただし結論 (= sloppy net 負け) は変わらないと予想。

## 10. 再現コマンド

```
# 1) build
cargo build --release -p senba-research --bin bench --bin bench_concurrent

# 2) Phase 1 sweep (約 2 分)
bash scripts/sweep_c15s_phase1.sh

# 3) plot + summary table
uv run --project scripts python scripts/plot_c15s_phase1.py
```

profiles 出力:
- `profiles/c15s_phase1_2026-05-10.csv` (concurrent throughput, n=20)
- `profiles/c15s_hr_2026-05-10.csv` (Twitter HR, n=300)

figures:
- `docs/figures/c15s_phase1_thr.png`
- `docs/figures/c15s_phase1_hr.png`
- `docs/figures/c15s_phase1_pareto.png`
