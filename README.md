# senba-cache

A Rust sandbox for exploring good implementations of the **SIEVE** cache eviction algorithm (NSDI'24, Zhang et al.).

The goal is not to ship a single "best" cache, but to develop several SIEVE variants in parallel — starting from a faithful port of the authors' reference C code, then iterating toward Rust-idiomatic and performance-tuned designs — and to study the trade-offs between them.

- Paper: <https://yazhuozhang.com/assets/publication/nsdi24-sieve.pdf>
- Authors' reference repo: <https://github.com/cacheMon/NSDI24-SIEVE> (included as a git submodule at `external/NSDI24-SIEVE/`)

## Variants

| Module | Role | Internal data structure |
|---|---|---|
| `src/sieve_orig.rs` | **Reference port.** A line-by-line Rust port of `Sieve.c` from libCacheSim. Treated as the spec / oracle — every other variant must reproduce its eviction behavior. | Arena (`Vec<Option<Node>>`) + `Option<NodeId>` doubly-linked list + single `hand` pointer + per-entry `freq` (visited bit). |
| `src/sieve_v0.rs` | First experimental variant. Replaces the linked list with a contiguous logical queue + tombstones + periodic compaction. | `Vec<Option<EntryId>>` queue + `BitSet` for `visited`/`tombstone` + periodic `compact()`. |

All SIEVE modules expose the same minimal API so they can be swapped in benchmarks:

```rust
let mut cache = SieveCache::new(capacity);
cache.insert(key, value);            // -> Option<(K, V)>  (returns evicted on overflow)
cache.get(&key);                     // -> Option<&V>      (sets visited bit on hit)
cache.contains_key(&key);
cache.len(); cache.capacity();
// sieve_orig also: cache.remove(&key) -> Option<V>
```

## Build / test

```bash
cargo test                 # all unit tests
cargo test sieve_orig      # just the reference port
cargo test sieve_v0        # just the v0 variant
cargo clippy
```

## Project layout

```
src/
  cache.rs            # Cache<K,V> trait (placeholder; not yet a fit for SIEVE — see CLAUDE.md)
  error.rs            # Error / Result
  lib.rs              # module declarations and re-exports
  sieve_orig.rs       # faithful NSDI'24 reference port  ← oracle
  sieve_v0.rs         # tombstone + compaction variant
benches/
  micro.rs            # criterion micro benchmarks (insert-only over Zipf trace)
scripts/
  criterion_compare.py  # criterion 結果を集計して orig vs v0 の表にする
  samply_top.py         # samply 出力 JSON から hot 関数を抽出
  samply_lines.py       # samply 出力 JSON を addr2line でソース行に解決
external/
  NSDI24-SIEVE/         # git submodule: authors' libCacheSim repo (read-only reference)
```

## Benchmarks

`benches/micro.rs` は criterion ベースで、`insert_only` (Zipf トレースを `cache.insert` で連続投入) を `(skew, capacity)` の組合せで回す。設定は NSDI'24 SIEVE 論文 §5.3 / §6.1 の synthetic Zipf 実験に寄せてある (詳細は `docs/sieve-paper-workload.md`):

- skew α ∈ {0.6, 0.8, 1.0, 1.2}
- footprint N = 100,000 ユニーク object
- trace 長 = 1,000,000 リクエスト (= footprint の 10x)
- キャッシュ容量 = footprint の {0.1%, 1%, 10%} = {100, 1000, 10000}

```bash
cargo bench --bench micro                    # 全ケース実行
cargo bench --bench micro insert_only        # フィルタ
cargo bench --bench micro -- --profile-time 5 'insert_only/v0/skew1/10000'
                                              # サンプリングをスキップして 5 秒間ループだけ回す
                                              # (プロファイラを当てるとき用)
```

結果は `target/criterion/<group>/<case>/new/estimates.json` に残る。4 実装を並べた比較表を出すには:

```bash
python3 scripts/criterion_compare.py
```

`benches/micro.rs` の定数 (`SKEWS`, `CAP_RATIOS`, `N_KEYS`, `TRACE_LEN`) を変えたら、`scripts/criterion_compare.py` の同名定数も合わせる。

## Profiling (samply)

> SIEVE 同士の差は数 % 〜数 10 % のオーダなので、関数単位ではなく**機械語/ソース行レベル**まで降りないと差分の出所が見えない。samply (Firefox Profiler 互換のサンプリングプロファイラ) を使う。

### 1 回だけの準備

```bash
cargo install samply

# samply / perf は perf_event_open を使うので、unprivileged な実行のために
# perf_event_paranoid を 1 以下に下げる (再起動まで有効)。
echo 1 | sudo tee /proc/sys/kernel/perf_event_paranoid
# 恒久化したい場合: /etc/sysctl.d/99-perf.conf に kernel.perf_event_paranoid=1
```

`Cargo.toml` には既に `[profile.release].debug = "line-tables-only"` が入っているので、`addr2line` でソース行が解決できる (DWARF はフルではなく行テーブルのみで十分)。

### 取得

```bash
# まず bench バイナリを作る
cargo bench --bench micro --no-run

# 出力された micro-XXXX のパスをメモ
BIN=$(ls -t target/release/deps/micro-* | grep -v '\.d$' | head -1)

# 該当ケースを 8 秒だけ回してプロファイルを保存
mkdir -p profiles
samply record --save-only -o profiles/v0_worst.json --rate 4000 -- \
  "$BIN" --bench --profile-time 8 'insert_only/v0/skew1/10000'

samply record --save-only -o profiles/orig_worst.json --rate 4000 -- \
  "$BIN" --bench --profile-time 8 'insert_only/orig/skew1/10000'
```

`--profile-time SECS` は criterion のフラグで、warm-up / 統計分析を全部スキップしてループだけを回す。サンプリング対象を 1 ケースに絞り込むのに必須。

### ブラウザで見る (フレームグラフ / call tree / inverted tree)

```bash
samply load --no-open --port 3000 profiles/v0_worst.json    # v0
samply load --no-open --port 3001 profiles/orig_worst.json  # orig
```

`samply load` は `Local server listening at http://127.0.0.1:PORT` と完全 URL (`https://profiler.firefox.com/from-url/...?symbolServer=...`) を出す。**WSL2 でも localhost は Windows 側に転送される**ので、その URL を Windows のブラウザに貼ればそのまま開く。シンボルは samply の symbol server が要求時に解決して返す。

UI で押さえるところ:
- **Flame Graph / Stack Chart** — 呼び出し階層と各関数の自時間
- **Call Tree → Inverted** — leaf 視点。「どの std/core 関数で時間が燃えているか」を直接ランキング
- **時間範囲ドラッグ** — `iter_batched` の cache 構築コストを除外したいとき、定常区間だけ選択する

### テキストでざっと見る

`profiles/*.json` から手元で集計するスクリプトを 2 種類用意してある。

```bash
# leaf address ベースで自時間 top 関数 (シンボルあり)
python3 scripts/samply_top.py target/release/deps/micro-XXXX \
        profiles/v0_worst.json profiles/orig_worst.json

# inline 階層をたどってソース行単位に集計し、
# sieve_v0.rs / sieve_orig.rs の hot lines + カテゴリ別比較を出す
python3 scripts/samply_lines.py
```

`samply_lines.py` は `addr2line -f -C -i` を 1 アドレスずつ呼ぶので、ユニークアドレスが数百あるとそれなりに時間がかかる (数十秒)。`BIN` パスはスクリプト先頭の定数を編集する。

### 命令単位まで降りる

UI の Call Tree からホット関数を選んで「Show in source view」すると、samply がローカルファイルを開いて行ごとのサンプル数を重ねる。さらに「Show in disassembly view」で**命令ごとのサンプル分布**が出る (ELF の DWARF を使ってくれる)。これが現在のところ Linux で「**ソース ↔ 機械語のどの命令が熱いか**」を最短で見る方法。

## Reference

```bibtex
@inproceedings{zhang2024-sieve,
  title={SIEVE is Simpler than LRU: an Efficient Turn-Key Eviction Algorithm for Web Caches},
  author={Zhang, Yazhuo and Yang, Juncheng and Yue, Yao and Vigfusson, Ymir and Rashmi, K.V.},
  booktitle={USENIX Symposium on Networked Systems Design and Implementation (NSDI'24)},
  year={2024}
}
```
