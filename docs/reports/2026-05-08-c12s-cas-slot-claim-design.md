# 2026-05-08 c12s — CAS-based slot claim 並行 SIEVE variant 設計

- 親系列: c8 (lock-free reader / Mutex writer) → c10s (visited 列分離) → c11s (conditional set) → **c12s (writer Mutex 完全排除)**
- 親報告: `docs/reports/2026-05-08-c11s-conditional-set.md` §5 §9 — c11s で reader 経路は飽和、残課題は writer Mutex critical section の sequential bottleneck
- structural base: **senba::Cache** (publishable lib) の SlotSize / AlignedTags / c-hoist trick / AVX2 dispatch を継承
- 期待実装: `research/src/experimental/sieve_c12s.rs` (research crate 内)、後の promotion で `senba::concurrent::Cache` 化候補

## TL;DR

c12s は **writer Mutex を完全排除** した並行 SIEVE variant。c11s 報告 §5 のスローガン「CAS-based slot claim」をアルゴリズムに具体化したもの:

1. `hand` を `AtomicUsize` 化 + per-tag CAS で **evict を lock-free** に
2. **install-at-evicted-pos** (= 同 pos に新 entry を install) を採用し、tail / compaction を構造的に廃止 → Mutex を 1 個も持たない state struct になる
3. structural skeleton は senba::Cache から継承 (SlotSize / AlignedTags / c-hoist / Xxh3Build / AVX2)、c8 lineage 由来の独自 tag layout を捨てる
4. 並行 SIEVE state machine 部分のみが research-side 実装で、promotion 時には `senba::concurrent::Cache` への structural 置換で済む

c11s 報告で確認された read-heavy zipf 16T の writer 律速 (c8/c10s/c11s が ~17–21 Mops に収束) を打破するのが第一目的。perf 見込みが立った時点で初めて correctness machinery (長時間 stress / Loom 等) に投資する段階構成 (§5 参照)。

## §1 構造 — senba::Cache 継承 / 置換 / 廃止

| 要素 | senba::Cache | c12s | 由来 |
|---|---|---|---|
| `SlotSize` (Slot16/32/64 stride) | ✓ | **継承** | senba |
| `AlignedTags` (32-byte align) | ✓ | **継承** | senba |
| c-hoist trick (`tag & ID_MASK = id × S::SIZE`) | ✓ | **継承** | senba |
| `Xxh3Build` / shard 選択 / Cache wrapper API | ✓ | **継承** | senba |
| AVX2 + BMI1 dispatch | ✓ | **継承** | senba |
| Tag layout | `LIVE\|VISITED\|id\|hash(8)` | `LIVE\|id\|hash(9)` | c11s 流に VISITED bit 削除、hash 1 bit 拡張 |
| VISITED 格納 | tag 内 | **別 `[AtomicU64]` array** | c11s |
| `tags` 型 | `[u16]` (plain) | `[AtomicU16]` | c10s/c11s |
| `entries` 型 | `Vec<MaybeUninit<...>>` | `UnsafeCell<Box<[MaybeUninit<Entry>]>>` | c8 |
| eviction 戦略 | shift-on-evict (I4') | **install-at-evicted-pos** | 新規 (並行性のため) |
| `len` の役割 | `0..cap` で動く高水位 | warmup 専用 (`L < cap` のみ動く)、steady では cap 固定 | 新規 |
| `hand` | `usize` | **`AtomicUsize`** | 新規 |
| compaction | 不要 (I4' で自然) | **不要** (install-at-evicted-pos で自然) | 新規 |
| writer 排他 | `&mut self` | **lock-free, Mutex 一切無し** | 新規 |
| K, V 制約 | Eq + Hash, V free | **K: Copy, V: Copy** | c8/c10s/c11s 継承 (seqlock 制約) |

### なぜ shift-on-evict を捨てるか

senba::Cache は I4' (`tags[0..len]` 全 LIVE) を shift-on-evict で維持していて、これにより compaction 不要・SIMD 走査窓が狭い・eviction 順が sieve_orig と byte-for-byte 一致、等の利益を得ている。しかし shift-on-evict は **single-writer 前提**で、以下の理由で並行化と非両立:

- shift 中の `tags[pos+1..len]` は同一 entry が複数 pos を巡回する。reader の seqlock-via-tag が `t1 == t2` を pass しても、entry の指す key が動いていれば不正データを返しうる。
- 複数 writer が同時に shift すると、shift の重なり方によっては非可逆の不整合 (id が複数 tag で参照される、entry が二重 drop) を起こす。

代替策が **install-at-evicted-pos**: evict した同 pos に新 entry を install することで、tags 配列は構造的に shift しない。各 (evict, install) 対が distinct pos で並行進行できる。

### tag layout 詳細

- `LIVE = 0x8000` (bit 15)
- `ID_MASK = ((MAX_PER_SHARD - 1) as u16) << ID_SHIFT`、`ID_SHIFT = log2(S::SIZE)`
- `HASH_MASK = 0x7FFF & !ID_MASK` (= 9 bit、senba::Cache の 8 bit より +1)
- `SCAN_MASK = LIVE | HASH_MASK`
- VISITED は **`[AtomicU64]` 別配列**、`pos` の bit は `(pos >> 6, 1u64 << (pos & 63))` で定位

c-hoist (`tag & ID_MASK == id × S::SIZE`) は senba::Cache とビット互換、AVX2 scan 内で entries pointer 計算を hoist する trick が同じ shape で使える。ただし c12s では **id == pos が常に成立** (§3 I-C1) するので、id field は構造的に冗長 (CAS 操作 / entry pointer 算出に使うが、論理的には pos と一致する)。AVX2 path との互換性を優先して残す。

## §2 writer state machine — `insert(k, v, hash)`

```text
insert(k, v, hash):
  needle = needle_from_hash(hash)
  loop:                                     // outer retry (race 時のみ回る)

    // ---- Path A: 既存キー update (in-place) ----
    if let Some(pos) = find_lockfree(k, needle):
      t = tags[pos].load(Acquire)
      if (t & SCAN_MASK) != needle: continue   // stale (動かされた)、retry

      // CAS で tag を invalidate して所有権を取る
      if tags[pos].compare_exchange(t, EMPTY, Release, Acquire).is_err():
        continue                               // 別 writer に取られた、retry

      // pos を所有: id == pos なので entries[pos] を上書き
      entries[pos].write(Entry{ k, v })
      fence(Release)
      tags[pos].store(LIVE | (pos << ID_SHIFT) | (needle & HASH_MASK), Release)
      // SIEVE oracle 通り update は visited を SET する (sieve_orig の freq=1 一致)
      let (w, b) = vbit(pos)
      visited[w].fetch_or(b, Relaxed)
      return None

    // ---- Path B: warmup install (len < cap) ----
    L = len.load(Acquire)
    while L < cap:
      if len.compare_exchange(L, L+1, AcqRel, Acquire).is_ok():
        // entry_id = pos = L を排他取得
        entries[L].write(Entry{ k, v })
        fence(Release)
        tags[L].store(LIVE | (L << ID_SHIFT) | (needle & HASH_MASK), Release)
        // visited は 0 のまま (新規 entry は visited を立てない)
        return None
      L = len.load(Acquire)                    // CAS 失敗 → 再 load

    // ---- Path C: 定常 evict + install ----
    let (pos, evicted_kv) = evict_one()        // pos の所有権を取得して帰る
    entries[pos].write(Entry{ k, v })
    fence(Release)
    tags[pos].store(LIVE | (pos << ID_SHIFT) | (needle & HASH_MASK), Release)
    return Some(evicted_kv)


evict_one() -> (pos, (K, V)):
  loop:
    pos = hand.fetch_add(1) % cap              // hand は [0, cap) を周回
    t = tags[pos].load(Acquire)
    if (t & LIVE) == 0:
      continue                                  // 別 writer の install 進行中 / EMPTY pad
    let (w, b) = vbit(pos)
    if visited[w].load(Relaxed) & b != 0:
      visited[w].fetch_and(!b, Relaxed)        // SIEVE: visited は剥がして次へ
      continue

    // !visited && LIVE: evict 候補。CAS で確定
    if tags[pos].compare_exchange(t, EMPTY, Release, Acquire).is_err():
      continue                                  // 別 writer に取られた

    // pos と entry_id (= pos) を所有。visited を CLEAR (entry が消えるので)。
    visited[w].fetch_and(!b, Relaxed)
    fence(Release)
    let evicted = entries[pos].assume_init_read()   // K, V: Copy で torn 非伝播
    return (pos, (evicted.key, evicted.value))


find_lockfree(k, needle) -> Option<pos>:        // c11s reader と完全同形
  // tags[..tags.len()] を AVX2 scan、(tag & SCAN_MASK) == needle で seqlock dance、
  // entry.key == k で hit。ただし visited fetch_or は撃たない (writer は SET しない)。
```

### §2 設計上の急所

1. **Path A の race 解消は 2 段 CAS**: `find_lockfree` で見つけた tag が、CAS 到達時には変わっている可能性あり (= 他 writer が evict、別 update が済んだ)。CAS 失敗 → outer loop で retry。retry ごとに find からやり直すので無限 loop にはならない (key が消えた → Path B/C 落ち、別 update が済んだ → 上書き先で last-writer-wins 意味論で勝つまで戦う)。

2. **Path A の `entries[pos]` 上書きは reader と race するが seqlock で吸収**: reader が `t1 = old_tag` を読んで `entries[pos]` を読む途中で writer が CAS LIVE→EMPTY して entries 上書きを始めると、reader の `t2` 検査で `EMPTY` か `LIVE_new (hash 違いの確率高)` が見えるので t1≠t2 で fail。c11s と同 soundness gap (key の hash 9 bit + tag id field がたまたま完全一致した場合のみ問題、確率的負担は negligible)。

3. **Path C の `entries[pos].assume_init_read()` 順序**: CAS 成功 (= tag を EMPTY に publish) **後** に entries を読む。reader の seqlock は CAS 成功時点で必ず fail するので、reader は old entry を返さない。writer は old entry の K, V を Copy で読み出してから新 entry を書くので、evicted_kv は正しく返せる。

4. **`entry_id == pos` 不変** (I-C1): senba::Cache の I8 (live ids = 0..len) を c12s では強化版「id == pos が常に成立」に置き換える。warmup は `pos = L = entry_id`、Path A は同 pos で reuse、Path C は evict pos に install。tag の id field は c-hoist arithmetic との互換のために残すが、論理的には pos と一致 (= 冗長)。

5. **hand 範囲 = [0, cap)**: `tags.len() = round_up(cap, LANE).max(LANE) ≥ cap` で、`tags[cap..tags.len()]` は永久 EMPTY pad。hand を `[0, cap)` に絞ることで pad を踏まず効率的。reader scan は senba と同じく `tags[..tags.len()]` 全帯走るが LIVE check で pad は捨てる。

6. **Mutex は本当に零個**: compaction は install-at-evicted-pos で発生しない (steady state は `tags[0..cap]` 全 LIVE)、shift も無い、free-id pool も無い (id == pos)。`writer.lock()` 相当の primitive は state struct から完全削除。

## §3 不変条件 (I-C-prefix)

senba::Cache の I4'–I8 と並ぶ形で c12s 固有の不変条件を立てる。strong = いかなる interleaving の最中でも成立 / weak = publish boundary を越えた時点で成立 (一時的に破れる)。

| ID | 不変条件 | 強度 | 守り方 |
|---|---|---|---|
| I-C1 | LIVE な `tags[pos]` について `id_of(tag) == pos` | strong | warmup install で id field = L、Path A/C も同 pos に install |
| I-C2 | `tags[cap..tags.len()]` は常に EMPTY (LANE-aligned pad) | strong | 構造的: pad は誰も書かない |
| I-C3 | `len` は monotonic non-decreasing、cap で停留 | strong | warmup CAS のみが `len` を増やす、`L < cap` の guard、remove 未実装 |
| I-C4 | `hand.load() % cap ∈ [0, cap)` | strong | fetch_add の値は finite、% cap で正規化 |
| I-C5 | LIVE な `tags[pos]` について `entries[pos]` は init 済み | strong | install path で `entries[pos].write` → fence(Release) → `tags[pos].store(LIVE)` の順 |
| I-C6 | EMPTY な `tags[pos]` の `entries[pos]` は logically dead | weak | Path A/C で「tag を EMPTY に CAS → entries[pos] を上書き」の窓では新旧混在の bit pattern。reader は seqlock で弾くので外部観測不能 |
| I-C7 | `visited` の clear は writer のみ (Path A の SET は writer + reader、clear は writer のみ) | strong | reader は `fetch_or` のみ、writer は `fetch_and(!b)` を撃てる |
| I-C8 | 任意の時点で同 entries[id] を install/read している writer は高々 1 個 | strong | 「pos の所有権」 = 直前の CAS LIVE→EMPTY (or len CAS 成功) に勝った writer のみが `entries[pos]` に書ける |
| I-C9 | reader の seqlock dance を pass した entry は、key 一致なら正しい (torn read を返さない) | strong | c11s と同 soundness 性質 (K, V: Copy + tag bracket)、ABA は SCAN_MASK 衝突確率に縮退 |
| I-C10 | 任意時点で同一 key K を持つ LIVE tag は **高々 1 個** | weak | 後述 "design gap: concurrent same-key update" 参照。基本は Path A の CAS で linearize されるが、loser retry 中に transient EMPTY 窓を見ると Path B/C 経由で duplicate を作りうる。SIEVE 1 sweep 以内に解消、外部 read は古い/新しい value のどちらかを返す (両方とも valid linearization) |

### Design gap: concurrent same-key update

I-C10 の弱化は本設計の **既知の race**: writer-1 が key K の Path A 中に CAS LIVE→EMPTY を成功させた直後 (entries 書き込み前)、writer-2 も同 K を update しようと find_lockfree を回すと、tag が transient EMPTY なので **K は cache 内に居ない** と判断し、Path B/C で K を新規 install する。結果、writer-1 は元 pos に LIVE_new(K, V1) を再公開、writer-2 は別 pos に LIVE_new(K, V2) を install。同一 K を持つ LIVE tag が 2 個になる。

**回復経路**: SIEVE hand が次に sweep する際、!visited の方 (= 古い方) が evict され、duplicate は 1 sweep 以内に自然解消。外部 read は race 窓中 V1 / V2 どちらかを返す (last-writer-wins ではないが両方 valid な linearization)。Cache integrity (tag state machine、I-C1〜C8、I-C9) は保たれる。

**研究 variant としての扱い**: 実 workload では同一 key の concurrent update は稀 (zipf hot key は read heavy、write は別 key が多い)、duplicate は一時的 + 自己解消なので c12s では **既知 gap として明記し、対策は別 variant に委譲** する。production-quality な解決には RCU / hazard pointer / hand-over-hand locking 等が必要だが、c12s スコープ外。

**I-C8** が c12s の根幹で、これが守られている限り Mutex 無しで entries の writer-writer race は発生しない。所有権の伝播ポイント:

- warmup: `len.compare_exchange(L, L+1)` 成功 → `entries[L]` の書き込み権
- Path A (update): `tags[pos].compare_exchange(t, EMPTY)` 成功 → `entries[pos]` の書き込み権
- Path C (evict+install): 同上 (evict_one の CAS 成功)

すべて single-CAS で解決し、所有権の race は発生しないが lost-CAS の retry はあり得る、という形。

### SIEVE 外部等価性

single-thread での c12s の挙動を sieve_orig と並べると、**eviction 順序 (どの key が次に追い出されるか)** は一致する見込み。internal layout は違う (sieve_orig は linked list、c12s は install-at-evicted-pos で位置を保つ) が、cache 中身 = key set は各 op 後で一致する。よって `matches_sieve_orig_externally_1shard` 型の oracle test は pass する想定。これは仮説で、§4.2 trace 検証で実証する。

c11s/c10s/c8 と違い、senba::Cache の I4' (= tags[0..len] 全 LIVE) を **c12s も steady state で持つ**。違いは「I4' を shift-on-evict で維持する (senba::Cache)」 vs 「I4' を install-at-evicted-pos で維持する (c12s)」だけで、外側から見ると同じ性質。SIEVE algorithm 上、両者は同じ "linked list の hand 巡回" の異なる array 表現。

## §4 テスト戦略

### §4.1 単体テスト (sieve_c11s からの mirror)

c11s の 25 件を port (cache_initially_empty / insert_then_get / get_missing_returns_none / contains_key_reflects_insertions / insert_existing_key_updates_value / evicts_oldest_when_full_and_unvisited / visited_entry_survives_first_pass / all_visited_clears_bits_then_evicts / total_capacity_is_respected_under_churn / churn_keeps_a_full_capacity_set / capacity_below_shards_panics / non_power_of_two_shards_panics / per_shard_above_max_panics / per_shard_at_max_works / works_with_non_default_shards / distinct_keys_full_per_shard_all_hit / matches_sieve_orig_externally_1shard / matches_j8_externally_1shard / bit_layout_exclusivity_u64_u64 / warm_up_to_steady_transition / compact_preserves_id_mapping → c12s では `id_eq_pos_preserves_under_churn` に置換 / update_existing_key_sets_visited_like_oracle / reader_hit_does_not_modify_tag / concurrent_invariants_under_zipf / self_insert_self_get_visibility)。

c12s 固有で追加:

| test | 目的 |
|---|---|
| `entry_id_equals_pos_invariant` | 全 LIVE tag について `id_of(t) == pos` を直接検査 (I-C1) |
| `install_at_evicted_pos_no_compaction` | 1000 evict 後も `tags[0..cap]` 全 LIVE (I-C2/C3) |
| `len_monotonic_under_concurrent_inserts` | warmup 期に複数 thread から insert、最終 len ≤ cap (I-C3) |
| `hand_atomic_advance_non_overlapping` | 2 thread から `evict_one` を回し、各々が distinct pos を返す (I-C8) |
| `bit_layout_no_visited_in_tag` | tag に VISITED bit が無い、HASH_MASK が 9 bit (= 0x7FFF & !ID_MASK) |
| `same_key_concurrent_update_self_heals` | 2 thread が同 key を update する小ケース (ops 数小、cap 小) で、最終的に LIVE tag 数 = 1 個に収束する (= I-C10 の自己解消性) |

### §4.2 オラクル等価 (matches_sieve_orig_externally_1shard)

c11s と同 trace (10000 ops、key = `(k * 2654435761) % 256`、cap=64) で:

```rust
for k in 0..256 {
    assert_eq!(orig.get(&k).copied(), c12s.get(&k));
}
```

§3 の SIEVE 外部等価性仮説の **直接検証**。仮説が外れたら eviction policy の前提理解を疑う (= install-at-evicted-pos が SIEVE 等価でない)、強い design-level の信号になる。

### §4.3 並行不変条件テスト (`concurrent_invariants_under_zipf` 拡張)

c11s の 4-thread × 50000 ops Zipf 後:

- I-C1: 全 shard の全 LIVE tag で `id_of(tag) == pos` を assert
- I-C3: total len ≤ cap
- I-C5/C6: live tag の指す entry の key が hash 整合 (key→hash→shard が正しい)
- I-C8: shard ごとの live id (= live pos) 集合に重複無し

c11s と同じく miri 抑制 (`#[cfg(not(miri))]`)、単一 thread path は miri pass。

### §4.4 single-shard testbed adapter

`research/src/single_shard.rs` の adapters に `C12sSingleShard` を追加し、`bench_single_shard` の variant に `"c12s"` を加える。c8/c9/c10s/c11s と同じ harness で:

- read-only / read-heavy / gim × zipf-{0.7,1.0,1.2} / adversarial-hot / uniform × T={1,2,4,8,16}
- 期待: **read-heavy zipf 16T で c8 (~21 Mops) を抜く** ことが c12s 採否の最大判定軸。c11s は同帯で c8 並だった。

### §4.5 スコープ外 (将来作業)

- **Loom systematic interleaving** (cap=2、2 threads、ops 数小): perf 見込みが立った後、`#[cfg(loom)]` で AtomicU16/U64/Usize を loom 版に切り替えて全 interleaving を網羅。c11s ですら入っていないので c12s の必須要件にしない
- **長時間 stress (時間〜日単位の continuous churn)**: promotion 検討フェーズで初めて入れる。c12s スコープ外
- **failure injection / panic 経路の不変条件保持**: K, V: Copy で Drop 走らないので比較的単純だが、明示テストは後付け

### §4.6 perf-gate と oracle test の影響範囲

c12s は research crate 配下なので senba 本体の perf-gate (`research/benches/sieve_cache_perf.rs`) には影響しない。`research/tests/oracle.rs` の `*_matches_orig_on_bundled_zipf` 系は CacheImpl を要求するが、c12s は `&self insert/get` を提供するので `&mut self` 版 CacheImpl 適合は trivial (single-thread caller でも内部 atomic で動く)。

## §5 実装ロードマップ — perf gate を中継点に置く incremental 構成

> 原則 ([feedback memory](../../../.claude/projects/-home-saka1-repos-senba-cache/memory/feedback_perf_first_correctness_later.md)): perf 見込みが立たない実装に correctness machinery を先行投資しない。perf checkpoint で「想定下回り」が出たら correctness 投資前に design に戻る。

### Phase 1: skeleton + single-thread (~3–4 hours)

1. `research/src/experimental/sieve_c12s.rs` 新設
2. struct fields: `tags: Box<[AtomicU16]>` (= cap LANE align)、`visited: Box<[AtomicU64]>`、`entries: UnsafeCell<Box<[MaybeUninit<Entry>]>>`、`hand: AtomicUsize`、`len: AtomicUsize`、capacity 等
3. `new(cap)` + `len/capacity/is_empty/contains/get` の reader path (c11s から copy + visited fetch_or 維持 / VISITED bit を tag から消す)
4. `insert` の Path B (warmup) のみ実装
5. **§4.1 の単体テスト (cache_initially_empty / insert_then_get / etc.) を pass させる** (warmup phase だけでも基本セット動く)
6. § 4.2 oracle test を **warmup までの trace で pass 確認** (ops 数を cap 以下に縮めた版)

### Phase 2: 単一スレッド evict (~3–4 hours)

7. `evict_one` 実装 (single-thread でも CAS path を踏む)
8. `insert` Path C (evict + install at pos)
9. `insert` Path A (update)
10. **§4.2 oracle test を full trace (10000 ops) で pass 確認**: 失敗したら **設計に戻る** (install-at-evicted-pos の SIEVE 等価性仮説の誤りなので根本検討)
11. **§4.4 bench_single_shard に C12sSingleShard adapter 追加**
12. **§Phase 2 perf checkpoint**: 1T で c11s 比 ±10% 以内に収まるか? 大幅 regress (>20%) なら CAS の overhead が想定超え → Phase 3 に進む前に design 見直し

### Phase 3: 並行不変条件確認 (~2–3 hours)

13. **§4.3 concurrent_invariants_under_zipf の port** (4 thread × 50000 ops Zipf)
14. I-C1/C3/C5/C8 の assert を厳しめに (id == pos / live id 重複なし / len ≤ cap)
15. miri は単一 thread path のみで pass、並行 path は `#[cfg(not(miri))]` で抑制
16. テストが落ちる場合の最初の疑い: I-C8 (entries の writer-writer race)、I-C5 (release ordering)、Path A の retry loop

### Phase 4: 主目的 perf 検証 (~2 hours)

17. **§4.4 sweep を T={1,2,4,8,16} × 5 workloads × 3 op-mixes で実行**
18. **採否判定**:
    - **read-heavy zipf 16T で c8 を 5%+ 上回る** → c12s 成立、Phase 5 へ
    - 並 (±5%) → 設計部分の boost 余地検討 (e.g. hand fetch_add の thread 別 stride、visited per-line padding)
    - 下回る → c11s に戻り、別 attack vector (e.g. shard 数増加、micro-batched insert) を検討
19. read-only adversarial-hot 等の既存軸で c11s を維持できているか確認 (regression 起こしていないか)

### Phase 5: 仕上げ (~2 hours)

20. clippy `--all-targets -D warnings` 通過
21. cargo fmt
22. doc comment の整備 (mod 先頭に「c11s からの差分」と「senba::Cache 由来部品」明記)
23. report 作成: `docs/reports/2026-05-08-c12s-cas-slot-claim.md` (or 翌日付) — Phase 4 の sweep 結果と判定理由

### スコープ外 (Phase 0 で確定)

- Loom test
- 長時間 stress
- senba::concurrent::Cache promotion の library 側コード (= 別タスク、c12s perf 確定後)
- non-Copy V のサポート (= 別変種でやる)
- remove API のサポート (= I-C1 と衝突するので別設計、c12s 範囲外)

## §6 promotion path (将来検討)

c12s の perf が成立したら、senba::concurrent::Cache を以下構成で起こす案:

- `senba/src/lib.rs` に `pub mod concurrent;` を生やす
- `senba/src/concurrent/mod.rs` に `pub struct Cache<K, V, S = Slot32, H = Xxh3Build>` (sequential 版と並列、`&self` メソッド)
- `senba/src/concurrent/shard.rs` に c12s の `Shard` を移植 (research crate からの promotion)
- structural skeleton (SlotSize / AlignedTags / hash) は senba 本体と共有
- perf gate を `research/benches/sieve_cache_concurrent_perf.rs` (新設) で固定

c12s が research に居続ける限り `senba` パッケージの crates.io payload には影響しない。promotion は別 PR / 別判定で。

## §7 open questions / 後の検証で詰める

- **Phase 2 で 1T overhead が想定超えだった場合の plan B**: hand を Atomic にしたまま、CAS を `unsynchronized` 化 (= relax) できる軸があるか? (e.g. eviction_one を thread-local hand pointer + amortized publish にする)
- **read-heavy 95/5 で writer 5% 比率の中、Path A (update) と Path C (evict+install) の比率予測**: zipf hot key の update path が支配なら Path A の 2 段 CAS 効率が肝、cold key の install 中心なら Path C 主役
- **adversarial-hot gim の 671 Mops (c11s) との比較**: c12s では writer も lock-free なので、key=0 の繰り返し insert (= Path A 連発) が c11s より速くなるはず。1 hot key の writer 集中軸として補助的判定
- **I-C10 の duplicate 発生頻度**: adversarial-hot gim は同 key=0 を全 thread が update するので I-C10 の race を最も踏みやすい。duplicate が発生しても SIEVE で 1 sweep 以内に解消する仮説の検証になる
- **同 key concurrent update の対策案**: 将来の variant で、Path A の transient EMPTY を別 sentinel tag (= IN_FLIGHT marker) に置換する案、あるいは writer per-shard の seqlock counter で existence query を linearize する案を検討余地。c12s では gap 明記のみ

実装後、Phase 4 の bench で上記が numerically どう出るかを確認 → §3/§4 の仮説のうちどれが当たり/外れだったかを post-mortem で記録。
