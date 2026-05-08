//! Single-shard concurrent SIEVE testbed.
//!
//! 並行 SIEVE の **shard 内側 1 個分** を抽象化し、c8/c9 / 将来の c10..c1n を
//! 同じハーネスで A/B できるようにする。`research/src/bin/bench_single_shard.rs`
//! が消費する。
//!
//! # 何故これが要るか
//!
//! 既存の `bench_concurrent` は 256 shards 全体に Zipf を当てるので、
//! 「shard 分散効果」と「単一 shard 内の並行スケーリング限界」が常に混ざった
//! 量しか測れない。`senba::concurrent::Cache` の最終形 (= c8 lineage で
//! lock-free reader) は単一 shard 性能が支配項なので、shard 内側だけを
//! 取り出して直接攻める観測装置が要る。
//!
//! # トレイト
//!
//! [`SingleShard`] は最小契約だけを規定し、`Copy` 等の追加 bound は
//! 各 impl 側で課す。これは c8 (`V: Copy` 必須) と c9 (`V: Clone`) と
//! 将来の epoch GC ベース実装 (`V: Send + Sync` のみ) を全部素直に
//! 受け入れるための判断。
//!
//! 主 API は **closure-based** `read<R>(&self, key, |&V| -> R) -> Option<R>`。
//! c8 は torn-safe な local copy を持って `f(&local)` を呼ぶ。c9 は Mutex
//! critical section の中で `f(&v)` を呼ぶ。`get -> Option<V>` は
//! `where V: Clone` の default method として `read` の上に乗る。

use std::hash::Hash;

/// 並行 SIEVE shard の最小契約。
///
/// `&self` で全操作を受ける (`Mutex` で wrap するか lock-free にするかは impl
/// 側の判断)。`read` が primary、`get` は `read` の上に乗った convenience。
/// `insert` は新規キーかどうかだけを返す (evicted 値は testbed では使わない)。
pub trait SingleShard<K, V>: Send + Sync
where
    K: Hash + Eq + Send + Sync,
    V: Send + Sync,
{
    fn new(capacity: usize) -> Self
    where
        Self: Sized;

    fn capacity(&self) -> usize;
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `key` を look-up し、見つかったら `f` に値の参照を渡してその返り値を返す。
    /// 見つからなければ `None`。VISITED bit を立てる side effect を許容する。
    fn read<R>(&self, key: &K, f: impl FnOnce(&V) -> R) -> Option<R>;

    /// 新規 insert なら `true` (新キー or evict してでも入れた)、既存 update なら
    /// `false`。bench harness 用の最小情報。
    fn insert(&self, key: K, value: V) -> bool;

    /// 値を clone して返す convenience。`read` を default body として使う。
    fn get(&self, key: &K) -> Option<V>
    where
        V: Clone,
    {
        self.read(key, V::clone)
    }
}

pub mod adapters {
    //! 既存変種から [`SingleShard`] を提供する adapter 群。

    use super::SingleShard;
    use crate::experimental::sieve_c8;
    use crate::experimental::sieve_c9;
    use crate::experimental::sieve_c10s;
    use crate::experimental::sieve_c11s;
    use crate::experimental::sieve_c12s;
    use crate::experimental::sieve_c13s;
    use senba::Xxh3Build;
    use std::hash::{BuildHasher, Hash};

    /// c8 の内部 [`sieve_c8::Shard`] を直接 wrap。フル `ConcurrentSieveCache`
    /// を組まないので shard 数 = 1 の純粋 single-shard。
    ///
    /// hash 計算は adapter 内で `Xxh3Build` を使って行い (c8 の Cache が使うのと
    /// 同じ hasher)、`Shard::get_by_hash` / `Shard::insert` に橋渡しする。
    pub struct C8SingleShard<K, V> {
        shard: sieve_c8::Shard<K, V>,
        hasher: Xxh3Build,
    }

    impl<K, V> C8SingleShard<K, V>
    where
        K: Hash + Eq + Copy,
        V: Copy,
    {
        pub fn new(capacity: usize) -> Self {
            Self {
                shard: sieve_c8::Shard::new(capacity),
                hasher: Xxh3Build,
            }
        }
    }

    impl<K, V> SingleShard<K, V> for C8SingleShard<K, V>
    where
        K: Hash + Eq + Copy + Send + Sync,
        V: Copy + Send + Sync,
    {
        fn new(capacity: usize) -> Self {
            Self::new(capacity)
        }

        fn capacity(&self) -> usize {
            self.shard.capacity()
        }

        fn len(&self) -> usize {
            self.shard.len()
        }

        fn read<R>(&self, key: &K, f: impl FnOnce(&V) -> R) -> Option<R> {
            let h = self.hasher.hash_one(key);
            // c8 の find_get は torn-safe な local Copy を返す (V: Copy 制約の根拠)。
            // ここで一度 local に持ったあとで `f(&local)` を呼ぶ。
            let v = self.shard.get_by_hash(key, h)?;
            Some(f(&v))
        }

        fn insert(&self, key: K, value: V) -> bool {
            let h = self.hasher.hash_one(key);
            // Shard::insert の戻り値は evicted Option<(K,V)>。返り値の意味:
            //   None  → 既存キー update or warmup (空きあり) で新規入れ
            //   Some(_) → cap 到達で evict してから新規入れた
            // SingleShard::insert の "true if new key" 契約に対しては
            //   既存キー update を区別する必要がある。c8 は writer_find が
            //   既存キー hit なら writer_update_in_place を呼んで None を返す
            //   経路と、空 slot に新規 install で None を返す経路が両方あり、
            //   None 同士で「新規 vs update」が判別できない。
            // testbed の利用観点では「writer 経路に入ったか」と「evict 発生有無」
            //   の方が重要なので、ここでは evict あり = true / なし = false で返す。
            //   (true = "shard が一杯で evict した" / false = "空きあり or update")
            self.shard.insert(key, value, h).is_some()
        }
    }

    /// c10s の内部 [`sieve_c10s::Shard`] を直接 wrap。c8 と同形 (visited 分離のみが差分)。
    pub struct C10sSingleShard<K, V> {
        shard: sieve_c10s::Shard<K, V>,
        hasher: Xxh3Build,
    }

    impl<K, V> C10sSingleShard<K, V>
    where
        K: Hash + Eq + Copy,
        V: Copy,
    {
        pub fn new(capacity: usize) -> Self {
            Self {
                shard: sieve_c10s::Shard::new(capacity),
                hasher: Xxh3Build,
            }
        }
    }

    impl<K, V> SingleShard<K, V> for C10sSingleShard<K, V>
    where
        K: Hash + Eq + Copy + Send + Sync,
        V: Copy + Send + Sync,
    {
        fn new(capacity: usize) -> Self {
            Self::new(capacity)
        }

        fn capacity(&self) -> usize {
            self.shard.capacity()
        }

        fn len(&self) -> usize {
            self.shard.len()
        }

        fn read<R>(&self, key: &K, f: impl FnOnce(&V) -> R) -> Option<R> {
            let h = self.hasher.hash_one(key);
            let v = self.shard.get_by_hash(key, h)?;
            Some(f(&v))
        }

        fn insert(&self, key: K, value: V) -> bool {
            let h = self.hasher.hash_one(key);
            // C8SingleShard と同じ契約: evict あり = true / なし (空き or update) = false。
            self.shard.insert(key, value, h).is_some()
        }
    }

    /// c11s の内部 [`sieve_c11s::Shard`] を直接 wrap。c10s と同形 (conditional
    /// visited set のみが差分)。
    pub struct C11sSingleShard<K, V> {
        shard: sieve_c11s::Shard<K, V>,
        hasher: Xxh3Build,
    }

    impl<K, V> C11sSingleShard<K, V>
    where
        K: Hash + Eq + Copy,
        V: Copy,
    {
        pub fn new(capacity: usize) -> Self {
            Self {
                shard: sieve_c11s::Shard::new(capacity),
                hasher: Xxh3Build,
            }
        }
    }

    impl<K, V> SingleShard<K, V> for C11sSingleShard<K, V>
    where
        K: Hash + Eq + Copy + Send + Sync,
        V: Copy + Send + Sync,
    {
        fn new(capacity: usize) -> Self {
            Self::new(capacity)
        }

        fn capacity(&self) -> usize {
            self.shard.capacity()
        }

        fn len(&self) -> usize {
            self.shard.len()
        }

        fn read<R>(&self, key: &K, f: impl FnOnce(&V) -> R) -> Option<R> {
            let h = self.hasher.hash_one(key);
            let v = self.shard.get_by_hash(key, h)?;
            Some(f(&v))
        }

        fn insert(&self, key: K, value: V) -> bool {
            let h = self.hasher.hash_one(key);
            self.shard.insert(key, value, h).is_some()
        }
    }

    /// c12s の内部 [`sieve_c12s::Shard`] を直接 wrap。c11s と同形 (writer Mutex 完全
    /// 排除 + install-at-evicted-pos のみが差分)。
    pub struct C12sSingleShard<K, V> {
        shard: sieve_c12s::Shard<K, V>,
        hasher: Xxh3Build,
    }

    impl<K, V> C12sSingleShard<K, V>
    where
        K: Hash + Eq + Copy,
        V: Copy,
    {
        pub fn new(capacity: usize) -> Self {
            Self {
                shard: sieve_c12s::Shard::new(capacity),
                hasher: Xxh3Build,
            }
        }
    }

    impl<K, V> SingleShard<K, V> for C12sSingleShard<K, V>
    where
        K: Hash + Eq + Copy + Send + Sync,
        V: Copy + Send + Sync,
    {
        fn new(capacity: usize) -> Self {
            Self::new(capacity)
        }

        fn capacity(&self) -> usize {
            self.shard.capacity()
        }

        fn len(&self) -> usize {
            self.shard.len()
        }

        fn read<R>(&self, key: &K, f: impl FnOnce(&V) -> R) -> Option<R> {
            let h = self.hasher.hash_one(key);
            let v = self.shard.get_by_hash(key, h)?;
            Some(f(&v))
        }

        fn insert(&self, key: K, value: V) -> bool {
            let h = self.hasher.hash_one(key);
            self.shard.insert(key, value, h).is_some()
        }
    }

    /// c13s の内部 [`sieve_c13s::Shard`] を直接 wrap。c11s structural skeleton +
    /// senba::Cache shift-on-evict + Path A lock-free CAS の合成変種。reader は
    /// V: Clone seqlock-clone dance を踏むので `read` は `get` 後 closure 呼び出し。
    pub struct C13sSingleShard<K, V> {
        shard: sieve_c13s::Shard<K, V>,
        hasher: Xxh3Build,
    }

    impl<K, V> C13sSingleShard<K, V>
    where
        K: Hash + Eq,
        V: Clone,
    {
        pub fn new(capacity: usize) -> Self {
            Self {
                shard: sieve_c13s::Shard::new(capacity),
                hasher: Xxh3Build,
            }
        }
    }

    impl<K, V> SingleShard<K, V> for C13sSingleShard<K, V>
    where
        K: Hash + Eq + Send + Sync,
        V: Clone + Send + Sync,
    {
        fn new(capacity: usize) -> Self {
            Self::new(capacity)
        }

        fn capacity(&self) -> usize {
            self.shard.capacity()
        }

        fn len(&self) -> usize {
            self.shard.len()
        }

        fn read<R>(&self, key: &K, f: impl FnOnce(&V) -> R) -> Option<R> {
            let h = self.hasher.hash_one(key);
            // c13s の get_by_hash は seqlock-clone dance で local owned V を返す。
            // 一度 local に持って `f(&local)` を呼ぶ (V: Clone コストは bench V=u64 で
            // trivial、production な V=String では Mutex 内 clone と同等)。
            let v = self.shard.get_by_hash(key, h)?;
            Some(f(&v))
        }

        fn insert(&self, key: K, value: V) -> bool {
            let h = self.hasher.hash_one(&key);
            // 戻り値の意味: C8/C10s/C11s/C12s と同じ契約 (evict あり = true / なし = false)
            self.shard.insert(key, value, h).is_some()
        }
    }

    /// c9 の `ConcurrentSieveCache` を `shards = 1` で構築した newtype。
    /// 中身は `Mutex<senba::Shard>` 1 個になり、c9 の素朴 `Mutex<Shard>` 戦略を
    /// 単一 shard 環境で評価する baseline。
    pub struct C9SingleShard<K, V>(sieve_c9::ConcurrentSieveCache<K, V>);

    impl<K, V> SingleShard<K, V> for C9SingleShard<K, V>
    where
        K: Hash + Eq + Send + Sync,
        V: Clone + Send + Sync,
    {
        fn new(capacity: usize) -> Self {
            Self(sieve_c9::ConcurrentSieveCache::with_shards(capacity, 1))
        }

        fn capacity(&self) -> usize {
            self.0.capacity()
        }

        fn len(&self) -> usize {
            self.0.len()
        }

        fn read<R>(&self, key: &K, f: impl FnOnce(&V) -> R) -> Option<R> {
            // c9 の get は内部で Mutex を取って senba::Shard::get → V::clone する
            // 既製 API。closure 経路は無いので、いったん cloned value を local に
            // 持って f(&local) を呼ぶ。bench 用途では V: Clone コストは V=u64 だと
            // trivial、V=String の場合は Mutex 内 clone として既に必要なコスト。
            let v = self.0.get(key)?;
            Some(f(&v))
        }

        fn insert(&self, key: K, value: V) -> bool {
            // c9::insert は evicted Option<(K,V)> を返す。C8 と同じ契約 (evict あり = true) に
            // 揃える。
            self.0.insert(key, value).is_some()
        }
    }
}

pub mod workload {
    //! 単一 shard testbed 専用の補助 workload。
    //!
    //! Zipf は既存 [`crate::workload::zipf::ZipfGen`] を再利用するので、ここでは
    //! 「全 thread が同一 hot key を叩く adversarial」と「thread 間で disjoint な
    //! 帯域 cycle で叩く uniform floor」の 2 種類だけを置く。
    //!
    //! 3 軸 (zipf / adversarial-hot / uniform) の差分から:
    //!   - `uniform → zipf` の劣化 = key 分布偏りに伴う contention
    //!   - `zipf → adversarial-hot` の劣化 = visited bit ping-pong の純粋効果
    //!   - `read-only → gim` の劣化 = writer Mutex の純粋効果
    //!
    //! が分離可能になる。

    /// 全 op が key=0 を返す。SIEVE shard 内の visited bit `fetch_or` cache-line
    /// ping-pong の理論上限ストレス。
    pub struct AdversarialHot;

    impl Iterator for AdversarialHot {
        type Item = u64;
        #[inline]
        fn next(&mut self) -> Option<u64> {
            Some(0)
        }
    }

    /// thread `tid` が `[tid * span, (tid+1) * span)` の disjoint range を round-robin
    /// で cycle する。shard 内の競合がほぼゼロな floor case (各 thread が異なる
    /// key を持つ → tag 比較は通るが key も違う、結果として shard 内の異なる
    /// slot を触るので cache-line ping-pong が起きにくい)。
    ///
    /// `span` は (keys / threads) を切り上げで設定する想定。
    pub struct UniformDisjoint {
        cursor: u64,
        start: u64,
        end: u64,
    }

    impl UniformDisjoint {
        pub fn new(tid: u64, threads: u64, total_keys: u64) -> Self {
            assert!(threads > 0 && total_keys > 0);
            let span = total_keys.div_ceil(threads).max(1);
            let start = tid * span;
            let end = ((tid + 1) * span).min(total_keys.max(start + 1));
            Self {
                cursor: start,
                start,
                end,
            }
        }
    }

    impl Iterator for UniformDisjoint {
        type Item = u64;
        #[inline]
        fn next(&mut self) -> Option<u64> {
            let k = self.cursor;
            self.cursor += 1;
            if self.cursor >= self.end {
                self.cursor = self.start;
            }
            Some(k)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn adversarial_hot_returns_zero() {
            let mut it = AdversarialHot;
            for _ in 0..100 {
                assert_eq!(it.next(), Some(0));
            }
        }

        #[test]
        fn uniform_disjoint_threads_have_no_overlap() {
            let threads = 4u64;
            let total = 100u64;
            let mut seen = std::collections::HashSet::<u64>::new();
            for tid in 0..threads {
                let mut it = UniformDisjoint::new(tid, threads, total);
                let span = total.div_ceil(threads);
                for _ in 0..span {
                    let k = it.next().unwrap();
                    assert!(seen.insert(k), "duplicate key {k} across threads");
                }
            }
        }

        #[test]
        fn uniform_disjoint_cycles_within_range() {
            let mut it = UniformDisjoint::new(0, 4, 100);
            let span = 100u64.div_ceil(4); // 25
            // 1 周
            let first: Vec<_> = (&mut it).take(span as usize).collect();
            // 2 周目で同じ列が出る
            let second: Vec<_> = (&mut it).take(span as usize).collect();
            assert_eq!(first, second);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::adapters::*;
    use super::*;

    #[test]
    fn c8_single_shard_basic() {
        let s = C8SingleShard::<u64, u64>::new(64);
        assert_eq!(s.capacity(), 64);
        assert_eq!(SingleShard::len(&s), 0);
        assert!(SingleShard::is_empty(&s));
        // initial insert: no evict (warmup), so returns false ("not evicted")
        assert!(!SingleShard::insert(&s, 1u64, 10u64));
        assert_eq!(SingleShard::len(&s), 1);
        // get
        assert_eq!(SingleShard::get(&s, &1), Some(10u64));
        // read with closure
        let r = s.read(&1u64, |v| *v * 2);
        assert_eq!(r, Some(20u64));
        // miss
        assert_eq!(SingleShard::get(&s, &999u64), None);
    }

    #[test]
    fn c9_single_shard_basic() {
        let s = C9SingleShard::<u64, u64>::new(64);
        assert_eq!(s.capacity(), 64);
        assert!(!SingleShard::insert(&s, 1u64, 10u64));
        assert_eq!(SingleShard::get(&s, &1), Some(10u64));
        assert_eq!(SingleShard::get(&s, &999u64), None);
    }

    #[test]
    fn c8_single_shard_evicts_when_full() {
        let cap = 4;
        let s = C8SingleShard::<u64, u64>::new(cap);
        // fill
        for k in 0..cap as u64 {
            assert!(!SingleShard::insert(&s, k, k * 10));
        }
        assert_eq!(SingleShard::len(&s), cap);
        // overflow ⇒ evict happens, insert returns true
        let evicted = SingleShard::insert(&s, 100u64, 1000u64);
        assert!(evicted, "overflow insert should report evict");
        assert_eq!(SingleShard::len(&s), cap);
        assert_eq!(SingleShard::get(&s, &100u64), Some(1000u64));
    }
}
