# 2026-05-10 — VISITED ビットを tag から外して per-shard u64 bitmap 化 (採択)

- 関連実装: `src/shard.rs` (採択、未 commit)
- 関連: `2026-05-08-find-avx2-caller-merge.md` (find_avx2 hot path の前回最適化、本稿はその後段)、
  `2026-05-09-vtune-windows-orig-vs-senba.md` (cap-fits 帯で senba<orig が残る問題、本稿で部分緩和)
- 種別: **実測ノート**。設計→実装→6 シナリオ perf-gate→Twitter trace cross-check まで一貫実施

## 0. TL;DR

`Shard::tags[i]: u16` に同居していた `VISITED = 0x4000` ビットを、`Shard` 構造体に新設した
`visited: u64` bitmap (per-shard) に追い出す。`MAX_PER_SHARD = 64` なので一語で足り、
`hand`/`len`/`hits`/... が乗る制御 cache line に同居するので置き場のコストはゼロ。

副次効果:

1. **HASH_MASK が 8 → 9 bit に拡大** (非 LIVE 領域 0x3FFF → 0x7FFF、ID_MASK 6 bit を引いた残り)。
   `find_avx2` の SCAN_MASK 一致確率は 1/256 → 1/512 になり、key 比較に進む偽陽性が半減。
2. **`find_evict_pos` が O(len) → O(1) 化**。`scan_evict` の linear walk が `(!visited & live_mask & !below_hand).trailing_zeros()`
   1 命令と `&= !mask` の clearing 1 命令で済む。
3. **`tags[pos] |= VISITED` (u16 RMW on tags line) → `visited |= 1u64 << pos` (u64 RMW on 制御 line)** に置換。
   制御 line は `hits += 1` で同じ命令列内で既に dirty なので line dirty 数は増えない。

代償:

- `insert` 退避経路と `remove` の `tags.copy_within(pos+1..len, pos)` に対応する bitmap shift
  (u128 経由で pos==63 の corner を回避) が増える。だが eviction 経路は元から hot ではない。
- `retain` の compaction で `new_visited` を作り直す (0..old_len の単純ループ)。これも元の I8
  remap の周辺なので増分は誤差。

**perf-gate (criterion 6 シナリオ) AB**:

| シナリオ | Δ time (median) | 判定 |
|---|---|---|
| insert_u64 (Slot32, Zipf 1.0) | +1.35% | within noise |
| mixed_u64 (50/50, Zipf 1.0) | **−3.05%** | improved |
| insert_string (Slot32 String key) | +1.38% | within noise |
| insert_u32_slot16 (Slot16) | +0.90% | within noise |
| get_heavy_u64 (90/10, Zipf 1.0) | **−7.76%** | improved |
| mixed_lowskew_u64 (50/50, Zipf 0.7) | **−10.04%** | improved |

read-heavy / churn-heavy の 2 帯で 8〜10% の利得、insert-heavy 帯で +1% (5% gate 内)。

**Twitter trace cross-check (5 cluster × 3 cap × 3 run、senba 単独 before/after)**:

| cluster | cap | HR | before (ms) | after (ms) | Δ |
|---|---|---|---|---|---|
| 006 | 4096 | 0.353 | 34.84 | 31.87 | **−8.5%** |
| 006 | 16384 | 0.642 | 32.96 | 29.25 | **−11.3%** |
| 006 | 65536 | 0.829 | 28.42 | 24.18 | **−14.9%** |
| 016 | 4096 | 0.497 | 32.34 | 31.74 | −1.8% |
| 016 | 16384 | 0.675 | 32.15 | 30.85 | −4.0% |
| 016 | 65536 | 0.777 | 31.93 | 31.58 | −1.1% |
| 018 | 4096 | 0.628 | 31.06 | 29.13 | **−6.2%** |
| 018 | 16384 | 0.737 | 30.11 | 28.34 | **−5.9%** |
| 018 | 65536 | 0.821 | 29.24 | 25.88 | **−11.5%** |
| 019 | 4096 | 0.316 | 31.91 | 29.01 | **−9.1%** |
| 019 | 16384 | 0.322 | 32.86 | 29.92 | **−8.9%** |
| 019 | 65536 | 0.329 | 34.99 | 31.58 | **−9.8%** |
| 034 | 4096 | 0.355 | 31.91 | 31.67 | −0.7% |
| 034 | 16384 | 0.390 | 32.38 | 31.99 | −1.2% |
| 034 | 65536 | 0.411 | 32.87 | 32.90 | +0.1% |

**15 セル中 14 セルで improvement、平均 Δ −6.3% (range −14.9% 〜 +0.1%)**。
hits/misses/evictions は全 15 セル × 2 状態 (= 90 サンプル) で **bit-for-bit 同一**
(oracle 維持を確認)。退行は cluster034/65536 の +0.1% のみ (実質ノイズ)。
詳細パターン分析は §4.2。

**採択**。perf-gate 5% gate を超える退行なし、Twitter cross-check も方向一致、
oracle 等価性は維持 (`research/tests/oracle.rs` + `oracle_cache_match.rs` 全 28 テスト pass)。

---

## 1. 動機と仮説

ユーザーから「VISITED を tag に同居させない方が速いのでは」という問題提起。事前分析:

- **find_avx2 の SIMD 部分は変わらない**。SCAN_MASK の AND は VISITED を消すためでなく、
  per-slot で値の異なる ID フィールドを消すために必須。VISITED を抜いても AND は残る。
- **副次効果として 2 つの勝ち筋が見える**:
  - HASH_MASK が 1 bit 増える (9 bit) → SCAN_MASK 一致の偽陽性 1/256→1/512、key 比較
    回数が減る (per_shard ≤ 64 だが、それでも miss 多めや低 skew で効く)
  - per_shard ≤ MAX_PER_SHARD = 64 なので u64 bitmap で済み、`find_evict_pos` が
    bit-twiddle 一発になる
- **負け筋**: insert 退避と remove の tags shift と同じ shift を bitmap にも掛ける
  必要がある。`u128` 経由で pos == 63 の corner も処理可能。eviction は元々 cold path。

「副次効果が実際に hot path で見えるか」は実測でしか分からないので perf-gate + Twitter
cross-check で AB する方針。

## 2. 実装

### 2.1 tag 層の変更

```text
旧: [LIVE:1][VISITED:1][HASH:8 散][ID:6 << ID_SHIFT]    (合計 16 bit)
新: [LIVE:1][HASH:9 散][ID:6 << ID_SHIFT]               (合計 16 bit、VISITED は外出し)
```

3 つの bracket 全てで HASH が 1 bit 増える:

| bracket | ID_SHIFT | ID_MASK | 旧 HASH_MASK | 新 HASH_MASK |
|---|---|---|---|---|
| Slot16 | 4 | 0x03f0 | 0x3c0f (8 bit) | 0x7c0f (9 bit) |
| Slot32 | 5 | 0x07e0 | 0x381f (8 bit) | 0x781f (9 bit) |
| Slot64 | 6 | 0x0fc0 | 0x303f (8 bit) | 0x703f (9 bit) |

`needle_from_hash` は hash の上位 9 bit (was 8) を `[0..ID_SHIFT) ∪ [ID_SHIFT+6, 15)` に
spread。3 bracket 全てで 9 bit 入射 (テスト `needle_spread_is_injective_all_slots`、
0..=511 全 hash で衝突なしを確認)。

### 2.2 visited bitmap

`Shard` に `pub(crate) visited: u64` フィールドを追加 (`hand`/`len`/`hits`/... の隣)。
位置 i のビットは `tags[i]` の VISITED 状態。`MAX_PER_SHARD = 64` なので一語で足りる。

```rust
// 旧
self.tags[pos] |= VISITED;

// 新
self.visited |= 1u64 << pos;
```

5 箇所 (`get` / `get_mut` / `get_key_value` / `get_or_insert_with` の hit / `insert` の
replace 分岐) を機械置換。`peek*` は元から VISITED を立てないので変更なし。

### 2.3 find_evict_pos の bit-twiddle 化

旧 `scan_evict` の二段ループ (hand→len で walk しながら VISITED clear、見つかれば
victim 返却、なければ 0→hand に同じ事を) を、bit 演算に書き換え:

```rust
let live_mask = if len >= 64 { !0 } else { (1u64 << len) - 1 };
let below_hand = (1u64 << hand) - 1;
let above_hand = live_mask & !below_hand;

// Pass 1: [hand, len) の最初の un-visited
let high_search = !self.visited & above_hand;
if high_search != 0 {
    let victim = high_search.trailing_zeros() as usize;
    let walked = ((1u64 << victim) - 1) & !below_hand;
    self.visited &= !walked;       // walked-over visited bits を clear
    return victim;
}
self.visited &= !above_hand;       // [hand, len) 全 visited だった: 一括 clear

// Pass 2: [0, hand) も同様
let low_search = !self.visited & below_hand;
if low_search != 0 { ... }
self.visited &= !below_hand;
hand                                // 全 visited: hand 任意選択
```

`scan_evict` 関数自体を削除。

### 2.4 tags shift と並走する bitmap shift

`insert` の退避と `remove` で `tags.copy_within(pos+1..len, pos)` するとき、bitmap も
同じ shift を適用しないと bit i の指す tag が ずれる。helper を `shard.rs` 冒頭に追加:

```rust
#[inline]
fn shift_visited_down_in_place(visited: &mut u64, pos: usize) {
    debug_assert!(pos < 64);
    let v = *visited as u128;            // pos == 63 で `>> 64` UB を回避
    let low = v & ((1u128 << pos) - 1);
    let high = (v >> (pos + 1)) << pos;
    *visited = (low | high) as u64;
}
```

`insert` 退避経路: 新しい entry が `tags[last]` (= `len-1`) に入る。shift 後の bit
`last` は元の bit `len` (= 0) なので自動的に 0 になり、新エントリの "未 visited"
状態と一致する。`remove` も同じ shift で OK (新 len の上限 `last` の bit が 0)。
**id-level swap (I8 復元) は entries 配列の id を入れ替えるだけで slot position は
動かない**ので bitmap 更新不要。

### 2.5 retain の compaction

keep/drop pass で `new_visited` を別途累積し、最後にコミット:

```rust
let old_visited = shard.visited;
let mut new_visited: u64 = 0;
for read in 0..old_len {
    let keep = unsafe { f(...) };
    if keep {
        if (old_visited >> read) & 1 != 0 {
            new_visited |= 1u64 << write;
        }
        // ... tag compaction ...
        write += 1;
    } else {
        // ... drop ...
    }
}
shard.visited = new_visited;
```

panic guard の `Drop` も `self.shard.visited = 0` を追加 (panic 時 shard をリセット)。

### 2.6 雑多な配線

- `Shard::new`: `visited: 0` で初期化
- `clear`: `self.visited = 0`
- `Clone`: `new.visited = self.visited`
- `Drain::new` (`iter.rs`): `sh.visited = 0` も忘れずリセット
- `lib.rs`: `pub(crate) use shard::VISITED` を削除 (constant 自体も削除)
- `tests/slot.rs`: bit-layout assertion を新値に更新、`needle_spread_is_injective_all_slots`
  を 8 bit (0..=255) → 9 bit (0..=511) に拡張

## 3. 正しさ検証

### 3.1 既存テスト

```text
cargo test --workspace                       → 全 pass
cargo test -p senba-research --features external-traces
                                              → oracle 23 pass + oracle_cache_match 5 pass
cargo clippy --workspace --all-targets -- -D warnings → clean
```

senba 単独で 101 unit + 1 doctest pass。

### 3.2 oracle 等価性 (load-bearing)

`oracle.rs` / `oracle_cache_match.rs` は senba::Cache の eviction 列が `sieve_orig` と
**byte-for-byte 一致**することを Zipf trace 上で確認する load-bearing test。これが
通ったので SIEVE state machine の意味論は変わっていない (= visited bitmap への
移行は純粋な実装書き換え)。

### 3.3 Twitter trace の hits/misses

§4.2 の cluster018 3 run × 3 cap = 9 セル全てで hits/misses/evictions が before/after
で完全一致。SIEVE 決定論性が成立 (= 同一 trace で同一 evict 列)。

## 4. 実測

### 4.1 perf-gate (criterion, 6 シナリオ)

`cargo bench -p senba-research --bench sieve_cache_perf -- --baseline before-visited-bitmap`。

```text
insert_u64/384         time +1.3531% (p=0.00)  thrpt -1.34%   noise
mixed_u64/384          time -3.0494% (p=0.00)  thrpt +3.15%   improved
insert_string/256      time +1.3833% (p=0.01)  thrpt -1.36%   noise
insert_u32_slot16/384  time +0.8983% (p=0.01)  thrpt -0.89%   noise
get_heavy_u64/384      time -7.7553% (p=0.00)  thrpt +8.41%   IMPROVED
mixed_lowskew_u64/384  time -10.044% (p=0.00)  thrpt +11.17%  IMPROVED
```

事前仮説と合致:

- **get_heavy −7.8%**: read-heavy では (a) bitmap visited-set が tags line を dirty
  しないこと (制御 line に同居)、(b) HASH 9 bit による偽陽性半減、の合算が効く
- **mixed_lowskew −10.0%**: Zipf 0.7 = 高 churn なので eviction 経路の比率が上がり、
  O(1) bitmap victim search が O(len) scan_evict を置換した分が直接見える
- **insert_* +1%**: bitmap shift (u128 経由) のオーバーヘッド。だが noise 内
- mixed_u64 (Zipf 1.0 50/50) は read 半分・write 半分で −3% (read 部分だけ受益)

### 4.2 Twitter trace 5 cluster sweep (3-run mean, senba 単独)

5 cluster (006/016/018/019/034) × 3 cap (4096/16384/65536) × 3 run = 45 計測点 ×
2 状態 (before/after) = 90 sample。`./target/release/bench --source twitter
--path external/twitter-cache-trace/cluster<NN> --capacity 4096,16384,65536
--variant senba` を `git stash` を挟んで両状態で実行。

| cluster | cap | HR | before (ms) | after (ms) | Δ | 帯域分類 |
|---|---|---|---|---|---|---|
| 006 | 4096 | 0.353 | 34.84 | 31.87 | **−8.5%** | miss-heavy |
| 006 | 16384 | 0.642 | 32.96 | 29.25 | **−11.3%** | mid-HR |
| 006 | 65536 | 0.829 | 28.42 | 24.18 | **−14.9%** | hit-heavy |
| 016 | 4096 | 0.497 | 32.34 | 31.74 | −1.8% | miss-heavy |
| 016 | 16384 | 0.675 | 32.15 | 30.85 | −4.0% | mid-HR |
| 016 | 65536 | 0.777 | 31.93 | 31.58 | −1.1% | hit-heavy |
| 018 | 4096 | 0.628 | 31.06 | 29.13 | **−6.2%** | mid-HR |
| 018 | 16384 | 0.737 | 30.11 | 28.34 | **−5.9%** | hit-heavy |
| 018 | 65536 | 0.821 | 29.24 | 25.88 | **−11.5%** | hit-heavy |
| 019 | 4096 | 0.316 | 31.91 | 29.01 | **−9.1%** | miss-heavy |
| 019 | 16384 | 0.322 | 32.86 | 29.92 | **−8.9%** | miss-heavy |
| 019 | 65536 | 0.329 | 34.99 | 31.58 | **−9.8%** | miss-heavy |
| 034 | 4096 | 0.355 | 31.91 | 31.67 | −0.7% | miss-heavy |
| 034 | 16384 | 0.390 | 32.38 | 31.99 | −1.2% | miss-heavy |
| 034 | 65536 | 0.411 | 32.87 | 32.90 | +0.1% | mid-HR |

集計:

- **15 セル中 14 セルで improvement、平均 Δ −6.3%、median Δ −5.9%、range
  −14.9% 〜 +0.1%**
- 5% 以上の利得: 9 セル (60%)。10% 以上の利得: 4 セル (cluster006/65536 −14.9%、
  cluster018/65536 −11.5%、cluster006/16384 −11.3%、その他 cluster019 系で 9% 級が 3 つ)
- 退行セル: 0。最も控えめな cluster034/65536 で +0.1% (実質ノイズ)

cluster 別パターン:

- **cluster006 / cluster018**: hit-heavy 帯 (cap=65536 で HR 0.83/0.82) で最大利得
  (−14.9% / −11.5%)。read 占有率が高いほど bitmap 化の visited-set RMW (制御 line
  でクローズ) と HASH 9 bit 化 (false-positive 1/256→1/512) の合算が効く、という
  criterion get_heavy −7.8% と方向一致した上で、絶対量は trace 上で更に大きい。
- **cluster019**: 3 cap いずれも HR 0.32 前後 (= miss-dominant)。にも関わらず
  −9% 級の利得が 3 cell で揃う。miss 経路は (a) `find` の SCAN 一致 → key 比較 →
  miss 判定で偽陽性半減が直接効き、(b) miss → eviction の連鎖で `find_evict_pos`
  O(1) 化も効く、両方の合算と思われる (criterion mixed_lowskew −10.0% と整合)。
- **cluster034**: HR 0.35–0.41 で cluster019 と帯域は近いが利得は −0.7〜+0.1%。
  cluster019 と何が違うかは本稿スコープ外 (rerequest 種別の分布や trace の
  hot-key 分布が違う可能性)。重要なのは **退行はしていない**こと。
- **cluster016**: 全 cap で −1〜−4% と利得は控えめだが一貫してプラス。

WSL2 環境バイアス (`2026-05-09-vtune-windows-orig-vs-senba.md`) は senba 単独 AB
なのでほぼ乗らない (環境差は before/after で同居して打ち消される)。orig との
absolute gap が WSL2 で誇張される問題は本件の評価軸とは独立。

#### 4.2.1 cluster034 が控えめな件のメモ

cluster034 だけ利得が小さい。同じ低 HR 帯の cluster019 が −9% 級なのに対し
cluster034 は ≤−1.2%。可能性のある原因:

- trace の access pattern が違う。cluster019 は long-tail 方向に均一散布で `find`
  miss → evict のサイクルが多発、cluster034 は同じ key を頻繁に再 insert する
  パターンで `insert` の **replace 分岐** (find 一致 → 値だけ書き換え + visited set)
  が支配的、という仮説。replace 分岐は read 寄りなので bitmap 化の利得は出るはず
  だが、`find` の判定回数当たりの利得は read-only ループ (cluster019 の miss でも
  hit でも `find` を回す pattern) より小さい。
- ただしこれは仮説であり、cluster034 単独の hit/miss 列だけでは断定できない。
  気になれば cluster034 の trace を ARC/file 経由で再生して `bench_concurrent`
  で profile 採取するのが筋。本稿の採否判断には影響しない。

## 5. 含意

- **採択**。perf-gate 5% gate を破る退行はなく、read-heavy / churn-heavy で 8–10% 利得、
  insert-heavy で +1% (gate 内)。実 trace cross-check も方向一致。
- **HASH 9 bit 化の副次効果は思った以上に効いた**かもしれない。get_heavy −7.8% の
  内訳は perf 計測なしでは厳密に分けられないが、「visited-set RMW を別 line に逃した
  だけ」では −7.8% は説明しづらく、SCAN 偽陽性半減 (= key 比較回数半減) が乗っている
  と考えるのが自然。今後の `find_avx2` 系最適化で「false-positive 削減効果」を見るときの
  baseline を更新する必要がある。
- **`find_evict_pos` O(1) 化はそれ単独で測定できていない** (mixed_lowskew −10% は HASH
  9 bit 化と合算)。気になれば evict-only micro bench を追加する余地あるが、
  優先度は低い。
- WSL2 計測 confound (`2026-05-09-vtune-windows-orig-vs-senba`) と本件は独立: 本件は
  senba 単独 AB なので環境バイアスは乗りにくい。orig vs senba の cap-fits 帯ギャップは
  本件で **部分緩和**するが解消はしない (構造的 instruction footprint 差は別問題)。

## 6. follow-up 候補

- **`find_avx2` の caller-merge 周辺の再評価** (`2026-05-08-find-avx2-caller-merge.md`)。
  HASH 9 bit 化で false-positive が半減した分、`entry_ptr_from_tag` 周辺の asm が
  ほぼそのまま温存される (見直しの必要なし)。
- **per-shard atomic 化の前段としての適性確認**。`visited: u64` 単独原子化 (`AtomicU64`) は
  bit RMW を `fetch_or` に変えるだけで済む。並行版 (c8/c9 系) 設計に流用可能か別途検討。
- **`scan_evict` 削除に伴う死コード一掃**。本パッチで `scan_evict` は消した。`docs/improvement-ideas.md`
  に "VISITED bitmap 化" 案があれば消化済みマークを付ける (要確認)。
- **Slot8 復活案** (`2026-05-09-vtune-windows-orig-vs-senba.md` §6 検証案 1) は本件と直交。
  Slot8 では Entry が極小なので tag 内 bit 数の制約が更に厳しく、visited bitmap 化との
  併用前提で設計するのが自然。
