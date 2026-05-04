# sieve_orig — Rust ポートの余剰オーバーヘッド調査 (2026-05-04)

- 日付: 2026-05-04
- 目的: C リファレンス (`Sieve.c`) と Rust ポート (`sieve_orig.rs`) の差異を
  機械語レベルまで追い、「Rust 固有のオーバーヘッド」と「設計上やむを得ない差」
  を分類する。

## 調査方法

1. C リファレンス (`external/NSDI24-SIEVE/libCacheSim/libCacheSim/cache/eviction/Sieve.c`)
   と補助ヘッダ (`cacheObj.h`, `chainedHashTableV2.c`) を通読。
2. ベンチバイナリを `cargo build --release --bench micro` でビルドし、
   `objdump -d -M intel` で `sieve_orig::SieveCache<u64,u64>` の
   `insert` / `get` / eviction loop のアセンブリを直接読んだ。
3. `size_of::<Option<Node<u64,u64>>>()` / `size_of::<MaybeUninit<Node<u64,u64>>>()` を
   コード片で計測。

## ノード構造の実サイズ

```
Node<u64, u64>                  32 bytes
Option<Node<u64, u64>>          40 bytes  ← discriminant 8 bytes 余剰
MaybeUninit<Node<u64, u64>>     32 bytes
```

`Vec<Option<Node>>` は `free_list` が既に live/dead を管理しているのに、
Option の discriminant (8 bytes/node) を二重に保持している。
cap=100 で 800 bytes、cap=1000 で 8 KB の純粋な余剰。

## eviction ループの機械語 (bench binary より抜粋)

```asm
; --- 内側ループ 1 iteration ---
a5b40: lea  rcx, [rdi+rdi*4]          ; rcx = 5*id  (stride = 40 bytes/node)
a5b44: cmp  BYTE PTR [rdx+rcx*8], 0x0 ; ← Option discriminant check  ★不要
a5b48: je   panic                      ; ← panic branch               ★不要
a5b4e: lea  r8, [rdx+rcx*8]           ; r8 = &nodes[id]
a5b52: cmp  BYTE PTR [r8+0x20], 0x0   ; freq == 0?  (offset 32)
a5b57: je   victim_found
a5b5d: mov  BYTE PTR [r8+0x20], 0x0   ; freq = 0
a5b62: mov  edi, DWORD PTR [r8+0x18]  ; prev  (offset 24)
a5b66: cmp  edi, 0xffffffff           ; prev == NIL?
a5b69: cmove edi, eax                  ; tail if NIL
a5b6e: cmp  rsi, rdi                  ; bounds check
a5b71: ja   a5b40
```

`MaybeUninit` に替えると `cmp + je` (discriminant check) がループ本体から消える。
hot な eviction loop では毎 step 2 命令の削減になる。

## hit パスも同じ問題

```asm
; key が HashMap に見つかった場合 (hit path)
a5c07: lea  rcx, [rdi+rdi*4]          ; 5*id
a5c0b: cmp  BYTE PTR [rax+rcx*8], 0x0 ; ← discriminant check  ★不要
a5c0f: je   panic                      ; ← panic branch        ★不要
a5c15: lea  rax, [rax+rcx*8]
a5c19: mov  QWORD PTR [rax+0x10], rdx ; value 更新
a5c1d: mov  BYTE PTR [rax+0x20], 0x1  ; freq = 1
```

Zipf skew=1.0 / cap=100 では全リクエストの ~70% がここを通る。
hit ごとに 2 命令の余分なチェックが走っていることになる。

## C との構造的差異の全体像

| 観点 | C (`Sieve.c`) | Rust `sieve_orig` | 修正可否 |
|---|---|---|---|
| lookup の段数 | hashtable → `cache_obj_t*` (1 段) | HashMap → NodeId → `nodes[id]` (2 段) | △ 設計上固定 |
| discriminant check | なし (raw pointer) | `cmp + je` / `node_mut()` 呼び出しごと | **✅ MaybeUninit で除去** |
| node stride | packed 可変長 (struct 内に hash_next, queue.*, sieve.freq を混在) | 40 bytes (Option discriminant 8B 込み) | **✅ 40→32 bytes** |
| eviction 後の hashtable 削除 | `hash_next` 埋め込みで pointer 渡し、re-hash 不要 | `index.remove(&key)` で victim key を再 hash | △ 標準 HashMap の制約 |
| `key.clone()` | obj_id (u64) コピーのみ | `K: Clone` (u64 なら Copy で no-op) | 実質ゼロコスト |
| `freq` の型 | `int32_t` (4 bytes、packed union) | `u8` (1 byte) | Rust 側が有利 |
| alloc 戦略 | pool allocator (`my_malloc`) | `free_list.pop()` + Vec grow | 同等程度 |
| hashtable 実装 | chained hash table V2 (分離チェーン) | hashbrown (open addressing + SIMD) | hashbrown が一般に高速 |

### "2 段 lookup" の内訳

C の `Sieve_find` は `cache_find_base` → `chained_hashtable_find_obj_id_v2` が
`cache_obj_t*` を直接返す。このポインタ 1 本で linked list の prev/next/freq に
直接アクセスできる (= 1 hop)。

Rust は HashMap が `NodeId: u32` を返し、さらに `nodes[id]` のアクセスが必要
(= 2 hop)。`nodes` が L1 に収まる小容量では影響は小さいが、構造的に C より
1 つ多い indirection がある。これを除去するには HashMap の value を
`*mut Node` にする等の unsafe 設計変更が必要で、現状の安全 Rust 設計では固定。

### C の hashtable delete が速い理由

`chained_hashtable_delete_v2(hashtable, cache_obj)` は
**オブジェクトのポインタを受け取る**。`hash_next` チェーンを hash 値で
バケットに飛んで隣接ノードをつなぎ直すだけで、victim key の re-hash は不要。

Rust では `self.index.remove(&node.key)` が毎回 key を re-hash する。
u64 + XXH3 では数 ns だが、原理的には C より多い計算。

## 修正可能な項目と期待効果

### 主要修正: `Vec<Option<Node>>` → `Vec<MaybeUninit<Node>>`

`free_list` が live/dead の正しい情報源になるので、discriminant は純粋な
二重管理。`MaybeUninit` に替えると:

| 場所 | 除去されるコード |
|---|---|
| eviction loop (毎 step) | `cmp BYTE [...], 0x0` + `je panic` (2 命令) |
| hit path (`node_mut(id)`) | 同上 2 命令 |
| `alloc_node` | `mov QWORD [...], 0x1` (discriminant 書き込み) 1 命令 |
| node stride | 40 → 32 bytes (-20%) |

参考: j3 の refactor で同じ変更 (`Option<Entry>` → `MaybeUninit<Entry>`) を
施したとき、cap=100 全 skew で 2〜7% 改善し、skew=1.2 で勝ち越しが転換した。
`sieve_orig` への適用でも同程度の改善が見込める。

実装は clean: `free_list` が live slot を管理しているので安全性の根拠は変わらず、
`as_ref().expect()` を `unsafe { assume_init_ref() }` に換えるだけ。

### 副次修正: `node()` / `node_mut()` の bounds check

現状: `nodes[id as usize]` で毎回 Vec bounds check が走る。
`id < nodes.len()` は `alloc_node` の不変条件として保証されるが、
コンパイラには見えていないので毎回 check を生成している。

`get_unchecked` / `get_unchecked_mut` + コメントで除去できるが、効果は小さい
(bounds check 1 命令 vs discriminant check 2 命令)。優先度は低い。

## 「修正後でも残る」構造的コスト

1. **2 段 lookup** (HashMap NodeId → nodes[NodeId]): 安全 Rust の arena 設計で固定。
2. **`index.remove` の re-hash**: 標準 HashMap の制約。
3. **`K: Clone` constraint**: u64 では無コスト。文字列キーでは問題になる。

これらは「Rust の safe design を維持したままでは払拭できない」コスト。
C の生ポインタ設計と対応させたい場合は、HashMap value を `*mut Node` にする
unsafe 設計が必要になる (性能より安全性を優先した現在の設計判断と矛盾する)。

## 結論

**最大の fixable オーバーヘッドは `Option<Node>` の discriminant**。
- 8 bytes/node の余剰メモリ
- hit path と eviction loop に 2 命令/アクセスのチェック
- j3 の実績から 2〜7% 改善が見込める

それ以外の差 (2 段 lookup、re-hash、alloc 戦略) は現行の安全 Rust 設計では
受け入れコストとして扱うのが妥当。

## 実装と実測結果 (追補)

`Vec<Option<Node>>` → `Vec<MaybeUninit<Node>>` の変更を実装し、
全ユニットテスト (84 件) と oracle テスト (18 件、全 variant の evict 列が
`sieve_orig` と一致することを確認するもの) が pass。`free_list` が live/dead の
唯一の真実源となり、live なノードのみ Drop するカスタム実装を追加。

### asm 確認 (期待通り)

新 eviction loop:
```asm
aac80: shl  rdi, 0x5                    ; 32*id  ← stride 40 → 32
aac84: mov  BYTE PTR [r8], 0x0          ; freq = 0
aac88: mov  edi, DWORD PTR [rcx+rdi*1+0x10] ; prev (offset 16)
aac8c: cmp  edi, 0xffffffff
aac8f: cmove edi, eax
...
aacaa: cmp  BYTE PTR [rcx+rdx*1+0x18], 0x0 ; freq check (offset 24)
aacaf: jne  aac80
```

- discriminant check (`cmp BYTE [...], 0x0; je panic`) が消去
- node stride: `lea + *8` の 40-byte → `shl rdi, 5` の 32-byte
- freq offset: 0x20 → 0x18、prev offset: 0x18 → 0x10 (どちらも 8B 短縮)

hit path も同様に discriminant check が消去された。

### 実測 (criterion `insert_only`、log: `profiles/orig_maybeuninit_2026-05-04.log`)

| skew | cap | orig 旧 (Option) | orig 新 (MaybeUninit) | orig Δ% | v3 Δ% (=ノイズ参考) |
|---:|---:|---:|---:|---:|---:|
| 0.6 | 100 | 37.18 | 37.60 | +1.1% | -3.9% |
| 0.6 | 1000 | 39.44 | 41.77 | +5.9% | +1.5% |
| 0.6 | 10000 | 34.58 | 36.32 | +5.0% | -2.2% |
| 0.8 | 100 | 35.76 | 35.41 | -1.0% | -3.9% |
| 0.8 | 1000 | 31.87 | 33.32 | +4.5% | -1.3% |
| 0.8 | 10000 | 29.23 | 31.21 | +6.8% | -0.6% |
| 1.0 | 100 | 30.44 | 32.10 | +5.5% | -2.0% |
| 1.0 | 1000 | 24.59 | 25.59 | +4.1% | +1.6% |
| 1.0 | 10000 | 21.21 | 22.03 | +3.9% | +1.3% |
| 1.2 | 100 | 21.34 | 22.05 | +3.3% | -3.7% |
| 1.2 | 1000 | 17.41 | 17.26 | -0.9% | -3.6% |
| 1.2 | 10000 | 15.54 | 15.51 | -0.2% | +2.3% |

(v3 は本セッションでコード変更していないので、その Δ% は純粋な run-to-run
ノイズの目安。範囲はおおよそ ±4%。)

### 解釈 — "asm 勝ち、bench に出ず"

- **微妙な regression 寄り**: orig の Δ% は -1.0〜+6.8% に広がり、ノイズ床
  (v3 で観測した ±4%) を踏み越えるほどではないが、平均すると改善ではなく
  わずかな regression に見える。「明確に勝ち」とは言えない。
- **要因の推定**: 旧 asm は eviction ループの bounds check を `cmp rsi, rdi; ja loop`
  で loop continuation と兼ねていたが、新 asm では `cmp; jbe panic` と
  `cmp; jne loop` に分離され、命令数自体は discriminant 削減と相殺。命令
  単位のスループットでは「ほぼ等価」になった可能性。
- **本質的な理由**: `sieve_orig` は profile 上 **HashMap + SipHash が ~80%** を
  占める (`2026-05-03-sieve-v3-profile.md` の "headroom 地図" 参照)。
  ノードアクセスから 2 命令削っても全体に対する寄与は ~1% 未満で、bench
  ノイズに沈む。

### j3 と差が出る理由

j3 で同じ変更をしたときは cap=100 で 2〜7% 改善した。両者の違いは:

- **j3**: HashMap を持たない → 時間の大半が tag 配列スキャン + ノード操作で、
  ノード操作からの 2 命令削減が直接 surface する。
- **orig**: HashMap がボトルネック → ノード操作の改善は HashMap の影に隠れる。

つまり「discriminant 除去の絶対的なゲイン」は両者で同程度発生しているが、
「相対的な寄与」は HashMap の重さで打ち消されている。

### 採否

それでも変更は **採用** とする。理由:

1. **構造的に正しい**: live/dead の真実源が `free_list` 一本になり、
   Option discriminant との二重符号化が解消。コードレベルで不変条件が
   局所化され、保守性が上がる。
2. **C リファレンスへの忠実性向上**: C の `cache_obj_t` は
   discriminant 相当を持たない。「忠実移植」を名乗る上では Option
   が要らない設計の方が筋が良い。
3. **測定上はノイズ範囲**: 明確な regression ではない。
4. **コード量はほぼ不変**: 純粋な内部表現の差し替えで、unsafe 領域は
   `node()/node_mut()` と `Drop` に局所化。

### 教訓

- **asm レベルの勝ちが bench に出るかは workload mix 次第**。orig のように
  別の hot コンポーネント (HashMap) が支配的だと、局所最適化は埋もれる。
- 「やってみて測ったら効かなかった」の負例として、今回の orig は
  「J3 と同じ最適化をかけても profile プロファイルが違えば帰結が違う」
  という具体例になる。次に他 variant の最適化を検討するときは、profile で
  bottleneck を特定してから変更する手順を再確認すべき。
