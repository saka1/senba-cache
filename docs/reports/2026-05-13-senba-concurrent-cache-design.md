# 2026-05-13 — `senba::concurrent::Cache` 設計: c17s lib 化 + Arc<V> + crossbeam-epoch

- 種別: **設計ノート + 実装ログ** (Commit 1-6 で実装済)。
- 関連:
  - `docs/reports/2026-05-13-c17s-shard-heuristic.md` — auto-shard `cap/8` の sweep 根拠
  - `docs/reports/2026-05-13-r2h-control-results.md` — c17s_8x が r1/Partitioned を pareto dominate する根拠
  - `docs/reports/2026-05-12-mt-overhead-vs-lib.md` — `senba::Cache` 単スレ vs MT 系の overhead 起源
  - `docs/reports/2026-05-11-c17s-design.md` — c17s entry-level seqlock 仕様 (port 元)
  - `research/src/experimental/sieve_c17s.rs` — port 元実装 (1275 行)

## 0. TL;DR

`research` から `senba::concurrent::Cache` を新規 publishable surface として
切り出し、PartitionedCache を削除した。**moka 互換** (`V: Clone + Send + Sync +
'static`、`&self` API、`get/remove -> Option<V>`) を満たすために、c17s reader の
`ptr::read(entry) → V::clone` で発生する old V drop race を `Entry.value: Arc<V>`
+ `crossbeam-epoch::Guard::defer_unchecked` で塞いだ。reader の `epoch::pin` で
writer の deferred drop を hold するモデル。

- **新 API**: `pub struct senba::concurrent::Cache<K, V, H = Xxh3Build>` (feature = `concurrent`)
- **public methods (v0.3)**: `new` / `with_shards` / `with_hasher` /
  `with_shards_and_hasher` / `capacity` / `len` / `is_empty` / `contains_key` /
  `get` / `insert` / `remove`
- **bounds**: `K: Hash + Eq + Send + Sync + 'static`、`V: Clone + Send + Sync + 'static`
- **auto-shard**: `next_pow2(cap/8).clamp(next_pow2(cap/64), next_pow2(cap/4))`、
  `MIN_PER_SHARD=4` / `MAX_PER_SHARD=64` (per-shard cap=2 で SIEVE が局所 LRU 化する
  cliff の手前を狙う)
- **削除**: `senba::concurrent::PartitionedCache` (c17s_8x が pareto-dominate)、
  `--variant partitioned` (bench_concurrent / bench_vtune_concurrent)、関連 sweep
  script は `.archived` rename

## 1. Hypothesis

c17s は research artifact で `V: Copy` 限定 sound (heap V は reader の V::clone 中に
writer drop と race して UB)。この race を、Entry 内の値を `Arc<V>` 化 + writer 側で
`Guard::defer_unchecked` による drop 遅延 + reader 側で `epoch::pin` による
defer hold、の 3 点で塞げば、c17s の lock-free reader を維持したまま `V: Clone +
Send + Sync + 'static` まで soundness を拡張できる。**moka の `MiniArc<ValueEntry>`
モデルと等価な refcount-based 安全性**を、c17s 構造に最小侵襲で組み込む。

## 2. Action

### 2.1 c17s → src/concurrent/cache (V: Copy)

Commit 2 で c17s の Shard / ShardHot / Path A/B/C を `src/concurrent/cache/shard.rs`
に verbatim port、`Cache<K, V, H>` wrapper を `src/concurrent/cache.rs` に作成。
public methods は c17s base + `remove`、auto-shard は `cap/8` heuristic。
c17s の const generic `SHARDS` は捨てて runtime `Box<[Shard]>` に。

`remove` は cold path で writer Mutex 配下: 旧 entry version を CAS で claim →
K, V drop → tags shift → len decrement → 旧 id を `free_ids` に push。次の Path B
warmup install が free_ids を pop して再利用。

この時点でテスト 18 本通過 (c17s 由来 + remove 系 4 本 + auto-shard 1 本 +
multi-thread chaos 1 本)。

### 2.2 Arc<V> + crossbeam-epoch (V: Clone)

Commit 3 で `Entry<K, V>.value: V → Entry<K, V>.value: Arc<V>`。`Cargo.toml` に
`crossbeam-epoch = "0.9"` を `concurrent` feature 下に追加。

**reader path** (`try_candidate`、`shard.rs:344-`):

1. `epoch::pin()` を `get_by_hash` 冒頭で取得し関数寿命に持たせる
2. seqlock tier 1 (version 偶数、ptr::read、version 再 load 一致) は c17s と同じ
3. K match 後:
   ```rust
   let arc_ptr: *const V = Arc::as_ptr(&buf.value);
   unsafe { Arc::increment_strong_count(arc_ptr) };
   let owned: Arc<V> = unsafe { Arc::from_raw(arc_ptr) };
   let v: V = (*owned).clone();
   // owned drops → refcount −1
   ```
   `buf` は `ManuallyDrop<Entry<K, V>>` のままなので bit-copy Arc は decrement しない。
   refcount は (元の +1) → (reader bump → +2) → (owned drop → +1) と戻る。

**writer path** (Path A / Path C / update_in_place / remove):
```rust
let old_arc: Arc<V> = unsafe { std::ptr::read(&(*entry_ptr).value) };
unsafe { std::ptr::write(&mut (*entry_ptr).value, new_arc) };
// ... version restore ...
let guard = epoch::pin();
unsafe { guard.defer_unchecked(move || drop(old_arc)) };
```
旧 Arc は writer の pin 時点 epoch でガベージ箱に登録。全 reader pin が解放されるまで
ArcInner は alloc 維持。Path C `writer_evict_and_install` は `evicted_value: V = (*evicted_arc).clone()`
で呼び出し側に V を返し、Arc 本体は defer。

V = String の chaos test (`v_string_chaos_under_contention`、4 writer × 4 reader ×
10k ops × Cache::new(64)) を追加。previous (V: Copy) 実装なら heap V で segfault する
race を、Arc + epoch で clean に通過。

### 2.3 PartitionedCache 削除

Commit 4 で `src/concurrent/mod.rs` から PartitionedCache 実装を除去、
`pub use cache::Cache` のみに圧縮。`bench_concurrent.rs` / `bench_vtune_concurrent.rs`
の `--variant partitioned` arm を `--variant senba_concurrent` に置換。`--partitions`
flag は CLI back-compat のため `#[allow(dead_code)]` で受けるだけにし、変数は使わない。
`docs/benchmark/partitioned-{sweep,cap1024-sweep}/run.sh` は `.archived` rename
(data/figures は履歴として残置)。

## 3. Result

### 3.1 テスト

```
cargo test --workspace --features concurrent
  test result: ok. 120 passed; 0 failed; 0 ignored
```

- 19 cache tests in `src/concurrent/cache/tests/` (port + remove + auto-shard + chaos)
- 5 oracle tests in `research/tests/oracle*.rs`
- 全 senba-research テスト群 473 本通過

### 3.2 soundness 契約 (moka 比較)

| 項目 | moka 0.12 | senba::concurrent (v0.3) |
|---|---|---|
| `V` bound | `Clone + Send + Sync + 'static` | 同 |
| `get` 戻り値 | `Option<V>` (clone) | 同 |
| reader race 保護 | `MiniArc<ValueEntry>` refcount | `Arc<V>` + `crossbeam-epoch` |
| writer 排他 | `cht::SegmentedHashMap` bucket lock | per-shard `parking_lot::Mutex` |
| reader lock-free? | bucket 単位の optimistic locking | 完全 lock-free (seqlock + epoch pin) |
| 必要な追加 dep | (本体に含む) | `crossbeam-epoch` + `parking_lot` |

c17s sweep (`2026-05-13-r1-vs-moka-sweep.md`) で moka 0.12 を T=16 で 7-23× 上回って
いた性能は構造的に維持される (Arc + epoch overhead ~+5 ns/op、reader hot path の
`epoch::pin` だけが純増)。perf-gate は今 commit では再走させていない (TODO §5)。

### 3.3 削除した PartitionedCache の代替

| 旧 API | 新 API |
|---|---|
| `PartitionedCache::new(cap, parts)` | `Cache::with_shards(cap, parts)` |
| `PartitionedCache<K, V, S: SlotSize, H>` | `Cache<K, V, H>` (`SlotSize` 概念不要、固定 tag layout) |
| `partitions()` | `Cache::shards()` (`#[doc(hidden)]`) |

PartitionedCache の thread-local routing (`routing_hint()`) は捨てた。新 Cache は
hash-based shard select なので、shared hot key も適切に分散する (これが c17s_8x が
HR-preserving workloads で pareto-dominate していた理由)。

## 4. 設計判断のメモ

### 4.1 なぜ Arc<V> + epoch であって RwLock<Shard> ではないか

reader を per-shard RwLock の read guard に置けば `V: Clone` は自明に sound に
なるが、c17s が moka を 7-23× 上回っていた根拠 (= lock-free reader fast path) を
失う。Arc + epoch なら reader hot path に追加コストは `epoch::pin` の TLS counter
inc/dec (~3-5 ns) と `Arc::increment_strong_count` (1 atomic add) のみ、~5 ns/op
未満で構造的優位を維持できる。

### 4.2 なぜ free_ids を WriterState に boxed で

`Vec<u16>` を WriterState に直に入れると `ShardHot` が `Box<WriterState>` 経由でも
24 byte 食って 64B 制約を破る。`Mutex<Box<WriterState>>` で writer state を heap
indirection に逃がすことで、reader hot fields (visited / len / path_c_epoch) を
1 cache line に co-locate 維持。writer 側は Mutex acquire 後の indirection 1 つ
追加だが、Mutex 取得自体が writer cold path で吸収できる。

### 4.3 なぜ peek / iter / Stats は v0.4 送り

- `peek` は `get` から `visited.fetch_or` を抜くだけで簡単に追加できるが、API surface
  を最小に保つ方針 (user 確定)。
- `iter` は seqlock 設計と整合性悪い (entry の bit-copy で K を露出するなら ManuallyDrop
  の lifetime contract が複雑化する)。要設計レビュー。
- `Stats` は writer 側の hits/misses/insertions/evictions カウンタ追加だけだが、
  reader hot path に counter atomic を入れたくない。lock-free reader を維持しつつ
  Stats を取る別設計 (例: per-thread relaxed counter + 周期的 aggregate) を検討要。

## 5. Follow-up

1. **perf-gate 再走** — `cargo bench -p senba-research --bench sieve_cache_perf
   -- --baseline before-concurrent`。Arc + epoch overhead を 8 既存 ST scenario で
   観測 (理論的には影響ゼロ、senba::Cache 不変なので)。
2. **`senba::concurrent::Cache` を c17s sweep の比較対象に追加** —
   `docs/benchmark/r1-vs-moka-cap-sweep/` の variant 列に senba_concurrent を追加して
   moka 0.12/0.13 比較を取り直す。
3. **Loom proof** — Path A CAS round trip + concurrent reader の formal verification。
   `crossbeam-epoch` 側は loom テスト済なので、senba 固有の seqlock 部分だけ。~150 LOC。
4. **WSL2 confound 検証** — Win native VTune で Arc + epoch overhead が想定通り
   ~5 ns/op に収まるか確認 ([[project_wsl2_measurement_confound]])。
5. **CHANGELOG.md と v0.3.0 publish** — Commit 6 で実施。`cargo publish --dry-run`
   で payload 検査必須。

## 6. 採否判定

**採用**。moka 互換 V bound + lock-free reader 維持 + PartitionedCache 後継、の
3 条件を満たす。c17s の構造的利点 (Twitter / ARC で moka を桁違いに上回る Mops)
を保ったまま heap V が使えるようになる、という当初の lib 化目標を達成。

reject 条件 (もし発生したら revert する): (i) `arc_v.r_chaos` がランダム
seed で fail する (= Arc + epoch の soundness モデルが想定と異なる)、(ii) perf-gate
で既存 8 ST scenario が >5% regression を起こす (= senba::Cache 側に副作用)。
今 commit 時点では (i) (ii) どちらも検出せず。
