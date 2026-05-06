# Twitter trace を生 String キーのまま流す経路の追加

date: 2026-05-06
status: implemented

## 背景

`senba::Cache<K, V, S: SlotSize, SHARDS>` (= `src/sieve_cache.rs`) が任意 K,V を受け取れるようになったことで、Twitter cache trace (OSDI'20) の anonymized_key を **u64 に pre-hash することなく** `Cache<String, u64>` にそのまま流せるはずだ、という動機で経路を追加した。

これまでの bench harness (`src/bin/bench.rs`) は `drive::<C: CacheImpl<u64, u64>>` として u64 ハードコードされており、`twitter_csv_from_path` (`src/workload/file.rs`) が String キーを `DefaultHasher::finish()` で u64 に潰してから iterator に流す形になっていた。pre-hashing は実装側の制約ではなく純粋にハーネス側の都合なので、`senba::Cache` の汎化が完了した今は取り払える。

## 変更点

1. **`src/sieve_cache.rs`**: `impl<K, V, S, const SHARDS> CacheImpl<K, V> for Cache<K, V, S, SHARDS>` を追加 (inherent method への委譲)。
2. **`src/workload/file.rs`**: `twitter_csv_from_path_string` を追加 (既存 pre-hash 版は parity 用に残置)。
3. **`src/bin/bench.rs`**:
   - `drive_str::<C: CacheImpl<String, u64>>` を追加 (key は `clone()`、value は trace index)。
   - `--source twitter-string` ソースを追加。`run_string_keys` で分岐。
   - 適用 variant は `orig` と `senba` の 2 本に限定 (j7/j8 はまだ String 化対応していない)。

## 検証

### HR 一致 (pre-hash 衝突の有無)

cluster018, len=500000, orig:

| capacity | u64 (pre-hash) HR | String HR | 一致? |
|----------|------------------|-----------|-------|
| 64       | 111509           | 111509    | ✓     |
| 256      | 177155           | 177155    | ✓     |
| 512      | 213708           | 213708    | ✓     |

→ DefaultHasher (SipHash-1-3) で 500K 行をハッシュしても衝突は出なかった。pre-hash 版の HR は実 trace の HR と (この規模では) 同一と確認できた。

### コスト比較 (cluster018, len=500000)

`orig` の u64 pre-hash 版 vs String 版:

| capacity | u64 ns/op | String ns/op | 倍率 |
|----------|-----------|--------------|------|
| 64       | 45.2      | 105.6        | 2.34x |
| 256      | 48.5      | 101.6        | 2.10x |
| 512      | 41.2      | 90.1         | 2.19x |

→ **String 比較・ハッシュコストはおよそ 2x**。orig は線形リスト走査 + key 比較なので妥当な範囲。

`senba::Cache<String, u64>` (Slot32, 8 shards) vs `orig<String, u64>`:

| capacity | orig-str ns/op | senba-str ns/op | 速度比 |
|----------|---------------|-----------------|--------|
| 64       | 105.6         | 60.1            | senba 1.76x |
| 256      | 101.6         | 61.9            | senba 1.64x |
| 512      | 90.1          | 65.6            | senba 1.37x |

→ String キーでも senba::Cache が 1.4-1.8x 高速。SIMD scan + per-shard 分割の効果が key 型に依存せず効いているのを確認。

### 注意点

- `senba::Cache` の per-shard 上限 64 (6-bit ID) があるため、SHARDS=8 の場合 capacity ≤ 512 でしか動かせない。`--capacity 1024` 以上で試したい場合は `--variant senba` 不可、または `Senba<K,V,Slot32,16>` 等の別 SHARDS 数を `bench.rs` に追加する必要がある (今回の scope 外)。
- `senba` と `orig` で HR がわずかに異なる (例: cap=64 で orig 111509, senba 117836)。これは shard 分割により各 shard が小さい SIEVE として独立評価されるため、実 workload に対しては「shard 分割版 SIEVE」の HR 特性となる。これは別現象で、本変更とは独立。

## 結論

`senba::Cache` が任意 K を取れるようになったことで Twitter trace の生 String キーをそのままキャッシュに流せるようになった。pre-hash 版との HR 一致を確認、ns/op コスト ~2x を実測。今後 j7/j8 等にも `CacheImpl<String, u64>` 経路を広げれば SIMD scan + String 比較の組み合わせを直接ベンチできる。
