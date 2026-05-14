# concurrent scalar fallback — perf-gate

`senba::concurrent::Cache` の reader 走査を AVX2+BMI1 と portable scalar の runtime dispatch に分けた commit `94e3269` の perf-gate run。CLAUDE.md「Concurrent perf-gate」契約 (>5% regression on any cell = commit-blocker) を満たすかを確認する。

## 仮説

scalar fallback を加えるために以下の構造変更が走った:

- `Cache` に `has_avx2_bmi1: bool` field を追加 (layout 変化、`u64` alignment 周りに padding 移動の可能性)。
- public hot path (`contains` / `get_by_hash` / `insert`) と内側の `Shard::insert` / `try_path_a` / `find_get` / `find_lockfree_for_path_a` 全てに `bool` 引数 1 本追加。レジスタ pressure と call ABI に薄く乗る。
- `find_get` / `find_lockfree_for_path_a` を `#[inline]` dispatcher に置き換え、`if has_avx2_bmi1 { return ...avx2(...) } else { ...scalar(...) }` の form に。LLVM が定数 fold で直接 call に落とせれば AVX2 path は zero-cost、しなければ branch + register move が乗る。

「AVX2 path は bit-for-bit 同一の `#[target_feature(enable="avx2,bmi1")]` body のまま」なので intrinsic 系列は不変。退行が出るとしたら struct layout / call ABI / dispatcher 折りたたみの失敗の 3 経路に絞られる。これらが合計で 5% 以下に収まるか。

## 計測

Host: WSL2 Linux x86_64, AVX2 + BMI1 present (`is_x86_feature_detected!("avx2") == true`)、`SENBA_FORCE_SCALAR` 未設定 → AVX2 path が実行される。toolchain `rustc 1.95.0 (59807616e 2026-04-14)`。

手順:

```bash
git checkout HEAD^                                                          # 0e86a09 (pre-scalar, src/concurrent は scalar 導入前)
cargo bench -p senba-research --bench sieve_concurrent_perf -- --save-baseline before
git checkout main                                                           # 94e3269 (scalar dispatch in)
cargo bench -p senba-research --bench sieve_concurrent_perf -- --baseline before
```

`research/benches/sieve_concurrent_perf.rs` は HEAD^ → HEAD で fmt-only drift (`Throughput::Elements((a*b) as u64)` 1 行化のみ、bench group 名・iteration count・seed・cap・shards 全て不変)、criterion baseline の互換性に影響しない。

## 結果

4 cell 全て `> 5%` 規制値を下回り、commit-blocker 到達なし。

| Cell | median Δtime | median Δthrpt | criterion verdict |
|---|---|---|---|
| `zipf1_u64_t4/4096`     | **+1.29%** | −1.27% | within noise threshold |
| `zipf14_u64_t16/4096`   | **+2.73%** | −2.66% | within noise threshold |
| `zipf1_string_t4/4096`  | **+1.11%** | −1.10% | within noise threshold |
| `zipf14_string_t16/4096`| **+0.57%** | −0.56% | no change (p = 0.52) |

絶対値: baseline → HEAD で `zipf1_u64_t4` 3.226 → 3.258 ms、`zipf14_u64_t16` 3.359 → 3.464 ms、`zipf1_string_t4` 6.606 → 6.704 ms、`zipf14_string_t16` 8.945 → 8.910 ms (string T=16 は逆に微改善側にも振れる範囲)。worst は `zipf14_u64_t16` の +2.73% で、5% gate の半分強。

Pattern 観察:

- **退行が u64 cell に集中** (+1.29% / +2.73%) し、**string cell ではほぼ消える** (+1.11% / +0.57%)。string path は per-op の `V::clone` + epoch defer が dominate するので、dispatcher 由来の薄い branch overhead が相対的に希釈される。u64 cell は per-op コスト自体が小さく (3.2–3.4 ms / 400k op = ~8 ns/op)、+0.1 ns/op の dispatcher branch が直接見える。
- **T=16 (zipf14_u64_t16) で最大** (+2.73%)。これは hot shard に CAS 競合する Path A の writer-side でも dispatcher が乗る (`try_path_a` → `find_lockfree_for_path_a`) ため、reader と writer 双方の overhead が積まれる cell。

3 cell の "within noise threshold" は criterion 自身の `noise_threshold(0.02)` 設定 (bench file 47 行) によるもので、retire していない通常の variance に分類された。`p < 0.05` で統計的に検出はされたが、実害域ではない。

## 結論

- CLAUDE.md gate (>5% regression on any cell = commit-blocker) は **PASS**。
- 退行構造は予想通り「dispatcher 追加の薄い branch overhead が u64 hot path に直接表面化」のみ。layout 変化 (`has_avx2_bmi1: bool` field 追加) の影響は識別できない (測定差は dispatcher 寄与で十分説明できる)。
- AVX2 path の codegen は壊れていない (intrinsic body は不変、cells の絶対値が 1-3% 程度のずれに収まる = SIMD 走査自体は等価で走っている)。

## Follow-ups

- `SENBA_FORCE_SCALAR=1` で scalar twin 単独の絶対 cost を測る perf-gate 派生 run は別 sweep の話題 (gate ではなく characterisation)。AVX2 absent の host の現実的下限を出す意味で別 report で取りたい。
- review で指摘した残件 (scalar twin の Path A double-load asymmetry の在地 doc 化、`SENBA_FORCE_SCALAR` env lookup の `OnceLock` cache 化、aarch64 cross-check の CI 追加) は perf 質的影響なしの低優先 polish なので分離して扱う。
