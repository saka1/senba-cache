# 2026-05-05 — `sieve_j8` 初回ベンチ: per_shard=64 固定 vs orig/j7

- 親設計書: `2026-05-05-sieve-j8-design.md`
- 比較対象: `sieve_orig`、`sieve_j7`
- trace: Twitter cluster018 (1M 行)
- raw: `profiles/j8_twitter_pareto_2026-05-05.csv`
- 実装: `src/sieve_j8.rs` + 既存 bench/test 連携
- **2026-05-06 改訂**: §3 以降を samply profiling の結果で書き換え。初稿の「entries[id] が
  scattered → L1 prefetch 不発」仮説は profile data で **棄却**された (詳細 §4.3)。
  退行の真因は dep chain 延長 + false-match 率倍増という別の構造的コスト。

## 1. 目的

`2026-05-05-sieve-j8-design.md` で詰めた「§M5.3 + tag 内 ID embed + free_list 廃止」設計の **最初の実機検証**。
「per_shard を固定して特性が出るか」を最優先で確認するため、構造的上限の `per_shard = 64`
(= 6-bit ID 制限の上限) 一本で sweep した。cap ∈ {1024, 4096, 16384} に対し
SHARDS = cap / per_shard = {16, 64, 256} となる。

論点は 2 つ:

1. **正しさ**: SIEVE 意味論が j7 / sieve_orig と一致するか (= eviction 列の外部観測)
2. **throughput**: `2026-05-05-sieve-j8-design.md` §8.1 の机上検討「j7 比 +0.5〜+1 ns/op (id 抽出 +2 cy 由来)」
   が当たるか。当たらない場合、§9.2 で挙げた候補 (id 抽出の register pressure / scattered
   entries access による L1 prefetch 不発 / hot working set の L1 不滞留) のどれが効いているか

メモリ観点 (= memfair sweep) は本レポートの範囲外。

## 2. 正しさ確認

### 2.1 oracle test (`tests/oracle.rs`)

`j8_1shard_matches_orig_on_synthetic_zipf` および `_on_bundled_zipf` を新設し、
`sieve_orig` と j8 (1-shard、cap ∈ {16, 32, 64}) で **eviction 列の完全一致** を検証
(skew ∈ {1.05, 1.1, 1.2, 1.5} × Zipf 200K trace + bundled NSDI24 zipf_1.0 100K)。
全 7 cell で divergence ゼロ。

> j8 (1-shard) は SIEVE 意味論を bit-exact に保つ。

### 2.2 in-module sanity (`sieve_j8::tests`)

22 テスト全 green。重要項目:

- `bit_layout_exclusivity`: `LIVE | VISITED | ID_MASK | HASH_MASK = 0xFFFF` (排他的に u16 を埋める)
- `warm_up_to_steady_transition`: cap=4 で 5 個目 insert が初 evict、freed_id pass-through を確認
- `compact_preserves_id_mapping`: tail が order_cap に達して compact が走った後も
  既存 key の get が正しい値を返す
- `per_shard_above_max_panics`: cap=65 / SHARDS=1 で `Inner::new` panic 確認
- `matches_j7_externally`: j7 と j8 で同 trace の get 結果が完全一致

### 2.3 bench での integrity 確認

下表の通り、**全 cell で j7 と j8 の hits/misses/evictions が bit-exact 一致**。
これは「tag bit 構成が変わっても eviction 列は不変」という設計上の予言 (§6.1) を実証している。

| cap   | j7 hits  | j8 hits  | j7 evictions | j8 evictions |
|------:|---------:|---------:|-------------:|-------------:|
| 1024  | 508,369  | 508,369  | 490,607      | 490,607      |
| 4096  | 627,648  | 627,648  | 368,256      | 368,256      |
| 16384 | 737,376  | 737,376  | 246,240      | 246,240      |

## 3. throughput 結果 (median ns/op、5 trial)

cluster018 1M 行、per_shard=64 固定。

| cap   | shards | orig    | j7      | j8      | Δ(j8 − j7) | Δ(j8 − orig) |
|------:|------:|--------:|--------:|--------:|-----------:|-------------:|
| 1024  | 16    | 38.33   | 31.48   | 35.57   | **+4.09**  | −2.76        |
| 4096  | 64    | 30.47   | 31.09   | 34.89   | **+3.80**  | +4.42        |
| 16384 | 256   | 29.09   | 31.51   | 33.43   | **+1.92**  | +4.34        |

### 3.1 観察

- **j8 は j7 比で +1.9〜+4.1 ns/op 退行** した。`2026-05-05-sieve-j8-design.md` §8.1 の机上検討
  「+0.5〜+1 ns/op (id 抽出 +2 cy ≈ +0.5 ns at 4 GHz)」を **2〜8 倍上回る**。
- cap=1024 では j8 はまだ orig を 2.76 ns 引き離すが、cap=4096/16384 では
  **j8 が orig に負ける** (それぞれ +4.42 ns、+4.34 ns)。j7 は cap=1024/4096 で
  orig を支配していたので、j8 の id 埋込で「j7 が稼いでいた帯域での優位」を失った形。

## 4. 退行原因 — 命令レベル + プロファイラで確定

机上検討の予測 (+0.5〜+1 ns) と実測 (+1.9〜+4.1 ns) の乖離が大きいので、
asm 比較と samply による命令レベル profile で原因を切り分けた。

### 4.1 disassembly: candidate-found path の差分

j7 / j8 の `Inner::find_avx2` を逆アセンブルし、SIMD scan で match が立った
直後の処理を並べる:

**j7** (lane → entries[pos]):
```asm
275d0: tzcnt  %r11d,%ecx
275d5: shr    $1,%edx                  ; lane = bit / 2
275d9: or     %r10,%rdx                 ; pos = i + lane
275df: shl    $0x4,%rbx                 ; pos * 16  (sizeof Entry)
275e3: cmp    %rsi,(%rdi,%rbx,1)        ; key compare @ entries[pos]
```

**j8** (lane → tag → id → entries[id]):
```asm
2c610: tzcnt  %r11d,%ecx
2c615: shr    $1,%edx
2c619: or     %r10,%rdx                 ; pos = i + lane
2c61c: movzbl 0x1(%r9,%rdx,2),%ebx      ; ★ tags[pos] の高位バイト (= live|visited|id) ロード
2c622: and    $0x3f,%ebx                ; ★ 6-bit id にマスク (visited/live を一緒に落とす)
2c625: shl    $0x4,%ebx                 ; id * 16
2c628: cmp    %rsi,(%rdi,%rbx,1)        ; key compare @ entries[id]
```

増えているのは命令 2 つだけで、code-gen は **狙い通りクリーン**。LLVM は
`(tag & ID_MASK) >> ID_SHIFT` を素直に書いた Rust コードから「上位バイト 1 byte ロード
+ `and 0x3f`」に畳んでいる。`shl $4` の indexing は j7 側にも存在するので差分にはカウントしない。

加わった単独コストは:
- `movzbl 0x1(%r9, %rdx, 2)`: u16 の上位バイトを 1 byte ロード。直前の AVX2 vpand が
  同じ cacheline `(%r9, %r10, 2)` を触っているので **L1 hit 確実**。ただし latency は
  4-5 cy 露出 (= L1-hit latency)
- `and $0x3f, %ebx`: 1 cy

→ 静的見積もり +5〜+6 cy ≈ +1.3〜+1.5 ns @ 4 GHz per "candidate found"。

### 4.2 samply profiling

cap=4096 / per_shard=64 cell で j7_n64 / j8_n64 を `samply -r 8000` × 50 反復
(=50M ops 相当) で記録。総 leaf サンプル数 j7 = 14115, j8 = 15403。

**find_avx2 が profile に占める share (絶対 sample / 総 leaf):**

| variant | total | find_avx2 内 | share | × ns/op | find_avx2 absolute |
|---------|------:|-----------:|------:|--------:|--------------------:|
| j7      | 14115 |       4611 | 32.67% | 31.09   | **10.16 ns/op** |
| j8      | 15403 |       6064 | 39.37% | 34.89   | **13.74 ns/op** |
|         |       |            |        |  Δ      | **+3.58 ns/op** |

つまり **観測された +3.80 ns/op の退行のほぼ全量 (94%) が find_avx2 の中で発生している**。
insert/evict/hash 経路は j7 と j8 で同等。問題は SIMD スキャンとその直後の処理に局在化。

### 4.3 「entries[id] が scattered で L1 prefetch 不発」仮説の棄却

初稿の本命仮説だったが、profile から否定される。

samply の skid を考慮して、`cmp [entries[id]]` の load レイテンシが伸びていれば、
その直後の命令 (= 成功時の `mov $0x1, %eax`、失敗時の `and $0x1e, %cl`) にサンプルが
集中するはず:

| 命令 | j7 samples | j8 samples |
|------|-----------:|-----------:|
| `mov $0x1, %eax` (key match 成功 skid 受け) | 553 | 561 |

**j7 と j8 で同一 (Δ +8、誤差圏内)**。entries load は j8 でも L1 hit していて、stall は
出ていない。

理由は単純で、per_shard=64 のとき 1 shard 分の entries arena は `64 × 16 B = 1024 B
= 16 cacheline` で **L1 (32 KB = 512 line) に余裕で収まる**。アクセス順が「pos 順 (j7)」
だろうが「id 順 (j8)」だろうが、全部 L1 命中する。HW prefetcher の出番が無く、
よって prefetch が「効くか効かないか」が結果に出ない。

> per_shard を上げて shard あたり working set が L1 から溢れる帯域では、初稿の仮説が
> 蘇る可能性はある。ただし j8 は構造的に per_shard ≤ 64 なので、本設計では永久に
> 起きない問題。

### 4.4 真の退行源 — dep chain 延長 + false-match 率倍増

samply で j7 / j8 の find_avx2 内の hot 命令を絶対 sample 数で並べる:

| 命令クラス (発火条件) | j7 | j8 | Δ |
|------|---:|---:|---:|
| `vpand`+`vpcmpeqw`+`vpmovmskb` (SIMD scan 核) | 1850 | 1652 | -198 (誤差) |
| `mov $1, %eax` (= cmp[entries] 成功 skid) | 553 | 561 | +8 |
| **`and $0x3f, %ebx` (= id_of の skid 受け)** | — | **433** | **+433** |
| **`and $0x1e, %cl` (= 失敗パスの mask clear、false-match 時のみ発火)** | 6 | **409** | **+403** |
| loop control (`add 0x10`, `cmp r8 r10`) | 550 | 676 | +126 |
| `lane = bit >> 1` (`shr $1`) | 386 | 485 | +99 |

#### (a) dep chain 延長 — 0x96 ns/op (推定)

`and $0x3f, %ebx` への +433 sample 集中は、その上流 `movzbl tags[pos]` の **L1-hit latency
(4-5 cy) が dep chain 上に直列に挟まり、IP がここに張り付く** ことの可視化。j7 の同位置
には 1 cy の `shl` しか無いので、+5 cy がそのまま見える。

candidate-found path のパイプ計算:

```
chunk-load (vpand) → vpcmpeqw → vpmovmskb → tzcnt → shr → or → ?
                                                                  ↓
                              j7:                  → shl → cmp[entries[pos]] → je
                              j8:  → movzbl[tags] → and  → shl → cmp[entries[id]] → je
                                       (+5 cy)    (+1 cy)
```

per get で candidate に当たる確率 ≒ hit ratio + false-match 期待値 = 0.627 + 0.25 ≈ 0.88
(cap=4096 cell)。「dep chain +5-6 cy」が candidate ヒット時に発火 → 0.88 × 1.4 ns ≈
**+1.2 ns/op**。

#### (b) false-match 率 64x 増 — +0.92 ns/op (実測)

j7 は hash 14 bit (false-match 率 1/16384)、j8 は hash 8 bit (1/256)。per_shard=64 で
1 scan = 4 chunk × 16 lane = 64 lane だから、scan あたりの期待 false match 数:

- j7: 64 / 16384 = **0.004 / scan**
- j8: 64 / 256   = **0.25 / scan**  (= 64x)

失敗パス (cmp[entries] 不一致 → mask clear → 次候補 or 次 chunk) のサンプル増 +403
は profile 全体の +2.61 pp に相当し、35 ns/op 換算で **+0.92 ns/op**。j8 はほぼ毎 scan で
false match を 1 つ余計に処理している。

#### (c) 残差 +1.5 ns/op

(a) +1.2 + (b) +0.92 = +2.1 ns/op。観測の +3.8 ns/op との差 +1.7 ns/op はサンプル分布上
loop control (`add 0x10`) と lane extract (`shr $1`) の skid 増 (+225 sample = +1.5 pp =
+0.5 ns/op) + 不可分な OOO 余裕削減 (= dep chain が長くなったことで chunk 間の
overlap が削れる二次効果) で説明できる範囲。確定するには PEBS / LBR が必要だが、
WSL 環境では perf が無いので本稿では推定までとする。

### 4.5 含意

退行は **タグ bit 配分の変更による構造的コスト** で、データレイアウト (= entries arena
が scattered になる) は per_shard=64 では効いていない:

- (a) は id を tag に embed する以上、L1-hit 1 回が dep chain に必ず増える。回避するなら
  「id を別配列 (= §M5.3 素朴版)」に戻すしかないが、それはメモリ削減の主目的を捨てる
- (b) は hash bit 数を削った代償。**per_shard を下げれば false-match 期待値も比例で減る**
  ので、(b) は per_shard で消せる成分

たとえば per_shard=16 なら scan は 1 chunk = 16 lane 分のみで、false-match 期待値は
16/256 = 0.0625 (j7 は 16/16384 = 0.001)。Δ false-match per scan は 0.06 で、
+0.06 × ~7 cy ≈ +0.1 ns/op しか出ない (cap=4096 のときの +0.92 ns/op の 1/9)。
つまり **per_shard=16 まで下げれば退行は (a) 単独の +1.0 ns/op まで縮む** と予測できる。
これは「同 cap 比較で j7 と並ぶ位置」になる可能性があり、検証する価値が高い (§7 D')。

## 5. メモリ的位置付け (再掲)

|       | inline B/cap | 備考 |
|-------|------------:|------|
| orig  | 25          | linked-list ノード |
| j7    | 36          | tags 2× + entries 2× (slack 両側) |
| **j8**| **20**      | tags 2× + entries 1× (slack 片側) |

j8 の本来の狙いは「memory-fair で orig を抜く」(memfair で orig に 1.25× cap を渡しても j8
が勝つ)。今回の per_shard=64 固定 sweep は **同 cap 比較** なので、j8 のメモリ優位はまだ
表面化していない。memfair sweep (j8 cap=N vs orig cap=1.25N) は別レポートで扱う。

## 6. 結論

- **正しさは確認できた** (eviction 列が j7/sieve_orig と完全一致、22 unit + 2 oracle test green)。
- **throughput は机上検討より退行が大きい** (予測 +0.5〜+1 ns に対し実測 +1.9〜+4.1 ns)。
  cap=1024 では j8 はなお orig を 2.76 ns 上回るが、cap=4096/16384 では orig に負ける。
- 退行の真因は **dep chain 延長 (+~1.2 ns/op) + false-match 率 64x 増 (+~0.9 ns/op)** の
  2 つで、初稿が本命視していた「entries[id] が scattered で L1 prefetch 不発」は profile
  data で棄却された (per_shard=64 では 1 shard が L1 内に収まり、access pattern は throughput
  に効かない)。
- 「j7 を捨てて j8 に乗り換える」は **同 cap・per_shard=64 では正当化されない**。ただし:
  - **per_shard を下げると (b) 成分が消えるので退行は (a) の +1.0 ns/op まで縮む** 可能性
    あり。per_shard=16 が最有力候補
  - memfair (j8 inline 20 B/cap vs orig 25 B/cap、−20%) では話が変わる可能性が高い
  - 両者は別実験

## 7. 次のアクション候補 (改訂)

| # | アクション | 期待される情報 | 工数 |
|---|---|---|---|
| **A** | **memfair sweep**: j8 cap=N vs orig cap=⌈1.25 N⌉ を per_shard=64 固定で 5 trial | 「memory-fair で j8 が orig を抜くか」の決着 | 30 分 |
| **D'** | **per_shard sweep**: per_shard ∈ {16, 32, 64} で j7 vs j8 を比較。§4.5 の予測「per_shard ↓ で false-match 退行が消えて (a) 単独になる」を実測で確認 | (a) と (b) の分解の確証 + j8 sweet spot 確定 | 30 分 |
| E | cluster019 (scan-heavy / 低 hit) cell の比較 | hit ratio gain (+6.32 pp at cap=1024 in j5/j7) が ns/op としてどう寄与するか | 1 時間 |
| ~~B~~ | ~~perf counter (L1-dcache-load-misses) 取得~~ | ~~初稿の L1 prefetch 仮説検証~~ → samply で既に否定済 (§4.3)、不要 | — |
| ~~C~~ | ~~entries を tag pos 順に並べ直す変種~~ | ~~L1 miss 由来 vs id 抽出由来の切り分け~~ → §4.3 で前提が崩れたので不要 | — |

最も学びが大きいのは **D' (per_shard sweep)** + **A (memfair)** の 2 本。
D' は §4.5 の (a)/(b) 分解を実測で詰める作業、A は j8 設計の主目的への直接の答え。
両者は独立に走らせて良い。

## 付録 A — raw median 計算

5 trial の elapsed_ns を昇順ソートし index 2 を採用 (`sort -n | sed -n '3p'` 相当)。
1M 行 trace なので `ns/op = elapsed_ns / 1_000_000`。

| cell | 5 trial elapsed_ns (sorted) | median ns/op |
|---|---|---:|
| orig cap=1024 | 37279424, 37801728, **38329590**, 38554587, 39847607 | 38.33 |
| j7_n16 cap=1024 | 30260359, 31127182, **31481962**, 31672348, 33398375 | 31.48 |
| j8_n16 cap=1024 | 33275642, 35231364, **35573619**, 35857813, 36490430 | 35.57 |
| orig cap=4096 | 30188592, 30191656, **30465970**, 31306676, 31675519 | 30.47 |
| j7_n64 cap=4096 | 28718015, 29862477, **31094441**, 31372081, 31391373 | 31.09 |
| j8_n64 cap=4096 | 33596218, 33734026, **34892093**, 35105292, 36082058 | 34.89 |
| orig cap=16384 | 27109922, 28157185, **29085199**, 29753945, 32012602 | 29.09 |
| j7_n256 cap=16384 | 30426883, 30843122, **31506082**, 32496385, 32995256 | 31.51 |
| j8_n256 cap=16384 | 32989250, 33356876, **33426056**, 34212044, 35798707 | 33.43 |

## 付録 B — samply profiling 手順 (再現用)

```bash
# 反復で trace parse のオーバーヘッドを償却 (1M trace × 50 cap = 50M ops/run)
CAPS=$(yes 4096 | head -50 | paste -sd, -)
samply record --rate 8000 --save-only -o /tmp/j7_n64.json.gz --no-open -- \
  ./target/release/bench --source twitter \
    --path external/twitter-cache-trace/cluster018 \
    --capacity "$CAPS" --variant j7_n64 > /dev/null
# 同様に j8_n64 も
gunzip -fk /tmp/j7_n64.json.gz /tmp/j8_n64.json.gz

# 関数名 substring で絞って leaf address を addr2line で line に解決し集計
python3 /tmp/aggr.py target/release/bench /tmp/j7_n64.json find_avx2
python3 /tmp/aggr.py target/release/bench /tmp/j8_n64.json find_avx2
```

`/tmp/aggr.py` は以下を行う簡易スクリプト:

1. samply JSON の各 leaf frame の (lib, address) を取得
2. メイン binary に属するアドレスだけ集計 (`stringArray` / `funcTable` / `resourceTable` 経由)
3. `addr2line -e <binary> -f -C -i 0xADDR` で innermost (関数名, file:line) に解決
4. 関数名 substring (例: `find_avx2`) でフィルタし、(loc, addr) 単位で sample 数を降順表示

samply は `target/release/bench` のシンボル名を解決し切れない (frame table に hex
アドレスしか入らない) ことがあるので、addr2line 経由の解決が必要。
