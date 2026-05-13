# 2026-05-14 — `sieve_r4` 実装計画 (Arc-less concurrent cache)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `docs/reports/2026-05-14-arc-less-concurrent-design.md` の設計 (設計上の仮称 `c19s`) を `sieve_r4` として実装し、c17s の reader hot path を壊さずに `V: !Copy` でも sound な並行 SIEVE variant を作る。Phase 3 (full sweep + 比較レポート) までで完結。

**Architecture:** c17s から **(a) Entry の K, V を `ManuallyDrop` で包み**、**(b) `crossbeam-epoch::pin` を reader hot path 先頭に置き**、**(c) writer Path A/B remove/Path C の `drop(old_kv)` を `defer_unchecked` で reclaim** する。`mem::needs_drop::<V>()` の monomorphize-time fold で V: Copy では epoch path が消え c17s 等価、V: !Copy では epoch::pin (~5ns) を払う。Race α (half-overwrite drop) は c17s の entry-version seqlock を継承、Race β (clone-mid-flight UAF) を新たに crossbeam-epoch で閉じる。

**Tech Stack:** Rust 1.89 (edition 2024), `crossbeam-epoch 0.9`, `parking_lot 0.12` (Path B/C Mutex), AVX2 + BMI1 intrinsics (`#[target_feature]`)。x86_64 + non-miri 限定 (research artifact)。

**設計参照:**
- `docs/reports/2026-05-14-arc-less-concurrent-design.md` — 一次仕様 (本計画の根拠)
- `research/src/experimental/sieve_c17s.rs` — 派生元 skeleton
- `docs/reports/2026-05-13-senba-concurrent-vs-c17s.md` — 退行の定量化 (median −34% / worst −63%)
- `docs/reports/2026-05-14-r3-vs-c17s.md` — RwLock 路線 reject の根拠
- `src/concurrent/cache/shard.rs` — lib `senba::concurrent::Cache` の現実装 (epoch 使い方の参照)

**命名 (OQ5 への回答):** 設計仮称 `c19s` を **`sieve_r4`** として実装する (r3 = RwLock baseline からの reader-optimization 系列継続)。`s` suffix は付けない (r-series 命名規約: `[[memory:variant series 命名の s suffix]]`)。

---

## Phase 0: 前提と scope check

- 本 plan は **Phase 1 (実装) → Phase 2 (sanitizer) → Phase 3 (full sweep + 比較レポート)** で完結。Phase 4 (lib `senba::concurrent::Cache` への置換) は **本 plan の対象外**、Phase 3 の数値が accept 基準を満たしてから別計画で扱う。
- 公開 API は research 用 trait `ConcurrentCacheImpl` 互換 (`with_capacity`, `get(&K)->Option<V>`, `insert(K,V)->Option<(K,V)>`)。設計 §G1 の lib API 維持は Phase 4 の責務。
- **API 上の差分**: Path C は `defer_drop_kv_if_needed` で K, V を closure に move するため、r4 の `insert` は **`Path C 経由で None を返す`** (旧 K, V は呼び出し元に渡らない)。oracle test は cache contents を比較するだけで evicted 系列は参照しないため影響なし。bench_concurrent も `let _ = ...insert(...)` で discard しているため影響なし。Phase 4 で API contract を再検討。

---

## Phase 1: 実装 (Tasks 1–9)

### Task 1: `sieve_r4` を c17s から bootstrap

**Files:**
- Create: `research/src/experimental/sieve_r4.rs`
- Modify: `research/src/experimental/mod.rs`

- [ ] **Step 1.1: c17s.rs を r4.rs として複製**

```bash
cp research/src/experimental/sieve_c17s.rs research/src/experimental/sieve_r4.rs
```

- [ ] **Step 1.2: module doc / 型名差し替え**

`research/src/experimental/sieve_r4.rs` の冒頭 doc コメントを以下に置換 (c17s の解説を流用しつつ r4 固有の追記を入れる):

```rust
#![cfg(all(target_arch = "x86_64", not(miri)))]
//! `sieve_r4`: c17s の entry-version seqlock を **race α (half-overwrite drop) 防御** に
//! 残したまま、**race β (clone-mid-flight UAF) 防御** として `crossbeam-epoch` を refcount
//! 抜きで合成した variant。
//!
//! 設計の一次資料: `docs/reports/2026-05-14-arc-less-concurrent-design.md`。
//! r3 (RwLock baseline) 系列の続編で、Arc<V> を排して reader hot path の atomic write を
//! ゼロに戻す (= c17s の atomic shape を回復する) のが目的。
//!
//! # c17s からの構造差分
//!
//! 1. `Entry::key` / `Entry::value` を `ManuallyDrop<K>` / `ManuallyDrop<V>` に変更
//!    (writer Path B remove / Path C で `ManuallyDrop::take` で抜き取って defer closure に
//!    move する設計のため)。`Drop for Shard` は `ManuallyDrop::drop` で明示破棄。
//! 2. reader `get_by_hash` 入口で `epoch::pin` を取得 (`pin_for::<V>`)。`mem::needs_drop::<V>`
//!    が false の場合は monomorphize-time に dead code 除去され、c17s と bit-identical の
//!    asm になる (期待)。
//! 3. reader `try_candidate` の v1/v2 load 間に `compiler_fence(Acquire)` を明示追加
//!    (LLVM IR 段の reorder 防止、x86 codegen 上は no-op)。
//! 4. writer Path A の `drop(old_value)` を `defer_drop_if_needed` に置換。
//! 5. writer `writer_update_in_place` の `drop(old_value)` を同様に置換。
//! 6. writer `writer_evict_and_install` の戻り値を **常に None** とし、旧 K, V を
//!    `defer_drop_kv_if_needed` で reclaim (race β + race γ 防御)。
//!
//! # K, V trait bounds (c17s からの追加要請)
//!
//! - `K: Hash + Eq + Send + 'static` (defer closure capture のため Send + 'static)
//! - `V: Clone + Send + 'static` (同上、reader が `&V` を読むので Sync は不要)
//!
//! # 実装 scope
//!
//! 本ファイルは **x86_64 + AVX2 + non-miri 専用** (research artifact)。AVX2 は
//! `Shard::new` で runtime detect。scalar fallback は持たない。
```

c17s 由来の以下を全置換 (sed 一発でよい):

```bash
# 識別子の入れ替え
sed -i 's/sieve_c17s/sieve_r4/g; s/c17s/r4/g' research/src/experimental/sieve_r4.rs
```

> 注: この sed は doc コメントと `pub(crate)` test names も全部置換するが、ConcurrentCacheImpl impl の trait path だけは触れないことを目視確認 (`crate::experimental::ConcurrentCacheImpl` のまま)。

- [ ] **Step 1.3: mod.rs に登録**

`research/src/experimental/mod.rs` の `pub mod sieve_r3;` 直後に挿入:

```rust
pub mod sieve_r3;
pub mod sieve_r4;
pub mod sieve_v0;
```

- [ ] **Step 1.4: smoke build**

```bash
cargo check -p senba-research
```

期待: clean build。`sieve_r4` が `sieve_c17s` と bit-identical なので失敗するなら sed の取りこぼし (例: pub mod 名 / pub struct 名残り) を疑う。

- [ ] **Step 1.5: コミット**

```bash
git add research/src/experimental/sieve_r4.rs research/src/experimental/mod.rs
git commit -m "$(cat <<'EOF'
chore(sieve_r4): bootstrap from c17s skeleton (no behavior change)

派生元 sieve_c17s と挙動 bit-identical の出発点。後続 task で
crossbeam-epoch + ManuallyDrop wrap を被せる。

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: bench_concurrent / oracle に r4 を wire

**Files:**
- Modify: `research/src/bin/bench_concurrent.rs`
- Modify: `research/tests/oracle.rs`

- [ ] **Step 2.1: bench_concurrent に use 文 + ConcCache impl 追加**

`research/src/bin/bench_concurrent.rs:62` の `use sieve_r3::ConcurrentSieveR3;` 直後に追加:

```rust
use senba_research::experimental::sieve_r4::ConcurrentSieveCache as ConcurrentSieveR4;
```

`research/src/bin/bench_concurrent.rs:190` の `impl<V, const S: usize> ConcCache<V> for ConcurrentSieveR3<...>` の直後に追加:

```rust
impl<V, const S: usize> ConcCache<V> for ConcurrentSieveR4<u64, V, S>
where
    V: Clone + Send + Sync + 'static,
{
    fn build(capacity: usize, _shards: usize) -> Arc<Self> {
        Arc::new(ConcurrentSieveR4::new(capacity))
    }
    #[inline]
    fn get_hit(&self, key: &u64) -> bool {
        ConcurrentSieveR4::get(self, key).is_some()
    }
    #[inline]
    fn insert(&self, key: u64, value: V) {
        let _ = ConcurrentSieveR4::insert(self, key, value);
    }
}
```

- [ ] **Step 2.2: variant accept list / dispatch / shards_col に r4 を追加**

`research/src/bin/bench_concurrent.rs:606-622` (`assert!(matches!(v.as_str(), ...))`) の `"r3"` のあとに `| "r4"` を追加し、`expected ...|r3|r4|...` のメッセージも更新する。

`research/src/bin/bench_concurrent.rs:925-944` (`emit` 関数の shards_col 判定) の `"r3"` 列にも `"r4"` を追加。

`research/src/bin/bench_concurrent.rs` の variant dispatch 箇所 (`("r3", v) => run_r3(...)`) 直後に追加:

```rust
("r4", v) => run_r4(&args, v, trace.clone()),
```

`run_r3` を template として `run_r4` を新設 (1117 行付近の直下に追加):

```rust
/// r4: c17s + crossbeam-epoch (V: !Copy soundness)。shard-count axis は c17s と同型。
fn run_r4(args: &Args, v: ValueKind, trace: Option<Arc<Vec<u64>>>) -> TrialResult {
    macro_rules! arm_r4 {
        ($s:expr) => {
            match v {
                ValueKind::U64 => run_trial::<u64, ConcurrentSieveR4<u64, u64, $s>>(args, trace),
                ValueKind::String => {
                    run_trial::<String, ConcurrentSieveR4<u64, String, $s>>(args, trace)
                }
            }
        };
    }
    match args.shards {
        4 => arm_r4!(4),
        8 => arm_r4!(8),
        16 => arm_r4!(16),
        32 => arm_r4!(32),
        64 => arm_r4!(64),
        128 => arm_r4!(128),
        256 => arm_r4!(256),
        512 => arm_r4!(512),
        1024 => arm_r4!(1024),
        2048 => arm_r4!(2048),
        4096 => arm_r4!(4096),
        8192 => arm_r4!(8192),
        16384 => arm_r4!(16384),
        32768 => arm_r4!(32768),
        65536 => arm_r4!(65536),
        131072 => arm_r4!(131072),
        n => panic!("r4 shards={n} not in supported set (4,8,...,131072)"),
    }
}
```

- [ ] **Step 2.3: oracle test 追加**

`research/tests/oracle.rs:472` (c17s test の直後) に追加:

```rust
/// **r4 (1-shard) は SIEVE 等価**: c17s skeleton + crossbeam-epoch (race β 防御) variant。
/// state machine は c17s と同型なので `sieve_orig` と eviction stream / cache contents が
/// 完全一致する。本テストは V=u64 (Copy) で走らせるため epoch path は monomorphize-time に
/// dead code 除去される (= c17s 相当の codegen を確認するついで)。設計 §6 で V=!Copy も
/// 同じ等価性を持つことが証明されている (V=String は stress test 側で実機検証)。
///
/// 設計詳細は `docs/reports/2026-05-14-arc-less-concurrent-design.md` 参照。
#[test]
fn r4_1shard_matches_orig_on_synthetic_zipf() {
    use senba_research::experimental::sieve_r4::ConcurrentSieveCache as R4;
    let mut total_diff = 0usize;
    let mut total_ops = 0usize;
    for &(skew, cap) in &[(1.05_f64, 16usize), (1.1, 32), (1.2, 64), (1.5, 64)] {
        let trace: Vec<u64> = ZipfGen::new(skew, 10_000, 42).take(200_000).collect();
        let mut a: sieve_orig::SieveCache<u64, u64> = sieve_orig::SieveCache::new(cap);
        let b: R4<u64, u64, 1> = R4::new(cap);
        for k in &trace {
            a.insert(*k, *k);
            b.insert(*k, *k);
        }
        let mut diff = 0usize;
        for &k in &trace {
            if a.get(&k).copied() != b.get(&k) {
                diff += 1;
            }
        }
        eprintln!(
            "[r4 vs orig] skew={skew} cap={cap}: diff={diff}/{} ({:.4}%)",
            trace.len(),
            100.0 * diff as f64 / trace.len() as f64
        );
        total_diff += diff;
        total_ops += trace.len();
    }
    assert_eq!(
        total_diff, 0,
        "r4 が sieve_orig と divergent ({total_ops} ops 中 {total_diff} diff): \
         epoch defer 導入が SIEVE 不変条件を破っている (= 設計通りでない)"
    );
}
```

- [ ] **Step 2.4: 全 test pass を確認 (Task 1 直後の no-op state での baseline)**

```bash
cargo test --workspace
cargo test -p senba-research --features external-traces oracle
```

期待: r4 の oracle test が pass (この時点では c17s と bit-identical なので必ず通る)。**ここで pass しないなら sed/import 取りこぼしがある**。

- [ ] **Step 2.5: コミット**

```bash
git add research/src/bin/bench_concurrent.rs research/tests/oracle.rs
git commit -m "$(cat <<'EOF'
chore(sieve_r4): wire into bench_concurrent and oracle test

variant の dispatch を r3 と同型 (shard sweep, value=u64/string) で追加。
oracle は c17s と bit-identical な現状でも pass する baseline 用。

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: `crossbeam-epoch` dep 追加 + Entry ManuallyDrop wrap

**Files:**
- Modify: `research/Cargo.toml`
- Modify: `research/src/experimental/sieve_r4.rs`

- [ ] **Step 3.1: `crossbeam-epoch` を research の dep に追加**

`research/Cargo.toml` の `parking_lot = "0.12"` 行の直後に追加:

```toml
parking_lot = "0.12"
# r4 が writer の old K/V drop を defer する。lib (senba::concurrent) と同じ 0.9 系。
crossbeam-epoch = "0.9"
```

- [ ] **Step 3.2: Entry struct の K, V を ManuallyDrop で包む**

`research/src/experimental/sieve_r4.rs:127-136` (`struct Entry<K, V> { ... }`) を以下に置換:

```rust
/// `repr(C, align(32))` で sizeof = 32 (power of 2、ID_SHIFT = 5)。version は offset 0。
/// reader は `entries[id].version` を tier 1 seqlock として load、Path A はここを CAS。
///
/// **r4 固有**: `key`/`value` を `ManuallyDrop` で包む。writer Path B remove / Path C で
/// `ManuallyDrop::take` で K, V を抜き取り、`defer_unchecked(move || drop(...))` の closure に
/// move するため、Entry 側に「自身の Drop で K, V を破棄しない」属性を型レベルで付ける。
/// `Drop for Shard` で live entry の K, V を `ManuallyDrop::drop` で明示破棄する。
#[repr(C, align(32))]
struct Entry<K, V> {
    /// 偶数 = stable、奇数 = in-flight。Path A / Path C entries 上書きは
    /// CAS even→odd → 値書き換え → store even+2 で囲う。
    version: AtomicU32,
    key: ManuallyDrop<K>,
    value: ManuallyDrop<V>,
}
```

- [ ] **Step 3.3: `try_candidate` を ManuallyDrop deref に対応**

`research/src/experimental/sieve_r4.rs:344-378` の `try_candidate` 内部、`buf.key == *key` および `let v = buf.value.clone();` を以下に書き換え:

```rust
        // SAFETY: ManuallyDrop で local の Drop を抑制。entries[id] が引き続き K, V の
        // 真の所有者であり、local は bitwise copy なので drop すると double-free。
        let buf: ManuallyDrop<Entry<K, V>> =
            unsafe { ManuallyDrop::new(std::ptr::read(entry_ptr)) };
        let v2 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
        if v1 != v2 {
            return Probe::Racing;
        }
        // Validated: buf is a consistent snapshot. Safe to call K::eq + V::clone.
        // r4: buf.key / buf.value は ManuallyDrop<K/V> なので一段 deref。
        if *buf.key == *key {
            let v = (*buf.value).clone();
            // visited bit conditional set (c11s 由来、hot key の MESI ping-pong 回避)。
            let mask = Self::vbit_mask(pos);
            if self.hot.visited.load(Ordering::Relaxed) & mask == 0 {
                self.hot.visited.fetch_or(mask, Ordering::Relaxed);
            }
            return Probe::Found(v);
        }
        Probe::Miss
```

- [ ] **Step 3.4: `try_path_a_candidate` を ManuallyDrop deref に対応**

`research/src/experimental/sieve_r4.rs:531-557` の `try_path_a_candidate` 内 `if buf.key == *key` を `if *buf.key == *key` に書き換え。

- [ ] **Step 3.5: `writer_find` (Mutex 配下) を ManuallyDrop deref に対応**

`research/src/experimental/sieve_r4.rs:582-629` の `writer_find` 内 `if buf.key == *key` を `if *buf.key == *key` に書き換え。

- [ ] **Step 3.6: Path A `try_path_a` の value field アクセスを ManuallyDrop 越しに対応**

`research/src/experimental/sieve_r4.rs:467-481` 付近の以下 2 行を書き換え:

```rust
            // SAFETY: version 奇数で reader は bail out、別 writer も CAS で弾かれる。
            //         value field のみ in-place write、key は不変。
            //         r4: ManuallyDrop<V> 越しに raw V を read/write する (sizeof 同じ、layout 同じ)。
            let value_ptr = unsafe { &mut (*entry_ptr).value as *mut ManuallyDrop<V> as *mut V };
            let old_value: V = unsafe { std::ptr::read(value_ptr) };
            unsafe {
                std::ptr::write(value_ptr, new_value);
            }
```

- [ ] **Step 3.7: `writer_update_in_place` の value field アクセスを ManuallyDrop 越しに対応**

`research/src/experimental/sieve_r4.rs:649-660` 付近を以下に書き換え:

```rust
        // SAFETY: version 奇数で reader は bail、別 writer も Path A は CAS で弾かれる。
        //         r4: ManuallyDrop<V> 越しに raw V を read/write する。
        unsafe {
            let value_ptr = &mut (*entry_ptr).value as *mut ManuallyDrop<V> as *mut V;
            let old_value: V = std::ptr::read(value_ptr);
            std::ptr::write(value_ptr, value);
            drop(old_value);
        }
```

- [ ] **Step 3.8: `writer_warmup_install` で K, V を ManuallyDrop で包む**

`research/src/experimental/sieve_r4.rs:663-686` 付近、`Entry { version, key, value }` literal 構築箇所を:

```rust
        // SAFETY: writer Mutex 排他下、entries[len] は uninit slot。
        unsafe {
            let slot_ptr = (*entries_mut).as_mut_ptr().add(len) as *mut Entry<K, V>;
            std::ptr::write(
                slot_ptr,
                Entry {
                    version: AtomicU32::new(0),
                    key: ManuallyDrop::new(key),
                    value: ManuallyDrop::new(value),
                },
            );
        }
```

- [ ] **Step 3.9: `writer_evict_and_install` の K, V 抜き取り/上書きを ManuallyDrop 越しに対応**

`research/src/experimental/sieve_r4.rs:727-759` 付近の `evicted_key` / `evicted_value` 取得と新 K, V install を以下に書き換え:

```rust
        // 旧 entry の (key, value) を取り出し
        // SAFETY: version 奇数で排他、Path A は CAS で弾かれる。
        //         r4: ManuallyDrop<K/V> から ManuallyDrop::take で抜き取る (Box::take と同形)。
        let evicted_key: K = unsafe { ManuallyDrop::take(&mut (*evict_entry_ptr).key) };
        let evicted_value: V = unsafe { ManuallyDrop::take(&mut (*evict_entry_ptr).value) };
```

```rust
        // 新 entry を entries[evict_id] に install (key, value 上書き、version flip)
        // SAFETY: version 奇数で排他確保済み、tag は LIVE が無い (= 上で EMPTY を踏んだ)
        //         ので reader はこの slot に当たらない。
        unsafe {
            (*evict_entry_ptr).key = ManuallyDrop::new(key);
            (*evict_entry_ptr).value = ManuallyDrop::new(value);
        }
```

- [ ] **Step 3.10: `Drop for Shard` を ManuallyDrop::drop 経路に書き換え**

`research/src/experimental/sieve_r4.rs:846-861` を以下に置換:

```rust
impl<K, V> Drop for Shard<K, V> {
    fn drop(&mut self) {
        let len = self.hot.len.load(Ordering::Relaxed);
        let entries_mut = self.entries.get();
        for i in 0..len {
            let t = self.tags[i].load(Ordering::Relaxed);
            if t & LIVE != 0 {
                let id = Self::id_of(t);
                // SAFETY: LIVE ⇒ entries[id] init 済み。&mut self なので reader 不在。
                //         r4: ManuallyDrop<K/V> なので明示 drop が必要。assume_init_mut で
                //         &mut Entry を取ってから ManuallyDrop::drop を 2 回呼ぶ。
                unsafe {
                    let entry: &mut Entry<K, V> = (*entries_mut)[id].assume_init_mut();
                    ManuallyDrop::drop(&mut entry.key);
                    ManuallyDrop::drop(&mut entry.value);
                }
            }
        }
    }
}
```

- [ ] **Step 3.11: ビルド + unit + oracle test pass を確認**

```bash
cargo clippy -p senba-research --all-targets -- -D warnings
cargo test -p senba-research --lib sieve_r4
cargo test -p senba-research --features external-traces r4_1shard
```

期待: 全 pass。**この時点では `defer_drop_*` が無いため race β はまだ未防御** (V=u64 ならそもそも race β 不在なので pass、V=String の concurrent 検証は Phase 2)。

- [ ] **Step 3.12: コミット**

```bash
git add research/Cargo.toml research/src/experimental/sieve_r4.rs Cargo.lock
git commit -m "$(cat <<'EOF'
feat(sieve_r4): wrap Entry K/V in ManuallyDrop, add crossbeam-epoch dep

writer 側で ManuallyDrop::take で K/V を抜き取って defer closure に move
するための型変更。Drop impl は Shard 側で明示破棄。defer 機構自体は次 task。

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Reader R1 epoch::pin 導入 + R8 compiler_fence

**Files:**
- Modify: `research/src/experimental/sieve_r4.rs`

- [ ] **Step 4.1: `crossbeam_epoch` import + `EpochGuardWrapper` 定義**

`research/src/experimental/sieve_r4.rs:89-95` の use 群直下に追加:

```rust
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering, fence, compiler_fence};

use crossbeam_epoch as epoch;

/// `mem::needs_drop::<V>()` の monomorphize-time fold で V: Copy の場合に
/// guard 取得を完全消去するための wrapper。`Some(epoch::Guard)` バリアントは
/// Drop 時に reader の pin を release する。
///
/// 設計 §8.1 (`needs_drop` const branch + `EpochGuardWrapper`) 参照。
#[allow(clippy::large_enum_variant)]
enum EpochGuardWrapper {
    Some(epoch::Guard),
    None,
}

#[inline(always)]
const fn needs_epoch<V>() -> bool {
    std::mem::needs_drop::<V>()
}

#[inline(always)]
fn pin_for<V>() -> EpochGuardWrapper {
    if needs_epoch::<V>() {
        EpochGuardWrapper::Some(epoch::pin())
    } else {
        EpochGuardWrapper::None
    }
}
```

> 注: `compiler_fence` は元々 `std::sync::atomic::compiler_fence`。`fence` と並んで `use` する。

- [ ] **Step 4.2: `get_by_hash` 入口で pin guard を取得**

`research/src/experimental/sieve_r4.rs:396-416` の `get_by_hash` を以下に置換:

```rust
    /// r4: 入口で `pin_for::<V>()` を取得し、scan + clone 中に writer の deferred drop が
    /// 走らないことを保証する。`needs_drop::<V>()` が false (= V: Copy) のとき pin は
    /// monomorphize-time に dead code 除去される (設計 §8.2)。
    pub fn get_by_hash(&self, key: &K, hash: u64) -> Option<V> {
        const MAX_READER_RETRY: usize = 4;
        let _guard = pin_for::<V>();
        let needle = Self::needle_from_hash(hash);
        for attempt in 0..MAX_READER_RETRY {
            let epoch_before = self.hot.path_c_epoch.load(Ordering::Acquire);
            // SAFETY: AVX2 は Shard::new の assert で検証済み。
            let (v, racing) = unsafe { self.find_get(key, needle) };
            if let Some(v) = v {
                return Some(v);
            }
            let epoch_after = self.hot.path_c_epoch.load(Ordering::Acquire);
            if !racing && epoch_before == epoch_after {
                return None;
            }
            if attempt + 1 < MAX_READER_RETRY {
                hint::spin_loop();
            }
        }
        None
        // _guard drops here → reader unpins → writer's deferred drops can be reclaimed.
    }
```

- [ ] **Step 4.3: `try_candidate` の v1/v2 load 間に compiler_fence を挿入**

`research/src/experimental/sieve_r4.rs` の `try_candidate` (Task 3.3 で書き換え済み) で、`std::ptr::read(entry_ptr)` の **直後** かつ `(*entry_ptr).version.load` (v2) の **直前** に挿入:

```rust
        let buf: ManuallyDrop<Entry<K, V>> =
            unsafe { ManuallyDrop::new(std::ptr::read(entry_ptr)) };
        // r4: LLVM IR 段の non-atomic load (ptr::read の中身) を v2 load の後ろに reorder
        //     させない。x86 では codegen 上 no-op (TSO で隠蔽)。設計 §6.3 参照。
        compiler_fence(Ordering::Acquire);
        let v2 = unsafe { (*entry_ptr).version.load(Ordering::Acquire) };
```

`try_path_a_candidate` (Mutex 配下だが reader と並走するので同様) にも同じ `compiler_fence(Ordering::Acquire)` を追加。

- [ ] **Step 4.4: ビルド + test 確認**

```bash
cargo clippy -p senba-research --all-targets -- -D warnings
cargo test -p senba-research --lib sieve_r4
cargo test -p senba-research --features external-traces r4_1shard
```

期待: 全 pass。

- [ ] **Step 4.5: コミット**

```bash
git add research/src/experimental/sieve_r4.rs
git commit -m "$(cat <<'EOF'
feat(sieve_r4): add reader pin_for + compiler_fence (race α/β scaffolding)

reader 入口で epoch::pin を取得 (V: Copy では const-fold で消える)。
v1/v2 load の間に compiler_fence(Acquire) を挟み LLVM IR reorder を禁じる。
writer 側 defer 化は次 task。

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Path A の `drop(old_value)` を defer に置換

**Files:**
- Modify: `research/src/experimental/sieve_r4.rs`

- [ ] **Step 5.1: `defer_drop_if_needed` helper を追加**

`Shard::new` の上 (e.g. 230 行付近、`impl<K, V> Shard<K, V> where K: Hash + Eq, V: Clone {` ブロックの前) に free fn として:

```rust
/// V: !Copy のときは epoch::pin + defer_unchecked で reclaim を遅延、V: Copy のときは
/// `mem::forget` (drop も forget も同じ asm)。`needs_drop::<V>()` の monomorphize-time
/// fold で片方の path が消える。
///
/// SAFETY: `defer_unchecked` は closure が **`Send + 'static`** であること、および呼び出し時点
/// で reader が自分の pin guard を保持している場合に reader の clone-mid-flight を保護する
/// crossbeam-epoch の標準契約に依存する。本関数は writer 経路から呼ばれ、上記契約は
/// `V: Send + 'static` の trait bound (Shard impl 側) で満たされる。
#[inline]
fn defer_drop_if_needed<V: Send + 'static>(v: V) {
    if needs_epoch::<V>() {
        let guard = epoch::pin();
        unsafe {
            guard.defer_unchecked(move || drop(v));
        }
    } else {
        std::mem::forget(v);
    }
}
```

- [ ] **Step 5.2: Path A の `drop(old_value)` を置換**

`research/src/experimental/sieve_r4.rs` の `try_path_a` 内、Task 3.6 で書き換えた箇所の最後 `drop(old_value);` を:

```rust
            defer_drop_if_needed::<V>(old_value);
```

`writer_update_in_place` (Task 3.7) の `drop(old_value);` も同様に:

```rust
            defer_drop_if_needed::<V>(old_value);
```

- [ ] **Step 5.3: trait bound に `V: Send + 'static` を追加**

`Shard<K, V>` の impl block:

```rust
impl<K, V> Shard<K, V>
where
    K: Hash + Eq + Send + 'static,
    V: Clone + Send + 'static,
{
```

`ConcurrentSieveCache<K, V, SHARDS>` の impl block にも同じ bound:

```rust
impl<K, V, const SHARDS: usize> ConcurrentSieveCache<K, V, SHARDS>
where
    K: Hash + Eq + Send + 'static,
    V: Clone + Send + 'static,
{
```

`unsafe impl Send/Sync for Shard<K, V>` の bound も以下に拡張:

```rust
unsafe impl<K: Send + 'static, V: Send + 'static> Send for Shard<K, V> {}
unsafe impl<K: Send + Sync + 'static, V: Send + Sync + 'static> Sync for Shard<K, V> {}
```

- [ ] **Step 5.4: ビルド + test 確認**

```bash
cargo clippy -p senba-research --all-targets -- -D warnings
cargo test -p senba-research --lib sieve_r4
cargo test -p senba-research --features external-traces r4_1shard
```

期待: 全 pass。trait bound 追加で `K`/`V` に `'static` 制約が乗るが、bench / oracle の K=u64,V=u64|String はすべて 'static なので問題なし。

- [ ] **Step 5.5: コミット**

```bash
git add research/src/experimental/sieve_r4.rs
git commit -m "$(cat <<'EOF'
feat(sieve_r4): defer Path A old V drop via crossbeam-epoch

try_path_a / writer_update_in_place の旧 V を sync drop から
defer_drop_if_needed に置換 (race β: clone-mid-flight UAF 防御)。
V: Copy では const-fold で mem::forget に縮退。

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Path C の K, V を defer に置換 (race β + γ 両方)

**Files:**
- Modify: `research/src/experimental/sieve_r4.rs`

- [ ] **Step 6.1: `defer_drop_kv_if_needed` helper を追加**

`defer_drop_if_needed` の直下に追加:

```rust
/// Path C / remove で旧 K, V の双方を defer drop する。両方が `!needs_drop` のときは
/// `mem::forget` の組に縮退、片方でも drop を持つなら 1 回の `epoch::pin` + 1 個の closure に
/// 束ねる。
#[inline]
fn defer_drop_kv_if_needed<K, V>(k: K, v: V)
where
    K: Send + 'static,
    V: Send + 'static,
{
    if std::mem::needs_drop::<K>() || std::mem::needs_drop::<V>() {
        let guard = epoch::pin();
        unsafe {
            guard.defer_unchecked(move || {
                drop(k);
                drop(v);
            });
        }
    } else {
        std::mem::forget(k);
        std::mem::forget(v);
    }
}
```

- [ ] **Step 6.2: `writer_evict_and_install` の戻り値を `()` に変更し、defer 経路を追加**

`research/src/experimental/sieve_r4.rs` の `writer_evict_and_install` 末尾を以下に書き換え:

```rust
        // path_c_epoch bump で reader の coarse seqlock を fire (shift で false-miss した
        // reader に retry を促す)。
        self.hot.path_c_epoch.fetch_add(1, Ordering::Release);

        // r4: 旧 K, V を epoch defer で reclaim (race β + race γ 防御、設計 §6.4)。
        //     Path C 完了後に呼ぶ理由は path_c_epoch publish と evicted の defer の順序を
        //     latch して reader retry がすれ違わないため (defer 自体は global queue に積む
        //     だけで visibility は epoch 進行に従う)。
        defer_drop_kv_if_needed::<K, V>(evicted_key, evicted_value);
    }
```

戻り値型を `(K, V)` から `()` に変更:

```rust
    fn writer_evict_and_install(
        &self,
        state: &mut WriterState,
        key: K,
        value: V,
        needle: u16,
    ) {
```

- [ ] **Step 6.3: caller (`path_bc`) を戻り値変更に合わせる**

`research/src/experimental/sieve_r4.rs` の `path_bc`:

```rust
    fn path_bc(&self, key: K, value: V, needle: u16) -> Option<(K, V)> {
        let mut state = self.hot.writer.lock();

        if let Some((pos, id)) = self.writer_find(&key, needle) {
            self.writer_update_in_place(pos, id, key, value);
            return None;
        }

        let len = self.hot.len.load(Ordering::Relaxed);
        if len < self.capacity {
            self.writer_warmup_install(len, key, value, needle);
            return None;
        }

        // r4: Path C は evicted (K,V) を defer に流し、caller には常に None を返す
        //     (API 上の差分。Phase 4 lib 統合時に再検討)。
        self.writer_evict_and_install(&mut state, key, value, needle);
        None
    }
```

- [ ] **Step 6.4: ビルド + test 確認**

```bash
cargo clippy -p senba-research --all-targets -- -D warnings
cargo test -p senba-research --lib sieve_r4
cargo test -p senba-research --features external-traces r4_1shard
```

`evicts_oldest_when_full_and_unvisited` 等の unit test は `assert_eq!(evicted, Some((1, 10)))` を assert しているはず。**ここで赤になる**ため、r4 のテストでは:
- ①該当 unit test (Task 1 で sed copy した c17s 由来 unit test 群) を **r4 仕様に合わせて削除または書き換え** する。具体的には Path C 経由の test (`evicts_oldest_when_full_and_unvisited` / `visited_entry_survives_first_pass` / `all_visited_clears_bits_then_evicts` / `warm_up_to_steady_transition`) で `assert_eq!(evicted, Some(...))` を `assert!(evicted.is_none())` または `cache.contains_key(&evicted_key) == false` ベースの assert に差し替える。
- ②具体的な書き換え方は task の範囲を超えるので、**unit test は r4 用に「evicted の identity を assert しない」方針** に統一する。

書き換え例 (`evicts_oldest_when_full_and_unvisited`):

```rust
    #[test]
    fn evicts_oldest_when_full_and_unvisited() {
        let cache: ConcurrentSieveCache<i32, i32, 1> = ConcurrentSieveCache::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        // r4: Path C は evicted (K,V) を defer drop に渡すため None を返す。
        let evicted = cache.insert(3, 30);
        assert!(evicted.is_none(), "r4 returns None on Path C eviction");
        assert_eq!(cache.len(), 2);
        assert!(!cache.contains_key(&1));
        assert!(cache.contains_key(&2));
        assert!(cache.contains_key(&3));
    }
```

`visited_entry_survives_first_pass` / `all_visited_clears_bits_then_evicts` / `warm_up_to_steady_transition` も同様に `assert_eq!(evicted, Some((..)))` の identity 部分を `assert!(evicted.is_none())` + `cache.contains_key(...)` で置き換える。

- [ ] **Step 6.5: oracle test を再走**

```bash
cargo test -p senba-research --features external-traces r4_1shard
```

期待: pass (oracle は cache contents 比較のみで evicted identity は見ない)。

- [ ] **Step 6.6: コミット**

```bash
git add research/src/experimental/sieve_r4.rs
git commit -m "$(cat <<'EOF'
feat(sieve_r4): defer Path C old K/V drop, drop evicted-pair return

writer_evict_and_install を `() ` 返しに変更、旧 K, V を defer_drop_kv_if_needed
で reclaim (race β + race γ 防御)。caller (path_bc) は Path C 経由で常に None を
返す。unit test の evicted identity 比較は contains_key ベースに書き換え。
oracle test (cache contents 比較) は pass。

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Copy 特殊化が monomorphize-time で効くことを `cargo asm` で確認

**Files:**
- 新規ファイルなし (検証のみ)

- [ ] **Step 7.1: cargo-show-asm のセットアップ確認**

```bash
cargo install cargo-show-asm --locked
```

(既に入っていれば skip。`cargo asm --version` で確認可)

- [ ] **Step 7.2: V=u64 で `crossbeam_epoch::pin` シンボルが asm に出ないことを確認**

```bash
cargo asm -p senba-research --release --rust \
    'senba_research::experimental::sieve_r4::Shard<u64,u64>::get_by_hash' \
    2>&1 | tee /tmp/r4_get_u64.asm
grep -E 'crossbeam|epoch|pin' /tmp/r4_get_u64.asm || echo "OK: no epoch symbols"
```

期待: `OK: no epoch symbols` (`needs_drop::<u64>()` は false なので const-fold で消える)。**残った場合**は OQ1 (= release-build でも fold が効かない) が真、`#[inline(always)]` の追加 / `pin_for` を `match` でなく `if needs_epoch::<V>()` のフラットな形に直す等の調整が必要 (設計 §8.1)。

- [ ] **Step 7.3: V=String で epoch symbol が **残る** ことを確認 (negative control)**

```bash
cargo asm -p senba-research --release --rust \
    'senba_research::experimental::sieve_r4::Shard<u64,alloc::string::String>::get_by_hash' \
    2>&1 | tee /tmp/r4_get_string.asm
grep -E 'crossbeam|epoch|pin' /tmp/r4_get_string.asm | head -5
```

期待: epoch symbol が出る (= V: !Copy では fold されず実行される)。

- [ ] **Step 7.4: 検証結果を docs に記録**

`docs/reports/2026-05-14-r4-implementation-plan.md` の Task 7 に **後追いで** 検証コマンドの実出力 1-2 行を追記 (= この plan 自身を update)。記録形式:

```markdown
> **Verification (YYYY-MM-DD):**
> - V=u64: `grep epoch /tmp/r4_get_u64.asm` → 0 件 (= const-fold OK)
> - V=String: `grep epoch /tmp/r4_get_string.asm` → N 件
```

> **Verification (2026-05-14):**
> - V=u64 path (`get_hit` 0x6be50-0x6c010, fully inlined): `grep crossbeam_epoch` → **0 件** (const-fold が効いて pin が完全に消えた)
> - V=String path (`Shard::get_by_hash` 0x7f430, sole out-of-line monomorphization): `grep crossbeam_epoch` → **2 件**
>   - `call <crossbeam_epoch::default::with_handle>` (= `epoch::pin` entry at function head)
>   - `call <core::ptr::drop_in_place<crossbeam_epoch::guard::Guard>>` (= guard release on unwind path)
> - 結論: 設計 §8 の monomorphize-time fold 仮説 (OQ1) は **release build で実証**。

- [ ] **Step 7.5: コミット (plan の追記分のみ。コードは変更なし)**

```bash
git add docs/reports/2026-05-14-r4-implementation-plan.md
git commit -m "$(cat <<'EOF'
docs(r4): record cargo-asm verification of Copy specialization const-fold

Task 7 の実出力を plan に追記。V=u64 で crossbeam-epoch シンボルが asm に
残らないこと、V=String では残ることを確認。

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: Smoke perf check (V=u64 / V=String、hot-key、T=16)

**Files:**
- 新規ファイルなし (smoke run + 結果記録のみ)

- [ ] **Step 8.1: smoke benchmark を 1 cell × 1 trial で走らせる (V=u64, T=16, gim, skew=1.4)**

```bash
cargo build --release -p senba-research --bin bench_concurrent --features senba/concurrent

./target/release/bench_concurrent --variant r4 --variant c17s --variant senba_concurrent \
    --shards 512 --cap 4096 --ops 8000000 --warmup 4000000 --trials 1 \
    --threads 16 --skew 1.4 --keys 100000 --op-mix gim --value u64 \
    --ways 1 --partitions 1 --source zipf 2>&1 | tee /tmp/r4_smoke_u64.log
```

期待: r4 の `aggregate_mops` が c17s ±10% 内 (= Copy 特殊化が効いている smoke 基準)。**>10% 落ちる場合**は const-fold が外れている、または compiler_fence / pin_for の inline が浅いのを疑い、Task 7 を再走する。

- [ ] **Step 8.2: V=String で同条件を走らせる**

```bash
./target/release/bench_concurrent --variant r4 --variant c17s --variant senba_concurrent \
    --shards 512 --cap 4096 --ops 8000000 --warmup 4000000 --trials 1 \
    --threads 16 --skew 1.4 --keys 100000 --op-mix gim --value string \
    --ways 1 --partitions 1 --source zipf 2>&1 | tee /tmp/r4_smoke_string.log
```

期待: r4 の `aggregate_mops` > senba_concurrent (= Arc 撤去で Mops 取り戻し)、≤ c17s (epoch::pin overhead 残るので)。**senba_concurrent を下回る場合**は Phase 3 sweep に進まず原因特定 (epoch defer queue blowup の可能性、設計 §10.5)。

- [ ] **Step 8.3: smoke 結果を plan に追記**

`docs/reports/2026-05-14-r4-implementation-plan.md` Task 8 セクションに以下フォーマットで追記:

```markdown
> **Smoke (YYYY-MM-DD, T=16, cap=4096, shards=512, skew=1.4 gim):**
> - V=u64:    c17s = X.X Mops, r4 = Y.Y Mops (Δ = ±Z.Z%), senba = W.W Mops
> - V=String: c17s = X.X Mops, r4 = Y.Y Mops (Δ vs senba = +Z.Z%)
```

- [ ] **Step 8.4: コミット**

```bash
git add docs/reports/2026-05-14-r4-implementation-plan.md
git commit -m "$(cat <<'EOF'
docs(r4): record smoke perf (V=u64 ±10% c17s, V=String beats senba_concurrent)

Phase 1 完了 gate を通過。次は sanitizer (Phase 2) → full sweep (Phase 3)。

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

**Phase 1 完了 gate**:
- ✅ oracle test pass
- ✅ unit test pass
- ✅ V=u64 smoke で c17s ±10% (Copy 特殊化が効いている)
- ✅ V=String smoke で senba_concurrent を上回る

いずれも満たさない場合は Phase 2 に進まず Task 3-7 を見直す。

---

## Phase 2: Sanitizer (Tasks 9–11)

### Task 9: TSan stress (V=String, hot-key, 60s)

**Files:**
- Create: `docs/benchmark/r4-sanitizer/run.sh`

- [ ] **Step 9.1: Rust nightly toolchain 確認**

```bash
rustup toolchain list | grep nightly || rustup install nightly
rustup component add rust-src --toolchain nightly
```

- [ ] **Step 9.2: TSan build (single cell)**

```bash
mkdir -p docs/benchmark/r4-sanitizer
cat > docs/benchmark/r4-sanitizer/run.sh <<'EOF'
#!/usr/bin/env bash
# r4 sanitizer stress: TSan で V=String hot-key を 60s 走らせ data race report を集める。
# 設計 §10.4 の検証戦略 (C) に対応。

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

HERE="docs/benchmark/r4-sanitizer"
LOG="$HERE/tsan.log"

RUSTFLAGS="-Zsanitizer=thread" \
    cargo +nightly build --release -p senba-research --bin bench_concurrent \
    --features senba/concurrent \
    -Zbuild-std --target x86_64-unknown-linux-gnu

./target/x86_64-unknown-linux-gnu/release/bench_concurrent \
    --variant r4 --shards 512 --cap 4096 --ops 60000000 --warmup 4000000 --trials 1 \
    --threads 16 --skew 1.8 --keys 1000 --op-mix read-heavy --value string \
    --ways 1 --partitions 1 --source zipf 2>&1 | tee "$LOG"

if grep -q "WARNING: ThreadSanitizer:" "$LOG"; then
    echo "[FAIL] TSan reported races. See $LOG" >&2
    exit 1
fi
echo "[OK] TSan clean."
EOF
chmod +x docs/benchmark/r4-sanitizer/run.sh
```

- [ ] **Step 9.3: TSan を走らせる**

```bash
./docs/benchmark/r4-sanitizer/run.sh
```

期待: `[OK] TSan clean.`。**race report が出る場合**は report の stack trace を確認し、設計 §6 の証明と突き合わせる (典型的には compiler_fence の位置漏れ、defer 順序ミスを疑う)。修正は別 task として plan に追加。

- [ ] **Step 9.4: コミット**

```bash
git add docs/benchmark/r4-sanitizer/run.sh
git commit -m "$(cat <<'EOF'
test(r4): add TSan stress harness for V=String hot-key

設計 §10.4 検証戦略 (C) に対応。60s で race report が出ないことを CI 外
の ad-hoc gate として保持する。

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 10: Miri test (V=u64 / V=String、basic concurrent suite)

**Files:**
- 新規ファイルなし

- [ ] **Step 10.1: r4 が `#[cfg(not(miri))]` で gate されていることを確認**

`research/src/experimental/sieve_r4.rs:1` の `#![cfg(all(target_arch = "x86_64", not(miri)))]` がそのままなら **Miri test は構造的に不可能** (= 設計 §10.4 (B) は applicable でない)。これは AVX2 intrinsics + UnsafeCell の組み合わせを Miri が解せないため。

設計 §10.4 (B) は楽観的な見積もりだったが、AVX2 gate と両立しない。Miri は **scalar 版を別 cfg で用意した上で** 走らせる必要があるが、scalar 版の実装は本 plan の対象外。

**判断**: Task 10 は **skip** とし、Miri による race β 検証は AVX2-free な reduced model (例: `crossbeam-epoch` の単独 race test を miri で再現) を別計画で実施する旨を Phase 3 report に明記する。

- [ ] **Step 10.2: コミット (skip 判断のみ、コード変更なし)**

skip 判断を Plan に追記:

```markdown
> **Miri verification status (YYYY-MM-DD):** skip — sieve_r4 は AVX2 intrinsics 経由で
> Miri と非互換 (`#![cfg(not(miri))]`)。代替検証は (a) TSan (Task 9)、(b) ASan (Task 11)、
> (c) crossbeam-epoch upstream の miri test に乗る (= compose としての健全性は upstream 保証)。
```

```bash
git add docs/reports/2026-05-14-r4-implementation-plan.md
git commit -m "$(cat <<'EOF'
docs(r4): document Miri skip rationale (AVX2 cfg-gate incompatible)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 11: ASan stress

**Files:**
- Modify: `docs/benchmark/r4-sanitizer/run.sh` (TSan run と並べる)

- [ ] **Step 11.1: ASan build を run.sh に追加**

`docs/benchmark/r4-sanitizer/run.sh` に追記:

```bash
echo "[r4-sanitizer] running ASan stress..."
RUSTFLAGS="-Zsanitizer=address" \
    cargo +nightly build --release -p senba-research --bin bench_concurrent \
    --features senba/concurrent \
    -Zbuild-std --target x86_64-unknown-linux-gnu

ASAN_OPTIONS="abort_on_error=1:halt_on_error=1" \
./target/x86_64-unknown-linux-gnu/release/bench_concurrent \
    --variant r4 --shards 512 --cap 4096 --ops 60000000 --warmup 4000000 --trials 1 \
    --threads 16 --skew 1.8 --keys 1000 --op-mix read-heavy --value string \
    --ways 1 --partitions 1 --source zipf 2>&1 | tee "$HERE/asan.log"

if grep -qE "ERROR: AddressSanitizer:|heap-use-after-free|SEGV" "$HERE/asan.log"; then
    echo "[FAIL] ASan reported errors. See $HERE/asan.log" >&2
    exit 1
fi
echo "[OK] ASan clean."
```

- [ ] **Step 11.2: 走らせる**

```bash
./docs/benchmark/r4-sanitizer/run.sh
```

期待: `[OK] ASan clean.`。**heap-use-after-free が出る場合**は race β/γ 防御が破れているサイン。stack trace から writer (Path A/C) の defer 漏れか reader pin の取り忘れを特定する。

- [ ] **Step 11.3: コミット**

```bash
git add docs/benchmark/r4-sanitizer/run.sh
git commit -m "$(cat <<'EOF'
test(r4): add ASan stress to r4-sanitizer harness

heap-use-after-free を実機検出して race β 防御の健全性を確認する。

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

**Phase 2 完了 gate**:
- ✅ TSan で race report ゼロ
- ✅ ASan で heap-use-after-free ゼロ
- (Miri skip は Task 10 で文書化済み)

---

## Phase 3: Full perf sweep + 比較レポート (Tasks 12–15)

### Task 12: `docs/benchmark/r4-vs-c17s/` infrastructure を作る

**Files:**
- Create: `docs/benchmark/r4-vs-c17s/run.sh`
- Create: `docs/benchmark/r4-vs-c17s/plot.py`

- [ ] **Step 12.1: run.sh を `senba-concurrent-vs-c17s/run.sh` から派生**

```bash
mkdir -p docs/benchmark/r4-vs-c17s/{data,figures}
cp docs/benchmark/senba-concurrent-vs-c17s/run.sh docs/benchmark/r4-vs-c17s/run.sh
```

`docs/benchmark/r4-vs-c17s/run.sh` 冒頭の説明を以下に書き換え:

```bash
#!/usr/bin/env bash
# sieve_r4 (c17s + crossbeam-epoch、Arc-less) vs sieve_c17s (research perf champion)
# vs senba::concurrent::Cache (lib 0.3.0、Arc+epoch).
#
# 目的: r4 が Arc<V> を撤去した結果、c17s の reader hot path bit-shape を回復しつつ
#       senba::concurrent の退行 (median -34% / worst -63%) を解消できるかを定量化。
#       設計 §9.4 / §G4 の accept 基準:
#         - V=u64:    median ≥ -5% vs c17s, worst ≥ -10% vs c17s
#         - V=String: median ≥ +30% vs senba_concurrent, worst ≥ +20% vs senba_concurrent
#
# 軸: 3 variant × 4 threads × 3 skew × 2 mix × 2 value = 144 cell × 3 trial = 432 row.
```

下部の variant ループを以下に書き換え:

```bash
VALUE_LIST="${VALUE_LIST:-u64 string}"

for value in $VALUE_LIST; do
  for variant in r4 c17s senba_concurrent; do
    for threads in $T_LIST; do
      for skew in $SKEW_LIST; do
        for op_mix in $MIX_LIST; do
          run_one "$variant" "$threads" "$skew" "$op_mix" "$value"
        done
      done
    done
  done
done
```

`run_one` の引数を `($1=variant $2=threads $3=skew $4=op_mix $5=value)` に拡張、`--value u64` をハードコードしている部分を `--value "$value"` に置き換える。

- [ ] **Step 12.2: plot.py を `senba-concurrent-vs-c17s/plot.py` から派生**

```bash
cp docs/benchmark/senba-concurrent-vs-c17s/plot.py docs/benchmark/r4-vs-c17s/plot.py
```

`docs/benchmark/r4-vs-c17s/plot.py` の variant 名と図のタイトルを r4 / c17s / senba_concurrent の 3-way に更新。具体的には:
- variant 列: `["r4", "c17s", "senba_concurrent"]`
- value axis (`u64` / `string`) で図を分割
- baseline = c17s として r4 / senba_concurrent の Δ% を併記

(既存 plot.py のレイアウトに合わせて細部は editor で調整。詳細は docs/benchmark/senba-concurrent-vs-c17s/plot.py の構造に従う)

- [ ] **Step 12.3: smoke で 1 trial だけ走らせて CSV header / variant col が出ることを確認**

```bash
TRIALS=1 T_LIST=16 SKEW_LIST=1.4 MIX_LIST=gim VALUE_LIST=u64 \
    ./docs/benchmark/r4-vs-c17s/run.sh
head -3 docs/benchmark/r4-vs-c17s/data/results.csv
```

期待: header + r4/c17s/senba_concurrent 各 1 row (合計 3 + 1 = 4 行)。

- [ ] **Step 12.4: コミット**

```bash
git add docs/benchmark/r4-vs-c17s/run.sh docs/benchmark/r4-vs-c17s/plot.py
git commit -m "$(cat <<'EOF'
bench(r4-vs-c17s): add 3-way sweep harness (r4 / c17s / senba_concurrent, V=u64+string)

senba-concurrent-vs-c17s sweep を 3-way × 2-value (u64/string) に拡張。
Phase 3 accept 基準は設計 §9.4 / §G4 を踏襲。

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 13: Full sweep を走らせる (432 row)

**Files:**
- Generate: `docs/benchmark/r4-vs-c17s/data/results.csv`

- [ ] **Step 13.1: full sweep を実行 (1-2h 想定)**

```bash
./docs/benchmark/r4-vs-c17s/run.sh 2>&1 | tee docs/benchmark/r4-vs-c17s/data/run.log
```

途中で `[FAILED]` が `crashes.log` に出ていないことを time-to-time 確認。OOM や TSan 由来の hang はここで起きうる (= 設計 §10.5 reclamation backlog 暴走を想定)。**hang する cell** が見つかったら、その cell の workload で reclamation backlog の RSS を別途計測して Open Question を更新。

- [ ] **Step 13.2: 結果の sanity check**

```bash
wc -l docs/benchmark/r4-vs-c17s/data/results.csv
awk -F, 'NR>1 {print $1}' docs/benchmark/r4-vs-c17s/data/results.csv | sort | uniq -c
```

期待: `1 + 432` 行。variant 別 row 数が `r4=144, c17s=144, senba_concurrent=144` で揃っている。

- [ ] **Step 13.3: コミット**

```bash
git add docs/benchmark/r4-vs-c17s/data/
git commit -m "$(cat <<'EOF'
bench(r4-vs-c17s): full 432-row sweep (3 variant × 4 T × 3 skew × 2 mix × 2 value × 3 trial)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 14: 図を生成 + ペアwise Δ% 集計

**Files:**
- Generate: `docs/benchmark/r4-vs-c17s/figures/*.png`

- [ ] **Step 14.1: plot.py を走らせる**

```bash
uv run --project scripts python docs/benchmark/r4-vs-c17s/plot.py \
    --csv docs/benchmark/r4-vs-c17s/data/results.csv \
    --out-dir docs/benchmark/r4-vs-c17s/figures
```

> 注: plot.py の CLI が既存 `senba-concurrent-vs-c17s/plot.py` と一致しているか確認。違うなら `--csv` / `--out-dir` の引数を実装側に揃える。

- [ ] **Step 14.2: median / worst Δ% を集計**

```bash
uv run --project scripts python -c "
import pandas as pd
df = pd.read_csv('docs/benchmark/r4-vs-c17s/data/results.csv')
agg = df.groupby(['variant','value','threads','skew','op_mix'])['aggregate_mops'].median().reset_index()
piv = agg.pivot_table(index=['value','threads','skew','op_mix'], columns='variant', values='aggregate_mops')
piv['r4_vs_c17s_pct'] = (piv['r4'] / piv['c17s'] - 1) * 100
piv['r4_vs_senba_pct'] = (piv['r4'] / piv['senba_concurrent'] - 1) * 100
print(piv.to_string())
print()
print('=== summary ===')
for value in ['u64','string']:
    sub = piv.xs(value, level='value')
    print(f'V={value}: median r4_vs_c17s = {sub[\"r4_vs_c17s_pct\"].median():.1f}%, worst = {sub[\"r4_vs_c17s_pct\"].min():.1f}%')
    print(f'V={value}: median r4_vs_senba = {sub[\"r4_vs_senba_pct\"].median():.1f}%, worst = {sub[\"r4_vs_senba_pct\"].min():.1f}%')
" | tee docs/benchmark/r4-vs-c17s/data/summary.txt
```

期待 (設計 §9.4 / §G4):
- V=u64: r4_vs_c17s median ≥ −5%, worst ≥ −10%
- V=String: r4_vs_senba median ≥ +30%, worst ≥ +20%

**満たさない cell の特定**: summary.txt の最も悪い 3 cell を pick して、その workload で `bench_concurrent` を VTune (Windows native) または `perf stat` (Linux) で counter 測定し、原因を後続 Open Question として記録。

- [ ] **Step 14.3: コミット**

```bash
git add docs/benchmark/r4-vs-c17s/figures/ docs/benchmark/r4-vs-c17s/data/summary.txt
git commit -m "$(cat <<'EOF'
bench(r4-vs-c17s): plot figures + median/worst Δ% summary

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 15: 比較レポート `docs/reports/2026-05-15-r4-vs-c17s.md` を書く

**Files:**
- Create: `docs/reports/2026-05-15-r4-vs-c17s.md`
- Modify: `docs/reports/index.md`

- [ ] **Step 15.1: レポート骨子 (hypothesis → action → result)**

`docs/reports/2026-05-15-r4-vs-c17s.md` を以下構造で書く:

```markdown
# 2026-05-15 — sieve_r4 vs c17s vs senba::concurrent (3-way 432-cell sweep)

## TL;DR

- 仮説: c17s の entry-version seqlock + crossbeam-epoch を refcount 抜きで合成すれば
  senba::concurrent (Arc<V>+epoch、median -34% / worst -63% vs c17s) の退行を回収しつつ
  c17s の reader hot path atomic shape (write 0) を保てる。
- やったこと: `sieve_r4` を c17s skeleton + ManuallyDrop wrap + crossbeam-epoch defer で
  実装、3-way 432-cell sweep (V=u64/string × 4T × 3skew × 2mix × 3trial) で計測。
- 分かったこと: V=u64 で median ±X%、V=String で senba 比 +Y%。
  [accept 基準: V=u64 median ≥ -5%、V=String vs senba median ≥ +30%]

## Sweep matrix と環境

- 参照: `docs/benchmark/r4-vs-c17s/run.sh` (axis 詳細)、`docs/benchmark/r4-vs-c17s/data/results.csv`
- 環境: WSL2 Ubuntu / Alder Lake P-core 16T (caveat: WSL2 計測 bias、Phase 4 で
  Windows native VTune / bare Linux 再走の必要)。

## Result (figures)

[plot.py の出力を貼る]

## Pairwise Δ% (median / worst)

| value | metric | r4 vs c17s | r4 vs senba_concurrent |
|-------|--------|-----------:|-----------------------:|
| u64    | median | x.x% | x.x% |
| u64    | worst  | x.x% | x.x% |
| string | median | x.x% | x.x% |
| string | worst  | x.x% | x.x% |

## 観測された surprise / refutation

(任意。例: V=u64 で c17s と完全同等にならず -3% 残る場合は const-fold が
 浅かった等の追加分析を書く)

## Phase 4 (lib integration) への引き継ぎ

- accept 基準を満たした cell / 満たさなかった cell の境界を明記
- `senba::concurrent::Cache` 置換時の API 差分 (Path C 経由の None 返し) を
  Phase 4 計画で再設計
- 残る Open Question (設計 §14): OQ1 (const-fold), OQ4 (closure size), OQ5 (命名)
```

- [ ] **Step 15.2: index に登録**

`docs/reports/index.md` の最新行 (2026-05-14 の r3-vs-c17s の下) に 1 段落追加 (`*"Hypothesis: X. Did Y. Found Z."*` 3-5 行):

```markdown
- [2026-05-15 — sieve_r4 vs c17s vs senba::concurrent](2026-05-15-r4-vs-c17s.md)
  Hypothesis: c17s の entry-version seqlock を race α 防御に残し、crossbeam-epoch を
  refcount 抜きで race β 防御に被せれば、Arc<V> 退行 (-34% median) を構造的に回収できる。
  Did: sieve_r4 を実装し 3-way 432-cell sweep (V=u64+string)。
  Found: V=u64 で c17s と median ±X%、V=String で senba 比 median +Y% を回収 [/しなかった]。
```

- [ ] **Step 15.3: コミット**

```bash
git add docs/reports/2026-05-15-r4-vs-c17s.md docs/reports/index.md
git commit -m "$(cat <<'EOF'
report(r4-vs-c17s): 432-cell 3-way sweep result + Phase 4 handoff

Phase 3 accept 基準 [pass/fail] と次フェーズ (lib 置換) への gating を整理。

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

**Phase 3 完了 gate**:
- ✅ 432-row sweep CSV が `docs/benchmark/r4-vs-c17s/data/results.csv` に存在
- ✅ figures が生成され `docs/benchmark/r4-vs-c17s/figures/` に存在
- ✅ レポートが `docs/reports/2026-05-15-r4-vs-c17s.md` に書かれ index に登録済み
- ✅ Accept 基準の達否を summary.txt に明記 (= Phase 4 進行可否の判断材料)

---

## 計画外の罠 (実装中に踏みうるもの)

実装中 plan に書いてない問題に当たったら以下を疑う (設計 §10 参照):

- **`mem::needs_drop` の fold が release で効かない** (§10.6, §11.3): Task 7 で先に検出。`#[inline(always)]` の追加 / `pin_for` を const branch でなく `if needs_epoch::<V>()` の trivial form にして const-prop を助ける。
- **Reclamation backlog 暴走** (§10.5): writer-heavy + 長寿命 reader で defer queue が膨れて RSS 暴走。Task 9/11 (TSan/ASan) の long-run で気付く。一時 mitigation: writer 側で `epoch::default_collector().try_advance()` を呼ぶ (= 設計 §10.5)。
- **Cross-shard epoch 干渉** (§10.8): 全 Shard が同じ global collector に乗るので 1 reader の pin が全 writer の defer を遅らせる。観測なら per-shard collector に分離 (= `crossbeam_epoch::Collector::new()` を Shard ごとに持つ)。本 plan では default collector で進める。
- **panic safety** (§10.3): K::Eq / V::clone / V::Drop の panic 時、ManuallyDrop で reader buf は drop されず resource leak しない、pin guard は unwind drop で release される (Rust の Drop guarantee)。defer closure 内 V::Drop panic は `std::process::abort` の可能性、これは V 側責任として doc に明記する (Task 1 の module doc 末尾に追加)。
- **API 差分による downstream 影響** (§Phase 0): r4 の `insert` は Path C 経由で常に None を返す。Phase 4 で lib に取り込むときは callback 形 (`fn insert_with(<callback for evicted>)`) などで補償する設計が必要 (本 plan の対象外、Phase 4 計画に持ち越し)。

---

## Self-review チェック (plan 自身の sanity)

- ✅ Spec coverage: 設計 §3 の 3 機構 (entry-version seqlock 継承 / crossbeam-epoch 新規 / path_c_epoch 継承) のうち、新規 = crossbeam-epoch 部分のみが Task 4-6 で扱われ、継承 2 つは Task 1 (c17s sed copy) で自動的に導入される。
- ✅ Spec §6 (soundness 証明) は plan の責務ではない (= 設計時に証明済み)。Plan は実装 + 実機検証 (Phase 2 sanitizer) で安全性を検算する。
- ✅ Spec §8 (Copy 特殊化) は Task 4 (`pin_for`) + Task 7 (`cargo asm`) で扱う。
- ✅ Spec §9 (perf model) の数値見積もりは Phase 3 sweep (Task 13-15) で実測検証。
- ✅ Spec §11 (test 戦略) のうち unit/oracle = Task 2-6、stress/TSan/ASan = Task 9, 11、miri = Task 10 で skip 判断、codegen verification = Task 7、perf sweep = Task 12-15。
- ✅ 全 step に具体的なコードまたは shell コマンドが入っている (placeholder なし)。
- ✅ 型整合性: `pin_for`, `needs_epoch`, `defer_drop_if_needed`, `defer_drop_kv_if_needed` は Task 4-6 を通じて同名で参照される。`EpochGuardWrapper::Some(epoch::Guard)` は Drop で pin release される (crossbeam-epoch upstream の実装)。

---

## Execution handoff

Plan complete and saved to `docs/reports/2026-05-14-r4-implementation-plan.md`. Two execution options:

**1. Subagent-Driven (recommended)** - 各 task ごとに fresh subagent を投げ、task 間で plan のチェックボックスを更新しつつ進める。Phase 1 → Phase 2 → Phase 3 の境界 (= Phase 完了 gate) でユーザに数値報告し継続承認を取る。

**2. Inline Execution** - 本 session で連続実行、Phase 完了 gate でチェックポイント。

どちらで進めますか?
