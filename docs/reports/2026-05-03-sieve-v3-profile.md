# sieve_v3 プロファイル分解 — 「効かなかった」の中身を読む (2026-05-03)

- 日付: 2026-05-03
- 出発点: `docs/reports/2026-05-03-sieve-v3-bench.md` で v3 の改善が
  「v1 とほぼ tie、orig には 1.11–1.22x 負け」だった原因を、より細かく
  自実装の中の何にどれだけ時間を使っているかで切る。
- 方法: `samply record` (4 kHz, criterion 1 bench-id ずつ) で取った
  Firefox Profiler 形式 JSON を `addr2line -i` で各サンプルの inline
  チェーンに展開し、自実装ソースの行に self-time を当てる。スクリプトは
  `scripts/samply_lines.py` を更新して v3 を扱えるようにした。

## 計測条件

- バイナリ: `target/release/deps/micro-d82976a2858802c6` (criterion bench、
  `release`, `debug = "line-tables-only"`)
- workload: bench レポートと同じ Zipf 1M req
- プロファイル先:
  - 主条件: `insert_only/<v>/skew1/10000`
    (footprint=100k の 10%。orig 20.7ms / v1 24.81ms / v2 23.45ms / v3 23.50ms)
  - v3 ワーストケース: `insert_only/<v>/skew0.6/100`
    (footprint=100k の 0.1%。orig 39.06ms / v2 41.86ms / v3 47.57ms)

JSON は `profiles/{orig,v1,v2,v3}_skew1_cap10000.json` と
`profiles/{orig,v2,v3}_skew0.6_cap100.json` に保存。

## 結果 — カテゴリ別 self-time

「`std/core`」は HashMap の siphash / バケット走査 / `Option::unwrap` 等が
`core` 内で leaf になっているもの、「hashbrown」は `hashbrown::raw` の
SIMD マッチや `find` 系。`sieve_*.rs` だけが純粋に「自分の bookkeeping」。

### insert_only / skew1 / cap=10000

| category    | orig   | v1     | v2     | v3     |
|-------------|-------:|-------:|-------:|-------:|
| std/core    | 68.55% | 64.32% | 65.92% | 63.55% |
| sieve_self  | 19.25% | 23.71% | 21.79% | 24.99% |
| hashbrown   | 10.95% |  9.68% | 10.35% |  9.42% |
| other       |  0.92% |  1.29% |  1.30% |  1.41% |
| total samples (4 kHz) | 21558 | 26361 | 24538 | 24713 |

絶対サンプル数 (= ほぼ wall time) でみると:

|             | orig  | v1    | v2    | v3    | Δ(v3-orig) | Δ(v3-v1) | Δ(v3-v2) |
|-------------|------:|------:|------:|------:|-----------:|---------:|---------:|
| sieve_self  |  4150 |  6251 |  5346 |  6177 |     +2027  |    -74   |    +831  |
| std/core    | 14778 | 16955 | 16175 | 15706 |      +928  |   -1249  |    -469  |
| hashbrown   |  2361 |  2552 |  2540 |  2328 |       -33  |    -224  |    -212  |

読み:
- **orig→v3 の余分な ~3K サンプル (≈3 ms) のうち 2/3 は sieve 自身の
  bookkeeping コスト**、残り 1/3 は std/core (= HashMap)。hashbrown はむしろ
  v3 の方が少ない。
- **v3 vs v2 はほぼ tie**: sieve_self が +831 サンプル増えた分、std/core が
  -469 戻ってきている。"bit-parallel + 2-pass" の最適化分が "scan ロジック
  自体の太り" にほぼ吸われている。
- **v3 vs v1 では sieve_self は実は同じ (-74)**、std/core が -1249 減って
  いる。v1 から v3 で「Option 剥がし」(order の `Vec<Option<EntryId>>` →
  `Vec<EntryId>`) の効きはここに出ている。Bench 比 v3/v1=0.947 と整合。

### insert_only / skew0.6 / cap=100 (v3 ワースト条件)

| category    | orig   | v2     | v3     |
|-------------|-------:|-------:|-------:|
| std/core    | 69.61% | 64.65% | 60.78% |
| hashbrown   | 19.96% | 18.04% | 16.57% |
| sieve_self  |  8.78% | 11.37% | 17.64% |
| other       |  1.29% |  1.91% |  1.61% |
| total samples| 15223 | 17575  | 18964  |

cap=100 の世界では **v3 の sieve_self 比率は 17.6% と orig の 2 倍**。
絶対値も `1337 → 3346` と 2.5x。orig→v3 の余分 ~3.7K サンプルのうち
sieve_self が +2009、hashbrown は実は逆に -100 ほど少ない。**この条件で
v3 が遅い理由は HashMap ではなく、scan + compact** であることがわかる。

## v3 内の時間の使われ方 (skew1/cap10000)

inline チェーンに `sieve_v3.rs:<line>` が登場するサンプル数 (= その行を
何かの形で実行中だったサンプル) で、上から並べた:

| self% | self  | 行   | 何をしているか |
|------:|------:|-----:|----------------|
| 39.91 |  9862 |  365 | (`Cache::insert` trait dispatch — 全 insert を含む) |
| 17.64 |  4359 |  146 | `if let Some(&eid) = self.index.get(&key)` (existing-key 判定) |
|  9.46 |  2338 |  148 | `let entry = self.entries[eid].as_mut().unwrap()` (existing-key 取得) |
|  4.05 |  1000 |  162 | `self.maybe_compact();` (および分岐の中身) |
|  3.69 |   912 |  301 | `self.compact();` |
|  3.21 |   794 |  153 | `return None;` (existing-key 終端) |
|  2.51 |   621 |  293 | `let entry = self.entries[eid].take()...` in `do_evict` |
|  1.93 |   476 |  364 | `fn insert` 自身 (関数 prologue) |
|  1.75 |   433 |   37 | `BitSet::set`/`get` (qpos 操作) |
|  1.55 |   383 |  328 | `let ent = self.entries[eid].as_mut()...` in `compact` |
|  1.53 |   378 |  152 | `self.visited.set(qpos);` (existing-key visited bit) |
|  0.92 |   228 |  209 | `let valid = bit_range_mask(b, end_b);` (scan inner) |
|  0.85 |   209 |  156 | `let evicted = if self.len == self.capacity { ... }` |
|  0.79 |   196 |   59 | `bit_range_mask` 本体 |
|  0.78 |   192 |  316 | `if self.tombstone.get(old_pos) { continue; }` (compact loop) |
|  0.74 |   183 |  228 | `let traversed = bit_range_mask(b, v_bit);` (scan, victim found) |
|  0.74 |   182 |  229 | `self.visited.words[w] &= !traversed;` (visited clear, victim found) |
|  0.68 |   168 |  200 | `find_victim_in_range` 関数 entry |

これを論理ブロックに丸めると (重なりは取り除く):

| ブロック                  | 行レンジ            | self% (合算) |
|---------------------------|---------------------|-------------:|
| existing-key fast path    | 146–153             |       ~31.8% |
| `evict_one` + `find_victim_in_range` + `do_evict` | 200–296 |        ~7.5% |
| `maybe_compact` + `compact` | 162, 299–345     |       ~10.0% |
| `alloc_entry` + tail/order writes (新規挿入後半) | 167–177 |        ~3–4% |
| BitSet ヘルパ (`set`/`get`/`clear`/`bit_range_mask`) | 33–65 |  ~3% |

**この分布から読めること**:

1. **v3 の最大コストは scan でも compact でもなく、hit パスの HashMap 周り**。
   行 146–153 で 31.8% を使っている。`index.get(&key)` の leaf は 7.35%
   (= sieve_v3.rs leaf hot 1 位)、これは v1=8.41%/v2=7.93%/orig=8.51% と
   ほぼ同じ — 全実装が同じ HashMap で同じ workload を回しているので当然
   そうなる。**この層を変えない限り 8% 程度は剥がれない**。

2. **v1→v3 で攻めた `find_victim_in_range` (bit-parallel scan + 2-pass) は
   全体の 7.5% しかない**。仮に scan を 0 ms にしても 7.5% しか縮まない。
   v3 bench で v3/v1 が 0.947–1.036 のレンジで頭打ちなのは、攻めるブロック
   自体が小さいから。

3. **compaction が無視できない**。skew1/cap10000 でも 10% 弱、cap=100 では
   15% 強。これは linked-list の orig には存在しないコストで、array-based
   実装が原理的に背負うもの。`order` を `2 * capacity` 取って tombstone
   が `len` を超えたら compact する設計なので、tombstone 比率の閾値や
   `order` のサイズはここに直接効く。

4. **「Option 剥がし」が orig→v2/v3 で剥がした分は微小**。v3 の line 148
   (`entries[eid].as_mut().unwrap()`) と line 293 (`entries[eid].take()`)
   はまだ `Option<Entry>` を持っている (`order` だけ Option 剥がし済み)。
   leaf hot の `option.rs:767` が orig=13.9% / v2=10.0% / v3=9.5% に下がって
   いるのは、`order: Vec<Option<EntryId>>` 由来の unwrap が消えた効き。
   `entries: Vec<Option<Entry>>` の方は arena 構造の都合で残っている。

5. **bit-parallel と素朴な linear の差は scan ブロック内では既に消えている**。
   v3 (bit-parallel) の find_victim 周り合算 ~6–7% に対し、v2 (linear) も
   行 213/228/240 を足すと ~6%。ワークロードでは hand から数 slot で victim
   が見つかるのが支配的で、word ロード 2 本 (visited+tombstone) のオーバー
   ヘッドが帯走査の利得を相殺している。bench レポート §「なぜ bit-parallel
   が効いていないように見えるか」をプロファイルが裏付けた格好。

## v3 ワーストケース (skew0.6/cap=100) でどう変わるか

cap=100 では:

| ブロック                        | skew1/cap10000 self% | skew0.6/cap100 self% |
|---------------------------------|---------------------:|---------------------:|
| existing-key fast path (146–153)|                ~31.8 |                ~10.4 |
| `maybe_compact` + `compact`     |                ~10.0 |                ~15.3 |
| `evict_one` + scan + do_evict   |                 ~7.5 |                 ~5.5 |
| HashMap (`std/core`+hashbrown)  |                ~73.0 |                ~77.4 |

cap=100 では `len` が小さいので 1 回の eviction あたりのスキャン距離が
短く scan は更に小さくなる一方、tombstone がすぐ溜まって compact が頻発
する (`order_cap = 2*cap = 200`, `dead >= len` トリガが頻発)。**v3 が
orig より遅くなる主因は cap=100 では compaction**。

orig は linked list なので unlink → free のみ、(secondary) dead slot 概念
が無くて compaction を払わない。array-based 路線でこのオーバーヘッドを
消すには:
- `order_cap` を `4 * cap` 程度にして tombstone 蓄積の余裕を持たせる
- compact のトリガ (`dead >= len`) を緩める / 段階的にする
- そもそも `order` を ring buffer にして tombstone を hand 通過時に
  即時回収する (compact 不要にする)

あたりが直接効きそう。

## 「miss path は v3 の方が速いはず」の検算

直感: cache miss 1 件のコストは
  - orig: linked list を hand から N nodes 歩いて freq=1→0 にする (N×ポインタ
    chasing + Option unwrap) + unlink + free + index.remove
  - v3: 64bit word 1〜2 本を load して `trailing_zeros` で victim 1 発 + 軽い
    bookkeeping (tombstone.set/visited.clear/order index) + index.remove

なので v3 の方が確実に少ない命令数・少ないキャッシュミスで終わるはずに
見える。bench は逆。

### 検証: 各 phase に "そこにしか出ない行" をマーカーにして leaf samples を分類

`scripts/samply_phases.py` で skew1/cap10000 のプロファイルを分類した結果。
HashMap/siphash 系は全部 "OTHER" にまとまる:

| phase                 | orig sample | orig %  | v3 sample | v3 %    |
|-----------------------|------------:|--------:|----------:|--------:|
| OTHER (HashMap+siphash+leaf core) | 19229 | 89.20% | 19757 | 79.95% |
| HIT path              |         334 |   1.55% |      1172 |   4.74% |
| EVICT (scan+do+call)  |        1542 |   7.15% |      2264 |   9.16% |
| INSERT-NEW (post-evict 後半) | 383 |   1.78% |       522 |   2.11% |
| COMPACT               |           0 |   0.00% |       847 |   3.43% |

trace 別 (1M req のうち) hit/miss は `cargo run --bin bench` で実測:
**hit 776,084 / miss 223,916 / eviction 213,916** (orig)。両者は Zipf 上で
同じ評価列を出すので v3 でも同じ。

### 直感が外れた 3 つの理由

**(1) hit path が v3 の方が約 3 倍重い** (1.55% → 4.74%)

orig の hit path:
```rust
let node = self.node_mut(id);   // arena cache line load + Option unwrap
node.value = value;             // 同じ cache line に書く
node.freq = 1;                  // 同じ cache line に 1 byte 書く
return None;
```

v3 の hit path:
```rust
let entry = self.entries[eid].as_mut().unwrap();  // arena cache line load + Option unwrap
entry.value = value;            // 同じ cache line
let qpos = entry.qpos;          // 同じ cache line から read
self.visited.set(qpos);         // 別の cache line (visited.words[qpos/64]) を RMW
return None;
```

`visited.set(qpos)` は qpos 空間の packed bitmap を RMW する。`entry`
本体とは別アロケーションで、別 cache line。**hit のたびに余計な L1
load + RMW が 1 本走る**。trace の 78% が hit なので、絶対値で
+838 サンプル ≒ 全体の +3.2% が出る。

**(2) miss path も v3 の方が速くない**

直感では「N nodes walk」が消えるはずだが、profile は逆 (orig 7.15% < v3
9.16%)。理由は 2 つ:

- **orig の N が想像より小さい**。orig EVICT 7.15% × 21.56ms = 1.54ms ÷
  213,916 evictions ≈ **7.2 ns/eviction**。1 eviction あたり L1 hit
  1〜2 本の規模で、ポインタ chasing は事実上発生していない。SIEVE の
  steady state では、hand が通過した直後の slot は freq=0、その先も
  低頻度 key が並んでいるので、hand は 1〜2 step で victim にぶつかる。
  そもそも問題サイズがゼロに近いので、「O(N) を O(1) にした」ご利益が
  測れない。
- **v3 は eviction 1 件あたりの bookkeeping が orig より多い**: tombstone
  bit set + visited bit clear + `Option<Entry>::take()` + hand wrap +
  `dead += 1` + `len -= 1` + `maybe_compact()` 分岐 + (たまに) compact 本体。
  orig は `unlink` (4 writes に集約) + `free_node` (Option take) +
  `index.remove`。v3 の方が 1 件あたりに触る cache line が多い。

具体的に v3 EVICT 内訳: SCAN 4.57% (`find_victim_in_range`) +
DO-EVICT 4.15% (tombstone/visited/take/index.remove/free_list.push) +
EVICT-CALL 0.44% (shell)。orig EVICT 7.15% にはこれらが全部畳み込まれている。
**結果として「scan を bit-parallel にした分」(~+1%-pt) を「bookkeeping
が増えた分」(~+1%-pt) でほぼ相殺している。**

**(3) compact が 3.43% (cap=10000) / 6.82% (cap=100) 余分にかかる**

orig には存在しない work。array-based の構造を保つために必須なので、
miss path のコストとして他の評価と切り離せない。cap=100 では maybe_compact
が約 590,000 evictions のうち ~6,000 回 compact 本体に入り、毎回 200 slots
を再配置する。

### 数で答え合わせ

skew1/cap10000 で v3 - orig = 24.71 - 21.56 ≈ 3.15ms (per iter)。

| 因子                   | Δ samples (v3-orig) | Δ ms 換算 |
|------------------------|--------------------:|----------:|
| HIT path 余計な visited.set | +838            |   ~0.8ms  |
| EVICT 内 bookkeeping     | +722              |   ~0.7ms  |
| COMPACT 全体             | +847              |   ~0.8ms  |
| INSERT-NEW              | +139               |   ~0.1ms  |
| OTHER (主に HashMap)     | +528              |   ~0.5ms  |
| 合計                    | +3074              |   ~3.0ms  |

bench の 3.15ms 差 ≒ profile の +3074 サンプル × ~1ms/1024 サンプル と
だいたい一致する (rate 4kHz / iter 当たり ~94 sample → 1 sample ≒ iter/94)。

### 結論: なぜ v3 ≧ orig (期待外れ) なのか

「evict 1 回が速くなる」は正しいが、その節約幅は **~7ns** しかなく:
- hit path の余分な visited.set RMW (×78% req)
- evict 1 回あたりの bookkeeping 増 (×22% req)
- compact (orig には無い work)

の 3 つの "v3 で増えたもの" の合計に負ける。orig の linked list は
**hand 起点で常にローカルなアクセスしかしない**ため、"O(N) walk" を消す
ご利益が測れるレベルの N にならない、というのが本質。

## まとめ

- v3 の中で v1→v3 の改善が攻めていたブロック (`find_victim_in_range` +
  do_evict) は **全体の 7–8%** しかなく、改善の上限がそもそも小さい。
  bench で「v1 とほぼ tie」になったのはこのため。
- v3 が orig に負けている分の **2/3 は sieve 内の bookkeeping** で、
  内訳は (a) compact の存在 (orig には無い)、(b) 配列レイアウトを維持する
  ための tombstone/visited 操作。
- `index.get` 自体は全実装でほぼ同じ時間を消費しているので、HashMap を
  入れ替えない限り **HashMap 層を超える改善は出ない**。
- 次の手としてプロファイルが筋を示しているのは:
  - **compact を消す / 軽くする** (ring buffer 化、`order_cap` 拡張、
    トリガ緩和) — cap が小さい条件で特に効きそう
  - **`entries: Vec<Option<Entry>>` の Option も剥がす** — `option.rs:767`
    leaf 9.5% を更に削れる可能性
  - **HashMap を `FxHashMap` / `ahash` に差し替える** — 全実装に効くが、
    比較ベンチの公平性のため別レイヤとして検証する必要あり

## 再現

```bash
cargo bench --bench micro --no-run
for v in orig v1 v2 v3; do
  samply record --save-only --no-open --rate 4000 \
    -o "profiles/${v}_skew1_cap10000.json" -- \
    target/release/deps/micro-d82976a2858802c6 \
    --bench "insert_only/${v}/skew1/10000\$"
done
python3 scripts/samply_lines.py
```
