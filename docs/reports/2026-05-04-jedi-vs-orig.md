# jedisct1/rust-sieve-cache 設計調査 (2026-05-04)

- 日付: 2026-05-04
- きっかけ: Rust で書かれた既存 SIEVE 実装として有名な
  [`jedisct1/rust-sieve-cache`](https://github.com/jedisct1/rust-sieve-cache) を読み、
  本プロジェクトの `sieve_orig` (NSDI'24 著者参照ポート) と比較して
  「Rust 実装としてどんな選択肢を取っているか」「`sieve_orig` の代替オラクル
  として使えるか」を確認する。
- ステータス: ソース読みと挙動分析まで完了。詳細な実測 (ヒット率比較・トレース
  比較) は **後回し**。理由は本文末尾。
- submodule: `external/rust-sieve-cache/` (this commit でステージ済み)

## jedisct1 の設計

`external/rust-sieve-cache/src/lib.rs` のコア型 (lib.rs:107–183):

```rust
struct Node<K: Eq + Hash + Clone, V> {
    key: K,
    value: V,
    visited: bool,
}

pub struct SieveCache<K: Eq + Hash + Clone, V> {
    map: HashMap<K, usize>,        // key -> nodes 配列のインデックス
    nodes: Vec<Node<K, V>>,        // 連結リストではなく単純 Vec
    hand: Option<usize>,           // nodes インデックス
    capacity: usize,
}
```

走査と立ち退き (lib.rs:603–682) の要約:

- 走査開始: `hand.unwrap_or(0)`
- 走査方向: **インデックス昇順** (`current_idx + 1`、末尾で 0 に wrap)
- 立ち退き: **`swap_remove` 相当** (末尾 Node を空き idx に移し、`map` を更新)
- 立ち退き後の hand: `Some(idx)` (= 末尾から降りてきた Node の現在位置)

挿入 (lib.rs:452–485):

- key 重複時は `visited=true` だけ立てて値を更新
- 容量超過時は `evict()` で 1 件追い出してから `nodes.push(node)`
- 新規 Node は **末尾** (= 最大インデックス) に置かれる

## `sieve_orig` (paper-faithful) との違い

| 観点 | `sieve_orig` (C ref) | jedisct1 |
|---|---|---|
| キュー構造 | 双方向リンクリスト (head=新, tail=旧) | `Vec<Node>` (ポインタなし) |
| hand の初期位置 | tail (最古) | idx 0 |
| hand の進行方向 | tail → head (`prev` で辿る) | idx 昇順 + wrap |
| 新規挿入位置 | head に prepend (hand から最遠) | 末尾 push |
| 立ち退きでの整理 | リストから unlink (他要素位置不変) | `swap_remove` で末尾要素が空き位置にジャンプ |
| 立ち退き後の hand | 立ち退いた要素の `prev` (= 1 つ head 寄り) | 立ち退いた idx (= 降りてきた末尾要素を指す) |

最後の 2 行が決定的に重要で、立ち退きごとにキュー上の **「相対順序」が局所的
に破壊される**。同じトレースを流しても `sieve_orig` と evicted-key 列が
一致しないので、CLAUDE.md の正しさ基準 (「`sieve_orig` と evicted-key 列が
一致」) では **不合格**。SIEVE 論文の挙動定義からも乖離する。

## 順序破壊の影響: 「ミス連発で CLOCK 化する SIEVE」

### 最小例

cap=4、走査は idx 昇順 wrap、`hand=0`、全 `visited=0` から:

```
state: [a, b, c, d]   (a 最古, d 最新)
miss → evict a → swap_remove で d が idx 0 へ
state: [d, b, c]      hand=0 (= d)
push e:
state: [d, b, c, e]
```

ここで `d` は元々「最新側」だったのに、scan 順では先頭 (= 最古扱い)。
`d.visited=0` なら次のミスで即 evict 候補。さらにミスが来た場合:

```
miss → evict d → swap_remove で e が idx 0 へ
state: [e, b, c]      hand=0 (= e)
push f:
state: [e, b, c, f]
miss → evict e (visited=0 のまま) ...
```

**連続ミス下では「直前に push したばかりの新規」が次々と hand 位置に降ろされ、
再アクセスを挟まない限りそのまま evict される**。これは CLOCK の「新規にgrace
period がない」性質そのもの。

### 三者比較

| 軸 | CLOCK | jedisct1 | SIEVE (paper) |
|---|---|---|---|
| 新規挿入の位置 | hand 位置 | vec 末尾 (hand から遠い側) | head (hand から最遠) |
| 立ち退き後 | hand はその場、新規が来る | swap_remove で末尾要素を hand 位置に引きずり下ろす | リストから unlink、他要素不変 |
| 新規アイテムの grace period | ほぼゼロ | **通常は長い、ただし swap で潰れることがある** | 最大 (head→tail を hand が辿る間) |

→ jedisct1 は **「定常 hit 多めなら SIEVE 寄り、ミスバースト下では CLOCK 寄りに
縮退する」ハイブリッド**。論文 SIEVE が CLOCK に対して優位を主張する場面 (scan
耐性、新規保護) でこの優位を一部失っている。

### ワークロード別の予想 (未実測)

| ワークロード | 予想される影響 |
|---|---|
| Zipfian / skewed (SIEVE 本領) | 差は小さい (推定 0–2pt)。ホットキーは visited=1 を素早く積み swap victim でも 1 巡で生き残る |
| recency-heavy (LRU 的) | grace period 縮退で早期 eviction 増 → ヒット率低下 |
| scan-heavy (scan resistance test) | jedisct1 が劣化。新規が swap で hand 位置に降ろされ scan 耐性が弱まる |
| 全 visited=1 ケース | 同等 (1 巡目クリア → 2 巡目で evict、構造的な差は出ない) |

## Rust 実装の参考としての評価 — 「変な方向の知見」

本来この調査の動機は **「Rust で SIEVE を書くときに HashMap 層・キュー構造を
どう実装するか」の実例を見たい** だった。だが jedisct1 は senba-cache が苦戦
している部分について新規の示唆をくれない:

- **HashMap 層**: `HashMap<K, usize>` (std default = SipHash + RandomState)。
  本プロジェクトの `samply_phases.py` プロファイルで HashMap 層は CPU 時間の
  80–89% を占めており、まさにこの層に headroom があるはず — だが jedisct1
  は最も素朴な使い方をしている。`K` は `Node` 側にも `map` 側にもコピーされ
  (lib.rs:479, 482)、合計 2 部の `K` を保持する。これは sieve_orig も同じ。
- **キュー構造**: 連結リストを捨てて `Vec` にしたのは Rust らしい選択ではある
  が、その代償として **アルゴリズムの順序不変量を壊している**。「Rust で連結
  リストを書きたくない」という言語側都合のために、SIEVE が CLOCK に対して
  もっていた性質の一部をトレードしている。
- **アルゴリズム≠Rust 表現の違い**: 結果として、jedisct1 は「同じ SIEVE を
  Rust で書き直したもの」ではなく **「Rust で書きやすいように変形した別アル
  ゴリズム」** に近い。本プロジェクトが探したかった「同じアルゴリズムの
  Rust 流の実装テクニック」は得られなかった。

つまり「Rust 実装ガイドライン」としての参照価値は低く、むしろ「SIEVE を
Rust で素朴に書くと、こうやって性質を失いがち」という反面教師に近い。

## 決定事項

- 詳細な挙動評価 (`sieve_jedi.rs` を起こしてベンチ) は **後回し** にする。
  - 理由: 上記の通り「Rust 実装テクニックの引き出し」としては学びが薄く、
    性質も別物。比較ベンチを取っても senba-cache の主目的 (sieve_orig 比で
    どう速くするか) には直接フィードバックしない。
  - やる場合の見立ては書いてあるので、興味が出たら拾える状態にしておく:
    Zipfian / scan / recency の 3 トレースで evicted-key divergence と
    ヒット率を測れば「ハイブリッドの境界」を可視化できるはず。
- submodule (`external/rust-sieve-cache`) は残す。読み直したくなったとき
  すぐ参照できるように。

## 次に向くべき方向 (この調査からの含意)

1. **HashMap 層の改善は jedisct1 に頼れない** — `2026-05-04-improvement-ideas.md`
   の軸 A (HashMap 層) は引き続き senba-cache 内部で詰めるしかない。
2. **「Rust らしさ」と「アルゴリズム忠実性」のトレードオフを明示する** —
   sieve_orig が `Vec<MaybeUninit<Node>>` + `u32` インデックスで連結リスト
   を実現しているのは、Rust 側の都合に妥協せずアルゴリズムを保つための
   選択。jedisct1 の轍を踏まないという意思表示として価値がある。
3. **新変種を作るときの足切り基準の再確認** — 「`sieve_orig` と evicted-key
   列一致」は単なる回帰テストではなく **アルゴリズム同一性の証明**。
   jedisct1 のように一致しない変種を「SIEVE 実装」と呼ぶかどうかは、命名
   と性質を分けて議論する必要がある。
