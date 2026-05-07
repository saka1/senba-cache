//! `senba_cache::Cache` — library-grade SIEVE implementation built on the j8 series,
//! with automatic padding via the `SlotSize` abstraction.
//!
//! Design details: `docs/reports/2026-05-06-senba-sievecache-design.md`.
//!
//! - Public type: [`Cache`]`<K, V, S = Slot32>`. Shard count is chosen automatically
//!   from `capacity` (smallest power of two with `per_shard <= MAX_PER_SHARD`).
//!   Use [`Cache::with_shards`] to override explicitly.
//! - [`SlotSize`] is a sealed trait; impls are [`Slot16`] / [`Slot32`] (default) / [`Slot64`]
//! - The entries arena uses a **fixed stride of `S::SIZE`** (= automatic padding).
//!   `sizeof(Entry<K, V>) <= S::SIZE` is enforced by const-eval with a friendly error message.
//! - The j8 c-hoist trick (`tag & ID_MASK = id × S::SIZE`) holds identically at slot granularity;
//!   the inner SIMD loop shortcut is reused as-is.
//! - **Shift-on-evict** (the key simplification vs the j-series): each steady-state
//!   `insert` evicts at the SIEVE-chosen position, shifts `tags[pos+1..len]` down by
//!   one, and writes the new tag at `tags[len-1]` (the head end). This keeps
//!   `tags[0..len]` contiguously LIVE *and* preserves the array's correspondence
//!   to `sieve_orig`'s tail→head linked-list order — `tags[0]` is always the oldest
//!   entry, `tags[len-1]` always the newest. No `compact` step is ever needed, the
//!   SIMD `find` window is always exactly `len` wide, and the eviction sequence
//!   matches `sieve_orig` byte-for-byte (oracle equivalence under any trace).
//! - `remove` does the same shift (mirroring `sieve_orig`'s linked-list unlink), and
//!   keeps the id-level swap-to-fill-gap so I8 (live ids = `0..len`) holds.
//!
//! ## Invariants (j8: I1–I8 plus the new in-place I4')
//!
//! - I4': `tags[0..len]` are all LIVE (no holes); `tags[len..]` are all EMPTY
//! - I5: entry_ids referenced by live tags are unique, count = `len`
//! - I6: only for ids in the I5 set is the **`entry` field** of `entries[id]` initialized
//! - I7: I5 set ⊆ `0..capacity`
//! - I8: live ids = `0..len` (maintained during warm-up and restored after remove via swap-to-fill-gap)

use std::borrow::Borrow;
use std::fmt;
use std::hash::{BuildHasher, Hash};
use std::marker::PhantomData;

pub mod hash;
mod inner;
mod iter;
mod slot;
mod stats;

pub use hash::Xxh3Build;
pub use iter::{Drain, Iter, IterMut, Keys, Values};
pub use slot::{Slot16, Slot32, Slot64, SlotSize};
pub use stats::Stats;

pub(crate) use inner::{EMPTY, Entry, Inner, MAX_PER_SHARD};
#[cfg(test)]
pub(crate) use inner::{LIVE, VISITED};

// ---------------- Public type Cache ----------------

/// Publishable SIEVE cache. The entry stride is specified at the type level via `SlotSize`.
/// The number of shards is chosen at construction time from `capacity`
/// (see [`Cache::new`]); use [`Cache::with_shards`] to override.
///
/// ```
/// use senba_cache::Cache;
///
/// // default Slot32: Entry<u64, String> (sizeof=32) fits exactly
/// let mut c: Cache<u64, String> = Cache::new(8);
/// c.insert(1, "hello".into());
/// assert_eq!(c.get(&1), Some(&"hello".to_string()));
/// assert_eq!(c.remove(&1), Some("hello".to_string()));
/// assert_eq!(c.get(&1), None);
/// ```
pub struct Cache<K, V, S: SlotSize = Slot32, H: BuildHasher = Xxh3Build> {
    pub(super) shards: Box<[Inner<K, V, S>]>,
    /// `shards.len() - 1`. Cached so `shard_of_hash` is a single AND.
    shard_mask: usize,
    hasher: H,
    /// AVX2 + BMI1 availability, resolved once in `new` so the SIMD dispatch in
    /// `Inner::find` is a single boolean load instead of a re-entry into
    /// `is_x86_feature_detected!` on every cache op. BMI1 is implied by AVX2 on
    /// every x86_64 CPU shipped to date, so detecting AVX2 suffices.
    has_avx2_bmi1: bool,
}

impl<K, V, S> Cache<K, V, S, Xxh3Build>
where
    K: Hash + Eq,
    S: SlotSize,
{
    /// Creates a cache with `capacity` total entries and the default
    /// [`Xxh3Build`] hasher. The shard count is the smallest power of two `N`
    /// such that `ceil(capacity / N) <= MAX_PER_SHARD`, i.e.
    /// `N = next_pow2(ceil(capacity / MAX_PER_SHARD))` (clamped to ≥ 1).
    /// The 6-bit per-shard id field then accommodates every entry without
    /// further tuning.
    pub fn new(capacity: usize) -> Self {
        Self::with_hasher(capacity, Xxh3Build)
    }

    /// Creates a cache with an explicit shard count and the default
    /// [`Xxh3Build`] hasher. `shards` must be a power of two, `>= 1`, and
    /// small enough that `ceil(capacity / shards) <= MAX_PER_SHARD` holds.
    /// Mainly useful for benchmarking / oracle comparison; prefer
    /// [`Cache::new`] in production code.
    pub fn with_shards(capacity: usize, shards: usize) -> Self {
        Self::with_shards_and_hasher(capacity, shards, Xxh3Build)
    }
}

impl<K, V, S, H> Cache<K, V, S, H>
where
    K: Hash + Eq,
    S: SlotSize,
    H: BuildHasher,
{
    /// Creates a cache with `capacity` total entries and the supplied
    /// [`BuildHasher`]. Auto-shards as in [`Cache::new`].
    pub fn with_hasher(capacity: usize, hasher: H) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        let n_min = capacity.div_ceil(MAX_PER_SHARD).max(1);
        let shards = n_min.next_power_of_two();
        Self::with_shards_and_hasher(capacity, shards, hasher)
    }

    /// Creates a cache with an explicit shard count and the supplied
    /// [`BuildHasher`]. `shards` must be a power of two, `>= 1`, and small
    /// enough that `ceil(capacity / shards) <= MAX_PER_SHARD` holds.
    pub fn with_shards_and_hasher(capacity: usize, shards: usize, hasher: H) -> Self {
        assert!(shards > 0, "shards must be > 0");
        assert!(
            shards.is_power_of_two(),
            "shards ({shards}) must be a power of two so shard select can be a bit mask"
        );
        assert!(
            capacity >= shards,
            "capacity ({capacity}) must be >= shards ({shards}) so each shard has cap >= 1"
        );
        let base = capacity / shards;
        let extra = capacity % shards;
        let inners: Vec<Inner<K, V, S>> = (0..shards)
            .map(|i| {
                let cap_i = base + if i < extra { 1 } else { 0 };
                Inner::new(cap_i)
            })
            .collect();
        let has_avx2_bmi1 = {
            #[cfg(target_arch = "x86_64")]
            {
                std::is_x86_feature_detected!("avx2")
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                false
            }
        };
        Self {
            shards: inners.into_boxed_slice(),
            shard_mask: shards - 1,
            hasher,
            has_avx2_bmi1,
        }
    }

    pub fn capacity(&self) -> usize {
        self.shards.iter().map(|s| s.capacity).sum()
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.len == 0)
    }

    /// Number of shards in this cache (always a power of two).
    pub fn shards(&self) -> usize {
        self.shard_mask + 1
    }

    /// Returns aggregated [`Stats`] counters across every shard. See the
    /// [`Stats`] doc for what each field counts.
    pub fn stats(&self) -> Stats {
        let mut s = Stats::default();
        for sh in self.shards.iter() {
            s.hits += sh.hits;
            s.misses += sh.misses;
            s.insertions += sh.insertions;
            s.evictions += sh.evictions;
        }
        s
    }

    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        self.shards[self.shard_of_hash(h)].contains(key, h, self.has_avx2_bmi1)
    }

    #[inline]
    pub fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        let i = self.shard_of_hash(h);
        self.shards[i].get(key, h, self.has_avx2_bmi1)
    }

    /// Returns a mutable reference to the value for `key`. Sets the SIEVE
    /// VISITED bit on hit (same as `get`), so in-place updates count as
    /// access for eviction purposes.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        let i = self.shard_of_hash(h);
        self.shards[i].get_mut(key, h, self.has_avx2_bmi1)
    }

    #[inline]
    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        let h = self.hasher.hash_one(&key);
        let i = self.shard_of_hash(h);
        self.shards[i].insert(key, value, h, self.has_avx2_bmi1)
    }

    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        let i = self.shard_of_hash(h);
        self.shards[i].remove(key, h, self.has_avx2_bmi1)
    }

    /// Non-promoting lookup: returns a reference to the value without setting
    /// the SIEVE VISITED bit. Use this when you want to inspect an entry
    /// without affecting its eviction priority.
    pub fn peek<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        self.shards[self.shard_of_hash(h)].peek(key, h, self.has_avx2_bmi1)
    }

    /// Non-promoting `&mut V` lookup. Same as `get_mut` but does not set
    /// VISITED, so in-place updates do not affect SIEVE eviction priority.
    /// Useful for housekeeping writes (counters, timestamps) that should not
    /// count as logical access.
    pub fn peek_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        let i = self.shard_of_hash(h);
        self.shards[i].peek_mut(key, h, self.has_avx2_bmi1)
    }

    /// Like `get`, but also returns a reference to the stored key. Sets
    /// VISITED on hit. Useful when looking up via `Borrow<Q>` and the
    /// canonical `&K` is wanted (e.g. `Cache<String, V>` looked up with
    /// `&str`).
    pub fn get_key_value<Q>(&mut self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        let i = self.shard_of_hash(h);
        self.shards[i].get_key_value(key, h, self.has_avx2_bmi1)
    }

    /// Non-promoting variant of `get_key_value`: returns `(&K, &V)` without
    /// setting VISITED.
    pub fn peek_key_value<Q>(&self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let h = self.hasher.hash_one(key);
        self.shards[self.shard_of_hash(h)].peek_key_value(key, h, self.has_avx2_bmi1)
    }

    /// Drops every entry, leaving the cache empty. Capacity, shard layout,
    /// and the SIEVE hand are reset; subsequent inserts behave as if on a
    /// freshly constructed cache.
    pub fn clear(&mut self) {
        for sh in self.shards.iter_mut() {
            sh.clear();
        }
    }

    /// Retains only the entries for which `f(&k, &mut v)` returns `true`.
    /// Order of visitation is unspecified. Survivors keep their VISITED state
    /// — `retain` is a non-promoting maintenance operation and does not
    /// affect SIEVE eviction priority for the entries it leaves behind
    /// (mirrors `iter` / `peek`). If `f` panics the cache is left empty but
    /// in a consistent state; the panic resumes after cleanup.
    ///
    /// Linear in the number of live entries: a single in-place compaction
    /// pass per shard, with no per-deletion hash lookup or memmove (unlike
    /// calling `remove` in a loop, which is `O(k·n)` per shard).
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&K, &mut V) -> bool,
    {
        for sh in self.shards.iter_mut() {
            sh.retain(&mut f);
        }
    }

    /// Returns an iterator over `(&K, &V)` pairs across all shards.
    /// Iteration order is unspecified and may change between releases — SIEVE
    /// has no LRU/MRU concept, and shards are walked in `shard_of_hash` order.
    /// Iteration does not set VISITED bits, so it does not affect eviction.
    pub fn iter(&self) -> Iter<'_, K, V, S> {
        Iter {
            shards: &self.shards,
            shard_idx: 0,
            slot_idx: 0,
        }
    }

    /// Returns an iterator over `(&K, &mut V)` pairs across all shards.
    /// Iteration order matches [`Cache::iter`] and is non-promoting (no
    /// VISITED bit is set on visited entries). Mutating values through this
    /// iterator does not change SIEVE eviction priority.
    pub fn iter_mut(&mut self) -> IterMut<'_, K, V, S> {
        let n = self.shards.len();
        IterMut {
            shards: self.shards.as_mut_ptr(),
            n_shards: n,
            shard_idx: 0,
            slot_idx: 0,
            _marker: PhantomData,
        }
    }

    /// Returns an iterator over `&K` for every live entry. Same order and
    /// non-promoting semantics as [`Cache::iter`].
    pub fn keys(&self) -> Keys<'_, K, V, S> {
        Keys { iter: self.iter() }
    }

    /// Returns an iterator over `&V` for every live entry. Same order and
    /// non-promoting semantics as [`Cache::iter`].
    pub fn values(&self) -> Values<'_, K, V, S> {
        Values { iter: self.iter() }
    }

    /// Removes every entry from the cache and returns an iterator over the
    /// owned `(K, V)` pairs.
    ///
    /// The cache is logically emptied as soon as `drain` is called: the
    /// returned [`Drain`] borrows the cache exclusively, [`Cache::len`]
    /// reports `0` for the lifetime of that borrow, and any entry that has
    /// not yet been yielded by the iterator is dropped when the [`Drain`]
    /// is dropped. Capacity, shard layout, and the chosen hasher are
    /// preserved; the cache is fully reusable once the [`Drain`] goes out
    /// of scope. The SIEVE hand is reset to 0 (the previous value would be
    /// meaningless against an empty live region).
    ///
    /// # Leak amplification
    ///
    /// As with [`std::vec::Vec::drain`] and
    /// [`std::collections::HashMap::drain`], leaking the returned iterator
    /// (e.g. via [`std::mem::forget`]) leaks every entry that was not yet
    /// yielded. The cache itself remains in a consistent and usable state
    /// — it does not hold pointers into the leaked entries — so subsequent
    /// inserts behave as on a freshly emptied cache (any storage previously
    /// occupied by leaked entries is overwritten in place by future
    /// inserts, leaking the originals' `K` and `V` allocations as expected
    /// from `mem::forget`).
    ///
    /// # Order and statistics
    ///
    /// Iteration order matches [`Cache::iter`] (shard order, then slot
    /// order within a shard) and is unspecified — SIEVE has no LRU/MRU
    /// concept, so the order leaks implementation details. Entries are
    /// dropped without incrementing [`Stats::evictions`]; like `clear` and
    /// `retain`, draining is treated as explicit removal rather than
    /// capacity-driven eviction.
    pub fn drain(&mut self) -> Drain<'_, K, V, S, H> {
        Drain::new(self)
    }

    /// Returns a reference to the value for `key`, or inserts the result of
    /// `f()` and returns a reference to it. The closure is only evaluated on
    /// a miss. On a hit, the entry's VISITED bit is set (same as `get`).
    /// Inserting may evict another entry; the evicted `(K, V)` is dropped
    /// (no listener API).
    pub fn get_or_insert_with<F>(&mut self, key: K, f: F) -> &V
    where
        F: FnOnce() -> V,
    {
        let h = self.hasher.hash_one(&key);
        let i = self.shard_of_hash(h);
        self.shards[i].get_or_insert_with(key, h, self.has_avx2_bmi1, f)
    }

    #[inline]
    fn shard_of_hash(&self, hash: u64) -> usize {
        (hash as usize) & self.shard_mask
    }
}

impl<K, V, S, H> Clone for Cache<K, V, S, H>
where
    K: Hash + Eq + Clone,
    V: Clone,
    S: SlotSize,
    H: BuildHasher + Clone,
{
    fn clone(&self) -> Self {
        Self {
            shards: self.shards.to_vec().into_boxed_slice(),
            shard_mask: self.shard_mask,
            hasher: self.hasher.clone(),
            has_avx2_bmi1: self.has_avx2_bmi1,
        }
    }
}

impl<K, V, S, H> fmt::Debug for Cache<K, V, S, H>
where
    K: fmt::Debug + Hash + Eq,
    V: fmt::Debug,
    S: SlotSize,
    H: BuildHasher,
{
    /// Renders the cache as a map of its current entries plus capacity / shard
    /// metadata. Iteration order is unspecified (`Cache::iter` order); the
    /// VISITED bit is **not** set, so `Debug` printing does not affect SIEVE
    /// eviction priority.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cache")
            .field("capacity", &self.capacity())
            .field("len", &self.len())
            .field("shards", &self.shards())
            .field("entries", &DebugEntries(self))
            .finish()
    }
}

struct DebugEntries<'a, K, V, S: SlotSize, H: BuildHasher>(&'a Cache<K, V, S, H>);

impl<K, V, S, H> fmt::Debug for DebugEntries<'_, K, V, S, H>
where
    K: fmt::Debug + Hash + Eq,
    V: fmt::Debug,
    S: SlotSize,
    H: BuildHasher,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.0.iter()).finish()
    }
}

impl<'a, K, V, S, H> IntoIterator for &'a Cache<K, V, S, H>
where
    K: Hash + Eq,
    S: SlotSize,
    H: BuildHasher,
{
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V, S>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, K, V, S, H> IntoIterator for &'a mut Cache<K, V, S, H>
where
    K: Hash + Eq,
    S: SlotSize,
    H: BuildHasher,
{
    type Item = (&'a K, &'a mut V);
    type IntoIter = IterMut<'a, K, V, S>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<K, V, S, H> Extend<(K, V)> for Cache<K, V, S, H>
where
    K: Hash + Eq,
    S: SlotSize,
    H: BuildHasher,
{
    /// Inserts every `(K, V)` from `iter` via [`Cache::insert`]. Pairs evicted
    /// by capacity pressure during the loop are dropped silently; if you need
    /// to observe them, call `insert` yourself in a loop.
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.insert(k, v);
        }
    }
}

impl<'a, K, V, S, H> Extend<(&'a K, &'a V)> for Cache<K, V, S, H>
where
    K: Hash + Eq + Copy,
    V: Copy,
    S: SlotSize,
    H: BuildHasher,
{
    fn extend<I: IntoIterator<Item = (&'a K, &'a V)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.insert(*k, *v);
        }
    }
}

// `Iter` / `IterMut` / `Keys` / `Values` / `Drain` live in `iter.rs`.

// `CacheImpl` intentionally does **not** expose `remove` (`is_empty` has a default
// impl on the trait). All sibling variants (sieve_orig, sieve_v*, sieve_j*) follow
// the same convention, so cross-variant bench / oracle drivers stay symmetric.
// `Cache::remove` is available on the inherent impl above when needed directly.
//
// Gated behind the `experimental` feature because `CacheImpl` is research /
// dev tooling — see `src/experimental/mod.rs` for the trait definition.
#[cfg(feature = "experimental")]
impl<K, V, S> crate::CacheImpl<K, V> for Cache<K, V, S>
where
    K: Hash + Eq,
    S: SlotSize,
{
    fn new(capacity: usize) -> Self {
        Self::new(capacity)
    }
    fn capacity(&self) -> usize {
        self.capacity()
    }
    fn len(&self) -> usize {
        self.len()
    }
    fn get(&mut self, key: &K) -> Option<&V> {
        self.get(key)
    }
    fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        self.insert(key, value)
    }
    fn contains_key(&self, key: &K) -> bool {
        self.contains_key(key)
    }
}

#[cfg(test)]
mod tests;
