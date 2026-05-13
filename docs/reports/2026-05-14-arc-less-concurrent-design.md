# 2026-05-14 — Arc-less `senba::concurrent::Cache` 設計 (c19s 仮称)

- 種別: **設計仕様 (実装前 spec)**。`docs/reports/2026-05-13-senba-concurrent-vs-c17s.md` follow-up ① (`V: Copy` specialization 案) の上位互換。
- 関連:
  - `docs/reports/2026-05-13-senba-concurrent-vs-c17s.md` — Arc<V> 退行 −34%/−63% 実測
  - `docs/reports/2026-05-14-r3-vs-c17s.md` — RwLock 路線 reject
  - `docs/reports/2026-05-13-senba-concurrent-cache-design.md` — 現 lib (Arc+epoch) 設計
  - `docs/reports/2026-05-11-c17s-design.md` — port 元 c17s seqlock 仕様
  - `research/src/experimental/sieve_c17s.rs` §76-81 — race β (`clone-mid-flight UAF`) caveat

## 0. TL;DR

c17s の **entry-version seqlock を snapshot consistency (race α) に**、crossbeam-epoch を
**V の heap reclamation (race β) に**、責務を独立に分離して合成する。Arc<V> は撤去し、
Entry に V を直置きする。reader hot path は c17s と bit-identical の atomic shape
(共有 atomic への write ゼロ) を維持しつつ、`epoch::pin` で writer の deferred drop
を hold する。

設計上の鍵:

1. **race α と race β の構造的分離**: c17s は seqlock 単独で race α (half-overwrite drop)
   は閉じているが、race β (clone-mid-flight UAF) は閉じていない。crossbeam-epoch を
   refcount ではなく **reclamation barrier** として被せることで、refcount に伴う MESI
   ping-pong を発生させずに race β を閉じる。
2. **`mem::needs_drop::<V>()` const branch による Copy 特殊化のフュージョン**: V: Copy
   では epoch path を monomorphize-time に dead code 除去し、c17s と bit-identical
   に縮退する。V: !Copy では epoch::pin ~5ns を払う。
3. **`Arc::clone` の cross-core fetch_add の消滅**: hot-key で 16 thread が同一
   `ArcInner.strong` に fetch_add する MESI ping-pong (現 lib の −48% 退行源) が、
   reader の atomic write ゼロにより構造的に解消する。

期待性能 (V=String, hot-key, T=16): 現 senba::concurrent から +50〜+150% を見込む
(c17s 近傍)。V=u64 (Copy): c17s と完全同等。

## 1. Background と問題定式化

### 1.1 c17s reader の構造

```rust
// research/src/experimental/sieve_c17s.rs:344-378
fn try_candidate(&self, pos, key, needle) -> Probe<V> {
    let t1 = self.tags[pos].load(Acquire);
    if (t1 & SCAN_MASK) != needle { return Miss; }
    let id = id_of(t1);
    let entry_ptr = entries_base.add(id);
    let v1 = (*entry_ptr).version.load(Acquire);
    if v1 & 1 != 0 { return Racing; }                              // race α 防御
    let buf = ManuallyDrop::new(ptr::read(entry_ptr));             // bitwise copy
    let v2 = (*entry_ptr).version.load(Acquire);
    if v1 != v2 { return Racing; }                                  // race α 防御
    if buf.key == *key {
        let v = buf.value.clone();                                  // ← race β: heap UAF
        // ... visited conditional set ...
        return Found(v);
    }
    Miss
}
```

### 1.2 race の分類

| race | 危険 | c17s が閉じる | senba::concurrent が閉じる | B の手当て |
|------|------|---------------|---------------------------|-----------|
| α: half-overwrite drop (`ManuallyDrop<V>` の drop で半上書き Vec/String の `free` 破壊) | ManuallyDrop::drop, SIGABRT | ✓ (entry version 奇数 pre-check) | ✓ | 継承 |
| β: clone-mid-flight UAF (reader の `V::clone` が writer の `drop(old_v)` 後の freed heap を読む) | UAF, UB | ✗ (`V: Copy` で回避) | ✓ (Arc strong-count + `epoch::defer_unchecked`) | **crossbeam-epoch を refcount 抜きで使う** |
| γ: K drop on remove (Path B remove 後 K の drop と reader の `K::Eq` が race) | UAF | ✗ | ✓ (epoch defer) | epoch defer (継承) |
| δ: Path C relocation 中の SIMD scan false-miss | miss 誤判定 | ✓ (path_c_epoch retry) | ✓ | 継承 |

B が解くべきは **β のみ**。α / γ / δ は c17s と senba::concurrent の既存機構を流用する。

### 1.3 現 senba::concurrent (Arc+epoch) の退行構造

`docs/reports/2026-05-13-senba-concurrent-vs-c17s.md` 計測: median −34%、worst −63% (T=16
gim z=1.4 で c17s 184 → senba_concurrent 67 Mops)。退行内訳の仮説:

- T=1 ~−16%: epoch::pin (~5ns) + Arc::new per miss + Arc::clone per hit (~5ns)
- T=16 ~−48%: 上記 + **Arc strong-count 単一 cache line への cross-core fetch_add 集中**

reader が hot-key を読むたび `Arc::clone` は `ArcInner.strong.fetch_add(1, Relaxed)` を
発行する。Relaxed でも RMW なので cache line を Modified 状態に変えなければならず、
16 thread が同時に同じ ArcInner を読むと cache line が thread 間を行き来する (MESI
M→I→M→I の繰り返し)。これは c17s の「reader は共有 atomic に write 一切なし」性質を
直接破壊する変更点。

B はこの fetch_add を構造的に消す。

## 2. Goals / Non-goals

### Goals

- G1. **Public API は現 `senba::concurrent::Cache<K, V>` を維持** (`V: Clone + Send + Sync + 'static`、`&self` API、`get/insert/remove -> Option<V>`)。
- G2. **reader hot path で shared atomic への write を発行しない** (visited fetch_or の conditional skip は既存 c17s 機構をそのまま継承)。
- G3. **V: Copy で c17s と bit-identical の asm** (`mem::needs_drop` の monomorphize-time fold)。
- G4. **V: !Copy で `senba::concurrent` から +50%/T=16 (hot-key)** (現 −48% 退行を半分以上回収)。
- G5. **moka 互換 soundness model 継承** (reader が pin している間 writer の defer は hold される)。

### Non-goals

- NG1. **epoch::pin overhead の完全消去** (V: !Copy で ~5ns 残る; これを消すなら hazard pointer 系の別 design)。
- NG2. **`V: !Send` / `V: !Sync` のサポート** (defer closure は Send 必須なので構造的に不可)。
- NG3. **`V: ?Sized` のサポート** (Entry が V を直置きするため Sized 必須; Arc 経由でしか dyn は載らない)。
- NG4. **moka と完全同形の eviction policy** (SIEVE を維持)。

## 3. 全体構造: 責務分離

```
┌─ Entry-version seqlock (c17s 継承) ───────────────────┐
│  責務: ManuallyDrop<Entry> の bitwise copy が単一 writer  │
│        の中間状態を観測しないこと (race α)。               │
│  primitive: AtomicU32 version, Acquire/Release pair。     │
└────────────────────────────────────────────────────────┘
┌─ crossbeam-epoch reclamation (新規導入) ──────────────┐
│  責務: reader が V::clone を完了する前に writer が V の   │
│        heap を free しないこと (race β, γ)。              │
│  primitive: epoch::pin (reader), defer_unchecked (writer)。│
└────────────────────────────────────────────────────────┘
┌─ path_c_epoch coarse retry (c17s 継承) ──────────────┐
│  責務: Path C shift transient の false-miss 回避 (race δ)。│
│  primitive: AtomicU64 counter, Acquire load + bump。      │
└────────────────────────────────────────────────────────┘
```

3 機構は **同じ atomic 変数を共有せず、独立に動く**。合成の正当性は §6 で形式的に
議論する。

## 4. Entry layout

c17s から bit-identical (Arc 抜きの c17s 元形)。

```rust
#[repr(C, align(32))]
pub(crate) struct Entry<K, V> {
    /// offset 0, 4B. seqlock counter. 偶数 = stable, 奇数 = Path A 進行中。
    /// Path C entries overwrite も flip させる。Path B Mutex 配下は flip しない
    /// (tags の Release store が代わりに publication として機能)。
    version: AtomicU32,
    /// 4B padding (alignment 維持)。c17s と同じ。
    _pad: u32,
    /// ManuallyDrop で 「Entry の owner は entries[]、reader の buf は bitwise copy
    /// なので drop しない」を型で表明。Drop 実装は Shard::drop_entries で writer
    /// 専用に走らせる。
    key: ManuallyDrop<K>,
    value: ManuallyDrop<V>,
}

// SAFETY 補強コメント:
// sizeof(Entry<u64, u64>) = 32 (4 + 4 + 8 + 8 + pad8)。
// sizeof(Entry<u64, String>) = 64 (Arch64; String=24B + alignment) — c17s と同じ。
// Arc<V> は使わない。V を直置きする。
```

c17s 比の差分:

- `Entry.value: Arc<V>` ではなく `ManuallyDrop<V>` に戻す (現 senba::concurrent の
  Arc を剥がす)。
- `Entry.key: K` ではなく `ManuallyDrop<K>` (c17s は K の所有を entries[] に置いた
  まま reader buf を bitwise copy するので reader 側で drop 抑止が必須)。

## 5. Reader protocol

### 5.1 step-by-step

```rust
// senba::concurrent::Cache::get → Shard::get_by_hash
pub fn get_by_hash<Q>(&self, key: &Q, hash: u64) -> Option<V>
where Q: Hash + Eq + ?Sized, K: Borrow<Q>
{
    // R1. epoch::pin (V: !Copy 時のみ実体化、V: Copy では mem::needs_drop の
    //     monomorphize-time fold で削除される)。
    let _guard: PinGuard = pin_for::<V>();

    const MAX_READER_RETRY: usize = 4;
    let needle = needle_from_hash(hash);
    for attempt in 0..MAX_READER_RETRY {
        // R2. path_c_epoch snapshot (race δ retry の左端)。
        let epoch_before = self.hot.path_c_epoch.load(Acquire);

        // R3. AVX2 SIMD scan + try_candidate (詳細 §5.2)。
        let (v, racing) = unsafe { self.find_get(key, needle) };
        if let Some(v) = v { return Some(v); }

        // R4. path_c_epoch snapshot (race δ retry の右端)。
        let epoch_after = self.hot.path_c_epoch.load(Acquire);
        if !racing && epoch_before == epoch_after { return None; }

        if attempt + 1 < MAX_READER_RETRY { hint::spin_loop(); }
    }
    None
    // _guard drops → reader unpins → writer's deferred drops can be reclaimed.
}
```

### 5.2 `try_candidate` (race β を閉じる箇所)

```rust
fn try_candidate<Q>(&self, pos: usize, key: &Q, needle: u16) -> Probe<V>
where Q: Hash + Eq + ?Sized, K: Borrow<Q>
{
    // R5. tag re-load + needle match.
    let t1 = self.tags[pos].load(Acquire);
    if (t1 & SCAN_MASK) != needle { return Miss; }
    let id = id_of(t1);
    let entry_ptr = self.entries_ptr().add(id) as *const Entry<K, V>;

    // R6. tier 1: entry version 偶奇 + 一致 (race α 防御)。
    let v1 = (*entry_ptr).version.load(Acquire);
    if v1 & 1 != 0 { return Racing; }

    // R7. bitwise copy. ManuallyDrop で reader 側 drop を抑止。
    //     SAFETY: K, V の真の owner は entries[id]; buf は bitwise copy。
    let buf: ManuallyDrop<Entry<K, V>> = ManuallyDrop::new(ptr::read(entry_ptr));

    // R8. compiler_fence(Acquire) — 必須。詳細 §6.3。
    //     LLVM が R7 の non-atomic loads を R9 の atomic load 後ろに reorder する
    //     のを禁止する。x86 hardware 上では TSO で隠蔽されるが、IR 段階の
    //     reorder を防ぐためにソース側で明示。
    atomic::compiler_fence(Acquire);

    // R9. tier 1 close: v2 一致確認。
    let v2 = (*entry_ptr).version.load(Acquire);
    if v1 != v2 {
        // buf は bit-inconsistent な可能性。K::Eq も V::clone も呼ばずに Racing。
        // ManuallyDrop なので buf の drop は走らない (resource leak も double-free
        // もなし)。
        return Racing;
    }

    // R10. K compare. buf.key は consistent snapshot (R6-R9 で証明)。
    //      Q::Borrow と Hash::Eq は user 提供; panic-free を想定するが、panic 時
    //      は §7.3 panic safety で議論。
    if (*buf.key).borrow() != key { return Miss; }

    // R11. V::clone. ここが race β の核心。
    //      pin が R1 で取られているため、buf.value が指す V の heap は
    //      writer の deferred drop closure に保持されており alive (§6.2 証明)。
    let v: V = (*buf.value).clone();

    // R12. visited bit conditional set (c11s 由来、c17s 継承)。
    //      MESI ping-pong 回避: 既に立っていれば fetch_or を撃たない。
    let mask = vbit_mask(pos);
    if self.hot.visited.load(Relaxed) & mask == 0 {
        self.hot.visited.fetch_or(mask, Relaxed);
    }
    return Found(v);
    // buf の drop はここで走らない (ManuallyDrop)。v だけが reader stack 上に
    // 残り return される。
}
```

### 5.3 atomic shape の比較

| step | c17s | 現 senba::concurrent | B (c19s) |
|------|------|---------------------|----------|
| R1 pin | (なし) | `epoch::pin` | `epoch::pin` (V:!Copy 時) |
| R2 path_c_epoch | load Acquire | load Acquire | load Acquire |
| R5 tag | load Acquire | load Acquire | load Acquire |
| R6 v1 | load Acquire | load Acquire | load Acquire |
| R7 ptr::read | non-atomic | non-atomic | non-atomic |
| R8 fence | (暗黙的依存) | (暗黙的依存) | **compiler_fence(Acquire) 明示** |
| R9 v2 | load Acquire | load Acquire | load Acquire |
| R11 clone | `V::clone` | `Arc::clone` + `(*owned).clone()` | `V::clone` |
| R12 visited | conditional fetch_or | conditional fetch_or | conditional fetch_or |
| pin drop | (なし) | guard drop | guard drop (V:!Copy 時) |

**shared atomic write 数**: c17s = 0 (visited 既 set 時)、現 lib = 1 (Arc fetch_add) + 1
(Arc decrement on owned drop) = **2 RMW per hot-key read**、B = **0** (visited 既 set 時)。

これが hot-key スループットを構造的に取り戻す核心。

## 6. Soundness 証明

### 6.1 race α (half-overwrite drop)

c17s の証明をそのまま継承。R6 の `v1 & 1 != 0` check で writer 進行中なら ptr::read
をスキップする。Path B/C も version flip (Path C entries overwrite 時) または
tags shift の Release store による publication で reader を invalidate する。
詳細は `2026-05-11-c17s-design.md` §6 参照。

### 6.2 race β (clone-mid-flight UAF) — 本仕様の主題

#### 6.2.1 主張

**reader R が V::clone を呼んでいる時刻 t に対し、V::clone が読む V の heap
allocation は時刻 t において alive である。**

#### 6.2.2 証明

writer W が Path A で旧 V を retire する手順 (§7.1):

```
W1. CAS version v→v+1  (Acquire/Acquire)
W2. let old_v: V = ptr::read(&entry.value)
W3. ptr::write(&mut entry.value, new_v)
W4. version.store(v+2, Release)
W5. visited.fetch_or
W6. let guard = epoch::pin();
    guard.defer_unchecked(move || drop(old_v));
```

**case I**: R の `epoch::pin` (R1) が W6 の `defer_unchecked` の **前** に happens-before
で順序づけられる場合。

  crossbeam-epoch の保証: defer_unchecked は内部的に「現在の global epoch e_w」を
  closure に紐付ける。global epoch は collector が advance させるが、「epoch e_x で
  pin されている thread が存在する限り epoch は e_x より先に進めない」のが invariant。
  R1 が W6 より前に happens-before している場合、R1 の pin は global epoch e_r ≤ e_w を
  hold する。global は e_r+2 に到達するまで W6 の defer を実行しない (crossbeam-epoch
  の bag flush 条件)。R1 の pin が release されない限り e_r+2 に到達しない。
  
  ∴ R が pin を hold している間 (= V::clone 実行中)、W6 の `drop(old_v)` は走らない。
  V の heap は alive。 ∎

**case II**: R1 が W6 の **後** で happens-before で順序づけられる場合。

  W4 の `version.store(v+2, Release)` は R6 の `version.load(Acquire)` と pairing する。
  R が W6 後に R1 を取った場合、W4 は R1 (program order で R6 より前) より前。R6 が
  読む v1 は v+2 (W4 が publish した値) または更新後の値。
  
  - subcase IIa: R6 が v+2 を観測 → R7 の ptr::read は new_v の bytes を読む。
    R8-R9 で v2 = v+2 で R10 に進む。V::clone は new_v に対して。new_v の heap は
    entries[id] が所有しており alive (W3 で install されたまま、未 retire)。 ∎
  
  - subcase IIb: R6 が v+4 以降 (別の Path A が既に走った後) を観測 → 同様に最新の
    new_v に対する V::clone。 ∎
  
  - subcase IIc: R6 が v (writer 開始前) を観測 → これは case II の前提と矛盾
    (R1 が W6 後だが R6 が W4 前を読むことは program order で不可能)。

**case III**: R1 と W6 が happens-before で順序づけられない (= concurrent) 場合。

  R1 と W6 は異なる thread でコンクリで実行され、同期点を介していない。だが R2-R9
  の seqlock 経路は W1-W4 と同期する。R6 の Acquire load が読む値で分岐する:
  
  - R6 が v を読む → W1 (W6 前の CAS) はまだ visible でない、つまり W1 は R1 と
    happens-before で順序づけられず R1 より「後」とみなせる。すると W6 も「後」。
    case I に帰着 (R1 → W6)。
  - R6 が v+1 (奇数) を読む → R6 が Racing 返す → V::clone 呼ばれない。 ∎
  - R6 が v+2 を読む → W4 は R6 より前。program order で W4 → W6 なので W6 は R6 と
    happens-before で順序づくが、R1 (R6 より前) との関係は未確定。ここで
    crossbeam-epoch の保証に頼る:
    
    R1 の pin 取得は global epoch を atomic load (Acquire) で読む。W6 の
    defer_unchecked も内部的に global epoch を atomic load する。これらは SC で
    順序づく (crossbeam-epoch の内部設計; `epoch::pin` は実装的に SeqCst fence
    に近い)。SC fence の global total order で R1 と W6 の前後が確定する:
    
    * R1 が SC 順で W6 より前 → case I。
    * R1 が SC 順で W6 より後 → R6 が v+2 を読むなら R1 も W4 visible 後の状態を
      pin しており、W6 の defer 時刻 epoch e_w を観測している。R1 の epoch e_r は
      max(e_r-prev, e_w)。global epoch が e_w+2 に進むには R1 unpin が必要。
      → V の heap reclaim は R unpin 待ち。 ∎

3 case 全てで V の heap は V::clone 実行中 alive。主張 §6.2.1 は成立。 ■

### 6.3 R8 の `compiler_fence(Acquire)` 必要性

x86 hardware: TSO により load-load reorder は起きない → fence 不要。

しかし LLVM IR レベル: R7 の `ptr::read` は non-atomic load の連続で、R9 の Acquire
load との依存関係が無いように見える。LLVM は理論上 R7 を R9 後ろに reorder できる
(Acquire load は「subsequent ops を before に reorder させない」のみで、prior ops の
movement は制約しない)。

`compiler_fence(Acquire)` を R8 に置くと、LLVM 側で「prior loads を after に動かせ
ない、subsequent loads を before に動かせない」の両方向制約となる (実際は IR の
`fence acquire` は acquire semantics で memory access reorder を禁止)。

x86 では runtime overhead ゼロ (codegen で何も出ない)。ARM/RISC-V 移植時に必要。

**c17s は fence を省略している**が、これは未文書化の暗黙的依存 (LLVM が偶然 reorder
してこなかった結果)。B では明示的に追加し、portable correctness を確保する。

### 6.4 race γ (K drop on remove)

Path B remove (§7.3):

```
remove1. writer Mutex acquire.
remove2. find existing key. version CAS even→odd で claim。
remove3. let old_entry: Entry<K, V> = ptr::read(entry_ptr);
remove4. tags[pos].store(EMPTY, Release); tags shift compact。
remove5. version.store(v+2, Release).
remove6. len.fetch_sub(1).
remove7. guard.defer_unchecked(move || {
            // SAFETY: old_entry の K, V は ManuallyDrop だが、ここで明示的に drop。
            unsafe { ManuallyDrop::drop(&mut old_entry.key); }
            unsafe { ManuallyDrop::drop(&mut old_entry.value); }
         });
```

remove4 で tags[pos] が EMPTY 化されるので、後続 reader の R5 で needle 不一致 →
Miss 返す。すれ違いで reader が R5-R6 を通過した場合は、R7 で ptr::read した buf.key
は Drop 対象の K の **bitwise copy** だが、ManuallyDrop なので drop されない。R10 の
K::Eq は K の bytes (ManuallyDrop 越し) を読むが、これは buf 内の bytes であり、
entries[id] 側の K が drop されても buf の bytes は reader stack 上に残る。

ただし K::Eq が **K の bytes を経由して heap を follow する場合** (例: K = String なら
String.ptr → heap "key text" を読む) は、heap が writer の ManuallyDrop::drop で
free されると UAF。これも race β と同型なので epoch::defer が必要。

remove7 の `defer_unchecked(drop(old_entry))` がこれを塞ぐ:

- R1 の pin が remove7 より前 → epoch 保護で drop 遅延 → K の heap alive。 ∎
- R1 が remove7 より後 → R5 で tag が EMPTY を観測 → Miss 返す (R10 まで到達せず)。 ∎

### 6.5 race δ (Path C false-miss)

c17s の path_c_epoch retry 機構をそのまま継承。Path C の `entries[id]` 上書きは
version flip で reader R6-R9 が捕える (R10 まで到達せず Racing 返す)。Path C shift
中の SIMD scan false-miss は path_c_epoch bump で R2-R4 が retry させる。
詳細は `2026-05-11-c17s-design.md` §7。

### 6.6 Send/Sync 境界

`unsafe impl<K, V, H> Send + Sync for Shard<K, V, H>` の justification:

- entries[] への non-atomic write は writer のみ (Path A の CAS or Path B/C の Mutex
  で排他化)。
- reader の non-atomic read (R7) は entry-version seqlock で snapshot consistency
  を validate (race α 防御)。
- V の heap reclamation は crossbeam-epoch で deferred (race β 防御)。
- defer_unchecked closure は `Send + 'static` 必須 → `V: Send + 'static` (および
  `K: Send + 'static`) が bound として要請される。`Sync` は reader が `&V` で V::clone
  を呼ぶため別途必要。

→ `K: Hash + Eq + Send + Sync + 'static`、`V: Clone + Send + Sync + 'static`
(現 senba::concurrent と同一 bound)。

## 7. Writer protocol

### 7.1 Path A (lock-free update)

c17s `try_path_a` の `drop(old_value)` (line 480) を `defer_unchecked(move || drop(old_v))`
に置き換える。それ以外は bit-identical。

```rust
fn try_path_a(&self, key: &K, needle: u16, value: V) -> Result<(), V> {
    const MAX_RETRY: usize = 1;
    let mut value_holder = ManuallyDrop::new(value);
    for _ in 0..MAX_RETRY {
        let found = unsafe { self.find_lockfree_for_path_a(key, needle) };
        let (pos, id, v_snap) = match found {
            Some(x) => x,
            None => {
                let v = unsafe { ManuallyDrop::take(&mut value_holder) };
                return Err(v);
            }
        };
        let entry_ptr = self.entry_mut_ptr(id);
        let version_ref = unsafe { &(*entry_ptr).version };
        match version_ref.compare_exchange(v_snap, v_snap.wrapping_add(1), Acquire, Acquire) {
            Ok(_) => {}
            Err(_) => continue,
        }
        // CAS 成功: 排他書き込み権。
        let new_value = unsafe { ManuallyDrop::take(&mut value_holder) };
        // SAFETY: version 奇数で reader bail。value field のみ touch、key 不変。
        //         ManuallyDrop wrapper の中身を読み出して所有権を writer stack に取る。
        let old_v: V = unsafe { ptr::read(&(*entry_ptr).value as *const ManuallyDrop<V> as *const V) };
        unsafe { ptr::write(
            &mut (*entry_ptr).value as *mut ManuallyDrop<V> as *mut V,
            new_value
        ); }
        version_ref.store(v_snap.wrapping_add(2), Release);
        let mask = vbit_mask(pos);
        self.hot.visited.fetch_or(mask, Relaxed);
        // ↓ ここが c17s からの唯一の差分。
        defer_drop_if_needed::<V>(old_v);
        return Ok(());
    }
    let v = unsafe { ManuallyDrop::take(&mut value_holder) };
    Err(v)
}

#[inline]
fn defer_drop_if_needed<V: Send + 'static>(v: V) {
    if std::mem::needs_drop::<V>() {
        // V: !Copy パス。
        let guard = crossbeam_epoch::pin();
        unsafe { guard.defer_unchecked(move || drop(v)); }
    } else {
        // V: Copy パス。drop は no-op、forget で済む。
        // monomorphize 後 drop も forget も全く同じ asm になるが、明示する。
        std::mem::forget(v);
    }
}
```

**重要**: `pin_for::<V>()` (reader R1) も `defer_drop_if_needed::<V>` も `needs_drop::<V>()`
const branch で monomorphize-time に dead code 除去される。V=u64 では epoch path は
完全に消え、c17s と bit-identical の asm になる (§9 で verify)。

### 7.2 Path B (warmup insert)

```rust
fn path_b_install(&self, key: K, value: V, needle: u16) -> Option<(K, V)> {
    // writer Mutex 配下。
    let id = self.alloc_id();          // free_ids pop or next_fresh_id bump。
    let pos = self.state.len;
    let entry_ptr = self.entry_mut_ptr(id);
    // SAFETY: entry[id] が new slot (uninit 状態) または free_ids 経由で取った
    //         previously-retired slot。後者は前 retire 時の defer drop が完了
    //         している保証はないが、writer は version=0 (偶数) で fresh state を
    //         publish するので、reader が観測する value bytes は新 install 後のもの。
    //
    //         「前回の defer drop がまだ走っていない slot を再利用していいのか」:
    //         entry の bytes は ptr::write で完全上書きされるため、前 K, V の
    //         instance はもう entries[id] からは参照されない。defer closure は
    //         自身が持つ old K, V の独立 instance を drop するだけで、entries[id]
    //         には触れない。∴ 安全。
    unsafe {
        ptr::write(entry_ptr, Entry {
            version: AtomicU32::new(0),
            _pad: 0,
            key: ManuallyDrop::new(key),
            value: ManuallyDrop::new(value),
        });
    }
    let tag = (LIVE | encode_id(id) | needle) as u16;
    self.tags[pos].store(tag, Release);   // ← reader publication.
    self.state.len += 1;
    None
}
```

### 7.3 Path C (evict + shift) and Path B remove

```rust
fn path_c_evict_and_install(&self, key: K, value: V, needle: u16) -> Option<(K, V)> {
    // writer Mutex 配下。
    let victim_pos = self.find_victim();
    let victim_id = id_of(self.tags[victim_pos].load(Acquire));
    let victim_entry_ptr = self.entry_mut_ptr(victim_id);

    // C1. Path C shift compact 前に victim entry の K, V を ManuallyDrop::take で
    //     writer stack に取り出す。
    let old_key: K = unsafe { ManuallyDrop::take(&mut (*victim_entry_ptr).key) };
    let old_value: V = unsafe { ManuallyDrop::take(&mut (*victim_entry_ptr).value) };

    // C2. entry version を bump (奇数 transient → reader が R6 で bail)。
    let v_old = unsafe { (*victim_entry_ptr).version.load(Acquire) };
    unsafe { (*victim_entry_ptr).version.store(v_old.wrapping_add(1), Release); }

    // C3. new K, V を install。
    unsafe {
        (*victim_entry_ptr).key = ManuallyDrop::new(key);
        (*victim_entry_ptr).value = ManuallyDrop::new(value);
        (*victim_entry_ptr).version.store(v_old.wrapping_add(2), Release);
    }

    // C4. tags shift compact (c17s と同じ手順)。各 store は Release。
    //     tags shift 完了後 path_c_epoch を bump。
    // ... (c17s 既存 logic) ...
    self.hot.path_c_epoch.fetch_add(1, Release);

    // C5. 旧 K, V を epoch defer で reclaim。
    defer_drop_kv_if_needed::<K, V>(old_key, old_value);
    None
}

#[inline]
fn defer_drop_kv_if_needed<K, V>(k: K, v: V)
where K: Send + 'static, V: Send + 'static
{
    if std::mem::needs_drop::<K>() || std::mem::needs_drop::<V>() {
        let guard = crossbeam_epoch::pin();
        unsafe { guard.defer_unchecked(move || {
            drop(k);
            drop(v);
        }); }
    } else {
        std::mem::forget(k);
        std::mem::forget(v);
    }
}
```

remove も同様: ManuallyDrop::take で K, V を抜き取り、tags shift + version flip 後に
`defer_drop_kv_if_needed` で reclaim。

### 7.4 Writer atomic shape

| step | c17s | senba::concurrent | B (c19s) |
|------|------|---------------------|----------|
| version CAS even→odd | RMW Acquire | RMW Acquire | RMW Acquire |
| ptr::read old | non-atomic | non-atomic of Arc<V> ptr | non-atomic of V |
| ptr::write new | non-atomic | non-atomic of Arc<V> | non-atomic of V |
| version.store even+2 | store Release | store Release | store Release |
| visited.fetch_or | RMW Relaxed | RMW Relaxed | RMW Relaxed |
| reclaim | sync `drop(old_v)` | `pin` + `defer_unchecked` | `pin` + `defer_unchecked` (V:!Copy) or `forget` (V:Copy) |

**Path A の追加コスト** (vs c17s): epoch::pin + defer_unchecked = ~10ns/Path A。
Path A は steady-state で update 頻度に比例なので read-heavy workload では影響軽微。

## 8. Copy 特殊化 (monomorphize-time fold)

### 8.1 fold mechanism

```rust
const fn needs_epoch<V>() -> bool {
    std::mem::needs_drop::<V>()
}

#[inline(always)]
fn pin_for<V>() -> EpochGuardWrapper {
    if needs_epoch::<V>() {
        EpochGuardWrapper::Some(crossbeam_epoch::pin())
    } else {
        EpochGuardWrapper::None
    }
}

enum EpochGuardWrapper {
    Some(crossbeam_epoch::Guard),
    None,
}
```

`needs_drop::<V>()` は `const fn` (Rust 1.36+) で concrete V に対しては monomorphize
時に評価可能。LLVM の const-folding pass で `if needs_epoch::<V>()` の分岐が dead
code として除去される。

**Verification**: `cargo asm` または `cargo rustc -- --emit=asm` で V=u64 の
`get_by_hash` を生成し、`crossbeam_epoch::pin` の呼び出しが残っていないことを確認する。

### 8.2 V: Copy の挙動

V: u64, [u8; 24], (u64, u64) 等の Copy 型:

- `needs_drop::<V>()` → `false` (Copy ⇒ !Drop が Rust の規則)
- `pin_for::<V>()` → `EpochGuardWrapper::None` (const-folded で if 全体が空)
- `defer_drop_if_needed::<V>()` → `mem::forget(v)` のみ (Copy なので no-op)

結果: epoch path は完全に dead code 除去され、reader/writer 共に c17s と bit-identical の
codegen になる (期待)。

### 8.3 V: !Copy の挙動

V: String, Vec<u8>, Box<T> 等:

- `needs_drop::<V>()` → `true`
- `pin_for::<V>()` → `EpochGuardWrapper::Some(pin)`
- `defer_drop_if_needed::<V>()` → `pin + defer_unchecked` path

reader/writer は epoch overhead を払う。

### 8.4 注意: `needs_drop` の false negative はない

`needs_drop::<V>()` は **conservative** で「drop が必要かもしれない」型では true を返す。
false (= drop 不要) を返すのは:

- Copy 型 (Copy bound は !Drop を含意)
- Drop impl がなく、かつ全 field も !needs_drop

これは false negative (= 本当は drop 必要なのに false を返す) を起こさない。よって
B の Copy 特殊化が「実は drop 必要な V を mem::forget して leak する」事故は構造的に
起きない。

## 9. Performance model

### 9.1 reader hot path ns/op 見積もり

各 atomic op の cost (Alder Lake P-core, L1 cached, 推定):

| op | cost (ns) |
|----|-----------|
| Acquire load (L1 hit) | 1 |
| Acquire load (L3 hit) | 10-15 |
| Relaxed load | 1 |
| RMW Relaxed (no contention) | 3-5 |
| RMW Relaxed (cross-core, hot line) | 30-100 (MESI bouncing) |
| compiler_fence | 0 (codegen 上 nop) |
| crossbeam-epoch pin | 5-7 (内部で SC atomic ops + TLS register) |

### 9.2 V=u64 (Copy), hot-key, T=16

| variant | atomic write count | epoch | ns/op (予想) |
|---------|---------------------|-------|--------------|
| c17s | 0 | なし | 7.5 (実測, partitioned-results) |
| 現 senba::concurrent | 2 (Arc strong inc + dec) | あり | 14 (実測, 退行 −48% から逆算) |
| **B (c19s)** | **0** | **dead-code 除去** | **7.5 (c17s 同等を期待)** |

### 9.3 V=String (!Copy), hot-key, T=16

| variant | atomic write count | epoch | ns/op (予想) |
|---------|---------------------|-------|--------------|
| c17s | 0 (ただし UB) | なし | 9-10 (String clone cost 含む) |
| 現 senba::concurrent | 2 | あり | 17-18 (Arc 退行 + epoch) |
| **B (c19s)** | **0** | **あり** | **12-14** |

B は c17s より遅い (epoch::pin ~5ns) が、現 lib からは +30〜+50% を見込む。Arc 退行
構造的に消える分を回収する。

### 9.4 V=u64, T=1 (epoch overhead 検証)

Copy 特殊化が効くなら c17s と完全同等。efectively monomorphize verify が perf-gate の
新規 cell として要る (§11)。

### 9.5 メモリ overhead

- Entry sizeof: c17s と同じ (Arc 抜きで V 直置きなので +sizeof(Arc<V>) - sizeof(V) ≈ 0
  for Copy, slightly negative for big V)。
- crossbeam-epoch local bag: per-thread ~1KB (deferred closure pool)。
- defer closure size: `move || drop(old_v)` は V の bytes を closure 内に格納するため
  closure size = sizeof(V) + tag bytes。V=String なら closure ~32B。
- Reclamation backlog: pin holding 中の全 deferred closures がメモリに残る。worst
  case = write rate × max pin hold time × sizeof(V)。実測 bounded で確認する。

## 10. 実装の難所

### 10.1 ManuallyDrop trap

c17s では reader の buf が `ManuallyDrop<Entry<K, V>>` だが、Entry 内部の `key`, `value`
は **K, V 生** (not ManuallyDrop) だった。これは c17s が K, V を所有 transfer
していたため。B では Entry 自身が `ManuallyDrop<K>, ManuallyDrop<V>` を内包する
必要がある (writer Path B/C remove で K, V を ManuallyDrop::take で抜き取って
defer closure に move するため)。

差分:

```diff
-struct Entry<K, V> { version, key: K,                value: V }
+struct Entry<K, V> { version, key: ManuallyDrop<K>, value: ManuallyDrop<V> }
```

reader 側 buf 操作は変わらないが、`buf.key` の型が `ManuallyDrop<K>` になる ので
K::Eq 呼び出しは `(*buf.key).borrow() != key` のように deref が一段増える (zero-cost)。

**罠**: Shard::drop (Cache が drop される時) で entries[] の全 K, V を明示的に
ManuallyDrop::drop で drop する必要がある。c17s は K, V が生フィールドなので
default drop で自動的に走ったが、B では Drop impl が writer 責務になる:

```rust
impl<K, V, H> Drop for Shard<K, V, H> {
    fn drop(&mut self) {
        // SAFETY: &mut self で排他、reader なし。
        let len = self.state.len;  // or whatever the in-use marker is
        for id in self.live_ids() {
            let entry_ptr = self.entry_mut_ptr(id);
            unsafe {
                ManuallyDrop::drop(&mut (*entry_ptr).key);
                ManuallyDrop::drop(&mut (*entry_ptr).value);
            }
        }
    }
}
```

live_ids の集合をどう取るか (free_ids 経由か state.len 経由か) は実装詳細。

### 10.2 compiler_fence の portability 確認

§6.3 で議論。x86 では codegen 上 no-op だが、ARM/RISC-V 向けには runtime barrier
が必要かもしれない (`fence(Acquire)` 相当)。senba は AVX2 gate で x86_64 限定なので
optimization としては compiler_fence で十分だが、`#[cfg]` で arch 別の fence を
切り替える設計余地がある。

**判断**: AVX2 gate のまま compiler_fence 一本で進める。non-x86 サポートは別 PR。

### 10.3 panic safety の全 path 検証

V::clone / K::Eq / K::Hash / V::Drop / K::Drop はすべて user 提供。panic 可能性あり。

- **R10 K::Eq panic**: buf は ManuallyDrop なので drop 走らない。pin guard が
  unwind で drop されて epoch unpin。writer の deferred drops が再開可能に。
  reader 側で resource leak も double-free もなし。panic propagate は caller へ。
- **R11 V::clone panic**: 同上。buf は ManuallyDrop。pin guard unwind drop。
  cloned V (途中まで作られたかもしれないが) は clone 内部で drop されている
  (Rust の Clone trait の panic 規約に従う)。Resource leak は V::clone の
  実装次第。
- **Writer Path A の `ptr::write(new_v)` panic**: ptr::write は non-panic だが、
  もし V の move ctor が呼ばれる暗黙経路で panic すれば... 実際は non-panic
  (mem::write は bitwise copy)。version は v_snap+1 (奇数) 状態で stuck。
  これは設計上 panic しない経路として正当化する (`ptr::write` は infallible)。
- **defer closure 内の V::Drop panic**: crossbeam-epoch の reclaim phase で発生。
  std::process::abort になる可能性 (double panic)。これは V の Drop の責任で
  正当化、lib 側で塞ぐ手は限定的。doc に明記する。

### 10.4 crossbeam-epoch + Path C 合成の loom verify 困難

crossbeam-epoch は **loom 非対応**。Loom の atomic primitives で書き直されていない
ため、crossbeam-epoch を呼ぶコードを loom test に組み込めない。

**回避策**:

- (A) epoch path 抜きの「seqlock 単独」を loom で verify (= c17s の loom test 拡張)。
  B が race α / δ を c17s と同じ機構で扱う限り、loom 結果は流用可能。
- (B) epoch path は **miri** で verify (miri は crossbeam-epoch 動かす)。
- (C) ASan/TSan + stress test (`bench_concurrent --value string --threads 16 --hot-key`)
  を CI に組み込んで race β を実機で検出。
- (D) crossbeam-epoch 自身は upstream で loom-test されている (`crossbeam-utils` の
  CachePadded など)。B はそれを composer として使うだけなので crossbeam の保証に
  乗る。

### 10.5 reclamation backlog の bound

writer-heavy workload (gim, write-mix=0.5) で長寿命 reader が並ぶと defer queue が
膨らむ。worst case の見積もり:

- write rate 100 Mops/s, sizeof(closure) ≈ sizeof(V) + 16B
- V=String (avg 32B) なら 48B/closure × 100M/s = 4.8 GB/s の生成 rate
- crossbeam-epoch の local bag は ~64 closure (`MAX_OBJECTS`); 超えたら global queue に
  flush。global queue は連結リストで bound なし。
- pin hold time が長いと global queue が爆発。

**実測対応**: stress test で `RSS` を 60s 計測し、定常状態に達するか暴走するかを判定。
暴走するなら mitigation (writer 側で `defer_unchecked` の前に `try_advance` を呼ぶ等)
を追加する。

### 10.6 `mem::needs_drop` の monomorphize fold が効かない可能性

LLVM が `const fn` 評価を const-prop で確実に折りたたむかは optimization level 依存。
release build (`opt-level = 3`) では効くはずだが、`opt-level = 1` の dev では
runtime 分岐が残る可能性。

**Verification**:

```bash
cargo rustc -p senba --release --bin <smoke> -- --emit=asm
# get_by_hash<u64, u64> の disasm で crossbeam_epoch::pin が消えていることを確認
```

dev build で残ること自体は acceptable (perf 計測は release のみ)。

### 10.7 同一 closure の `Send + 'static` 制約と V のライフタイム

`defer_unchecked(move || drop(old_v))` の closure は `Send + 'static`。`old_v: V` で
V: 'static + Send なら closure も自動的に 'static + Send。bound check 失敗時の
コンパイルエラーが walltime に出やすいよう、`Cache<K, V>` の bound に明示する。

### 10.8 multi-shard と global epoch の干渉

各 Shard が独立に epoch::pin/defer する。crossbeam-epoch の global epoch は
collector 単位 (default は global) で 1 つ。全 Shard が同じ global epoch に乗る。

これにより「Shard A の reader が pin 中、Shard B の writer の defer が遅延する」
cross-shard 干渉が起きる。**性能影響は限定的** (reader の pin time は短いので
collector が advance しないまま長期間 stuck することは稀) だが、極端な writer-heavy
+ reader 長寿命 pattern で issue になる可能性。

**緩和**: 各 Shard 独立の collector (`crossbeam_epoch::Collector::new()` per shard) を
持たせる。trade-off は collector 数だけ TLS 領域が増えること。判断は実測後。

## 11. テスト戦略

### 11.1 Correctness

| level | tool | scope |
|-------|------|-------|
| unit | `cargo test -p senba` | 既存 senba::concurrent test 18 本を全 pass |
| oracle | `tests/oracle_cache_match.rs` | sieve_orig との eviction sequence 完全一致 |
| miri | `cargo +nightly miri test -p senba concurrent_basic` | UB 検出 (V=u64 と V=String) |
| stress | `bench_concurrent --threads 16 --value string --duration 60s` + ASan/TSan | race β/γ の実機検出 |
| loom (seqlock only) | `loom_test_entry_seqlock` | c17s から流用、race α/δ の verify |

### 11.2 Performance

- 既存 `research/benches/sieve_cache_perf.rs` (perf-gate) はそのまま (V=u64 で c17s
  と等しいことを確認する以上の役割は無い)。
- 新規: `bench_concurrent --variants c17s,senba_concurrent,c19s --value {u64,string}
  --hot-key` の 3-way sweep を `docs/benchmark/c19s-vs-c17s-vs-lib/` に保存。
- `senba-concurrent-vs-c17s` の 144 cell sweep を c19s で再走し、退行が解けるか
  確認。**accept 基準**: median ≥ −5% vs c17s、worst ≥ −15% vs c17s (Copy 特殊化が
  効いていれば V=u64 で 0%)。

### 11.3 Codegen verification

- `cargo asm senba::concurrent::Cache::get -V=u64,u64` で `crossbeam_epoch` 呼び出し
  symbol が含まれないことを assert する CI check を `scripts/` 配下に置く。

## 12. 実装 phasing

### Phase 1: research variant `sieve_c19s` (3-5 commit)

`research/src/experimental/sieve_c19s.rs` に c17s からの diff として実装:

1. Entry に ManuallyDrop wrap を追加。
2. reader R8 に compiler_fence 追加。
3. writer Path A/B/C の drop を `defer_drop_if_needed` に置換。
4. `mem::needs_drop` の Copy 特殊化分岐を実装。
5. `research/src/experimental/mod.rs` に register。
6. `research/tests/oracle.rs` に `c19s_matches_orig_on_bundled_zipf` を追加。

Phase 1 完了基準: oracle test pass、`bench_concurrent --variant c19s --value u64 hot-key`
が c17s 同等 ±10%。

### Phase 2: stress + miri + ASan

- `cargo +nightly miri test -p senba-research --features external-traces` (重い、
  数時間)。
- TSan build: `RUSTFLAGS="-Zsanitizer=thread" cargo +nightly build --bench bench_concurrent`、
  V=String hot-key で 60s 実行。

Phase 2 完了基準: TSan で race report ゼロ、miri stderr で UB ゼロ。

### Phase 3: perf sweep + design verification

- `bench_concurrent --variants c17s,senba_concurrent,c19s --value u64,string` の
  144 cell sweep を 3-way で実施 (現 senba-concurrent-vs-c17s の continuation)。
- VTune memory-access で reader L1 fetch 数の c17s 同等性を確認 (`docs/benchmark/c19s-vtune/`)。

Phase 3 完了基準:

- V=u64: median ≥ −5% vs c17s、worst ≥ −10% vs c17s。
- V=String: median ≥ +30% vs senba::concurrent、worst ≥ +20% vs senba::concurrent。

### Phase 4: lib integration

- `senba::concurrent::Cache` の Shard 実装を c19s に置換 (Arc/epoch::defer の
  refcount path を削除、c19s の直 V + epoch::defer path を install)。
- API は変更なし (G1 維持)。バージョン bump は patch (0.3.x → 0.3.y) で良い (semver
  互換)。
- `docs/reports/2026-05-13-senba-concurrent-cache-design.md` の続報として
  `2026-05-XX-c19s-lib-integration.md` を書く。

Phase 4 完了基準: workspace test pass、perf-gate pass、c19s sweep の数値が Phase 3 と
一致 (lib 化で構造変化なし)。

## 13. Alternative designs considered

- **hazard pointer**: epoch::pin overhead (~5ns) も消す redesign。dep が増え、writer drop
  scan が O(thread count)、設計面積大。B 採用後の next-step として keep。
- **`V: Copy` 限定 lib (Approach A)**: 公開 API を `V: Copy` に縮減。実装容易だが
  String/Vec 等の典型用途を捨てる。並列に検討する余地はあるが「lib の主要 use case を
  逃す」リスクで本仕様より下位。
- **RwLock<ShardInner> (r3)**: reader counter atomic write で −80% 退行、reject 済み
  (`2026-05-14-r3-vs-c17s.md`)。

## 14. Open questions

- **OQ1**: `mem::needs_drop` const fold が opt-level=3 で全 build で効くか。`cargo asm`
  で先に prototype 検証する。
- **OQ2**: crossbeam-epoch の per-shard collector が pin 時間を短縮するか。Phase 3
  で計測。
- **OQ3**: ASan/TSan が x86_64 Linux で stable に動くか (rust nightly toolchain 必要、
  WSL2 で動くか未確認)。
- **OQ4**: `defer_unchecked` の closure に格納される V のサイズが大きい場合
  (`V = Vec<u8, 1KB>` 等)、closure heap allocation cost が write path で支配的になる
  可能性。Phase 1 で `--value bytes1024` cell を追加検証。
- **OQ5**: variant 命名 (`sieve_c19s` で c-series 継続するか、`sieve_e1` で新 series
  起こすか)。c17s の構造を継承して soundness gap を埋めるだけなので c19s で良いと
  考えるが、user judgement で。
