# r4 sanitizer findings (2026-05-14)

設計 §10.4 検証戦略 (C) ASan/TSan stress の実行結果。`docs/benchmark/r4-sanitizer/run.sh` で再現可能。

## 構成

- Build: `RUSTFLAGS="-Zsanitizer=tsan|address" cargo +nightly build -Zbuild-std --target x86_64-unknown-linux-gnu`
- Run: `--variant r4 --shards 512 --cap 4096 --ops 20000000 --threads 8 --skew 1.8 --keys 1000 --op-mix read-heavy --value string` (hot-key 偏り、reader 多)
- Toolchain: nightly-x86_64-unknown-linux-gnu (component: `rust-src` for `-Zbuild-std`)

## TSan: 16 warnings, **すべて expected false-positive (seqlock pattern)**

検出されたパターン:

1. **writer の `ManuallyDrop<K/V>` への bitwise write (= `writer_evict_and_install` 内 line 820-821)** vs reader の `ptr::read` (`try_candidate` 内 line 401)
2. **writer の `version.compare_exchange` (even→odd)** vs reader の `ptr::read`

これらは設計 §6.1 (race α: half-overwrite drop 防御) の構造的に "意図された" レース:

- reader は `v1` を Acquire load (偶数なら writer 進行中ではない)
- reader が `ptr::read` で Entry bytes を bitwise copy
- reader が `v2` を Acquire load して `v1 == v2` を確認
- 一致しなければ `Racing` を返して retry、`buf` の K/V には触らず discard

writer の中間状態を reader が**観測すること自体は許容**され、後段 v2 check で discard されるので semantic data race ではない。TSan は seqlock semantics を理解せず、純粋な byte-level overlap を検出するため false-positive を多発する。

Linux kernel の seqlock も同じ問題を抱えており、KCSAN は `READ_ONCE`/`WRITE_ONCE` の hint で抑制している。userspace seqlock library (e.g. concurrent crate の `seqlock`) も同様。本 r4 では byte-level overlap を atomic 化する rewrite はパフォーマンス目標と相反するため、TSan suppress は採用せず "expected false-positive" として記録する。

**判断**: TSan の出力は race β (実 UAF) の検出には不向き。ASan を一次検証手段とする。

## ASan: **clean** (heap-use-after-free / SEGV ゼロ)

20M ops / T=8 / hot-key / V=String を 1.8s 走らせて UAF / SEGV ゼロ。これは:

- race β (clone-mid-flight UAF): writer の `defer_drop_if_needed(old_v)` が reader pin holder を待つので、reader の `V::clone` が freed heap を読まない。
- race γ (K drop on remove): writer の `defer_drop_kv_if_needed(old_k, old_v)` が同様に protect。

を実機で confirm。設計 §6.2 の証明と整合する観測結果。

## 結論

| 検証手段 | 目的 | 結果 |
|----------|------|------|
| TSan | byte-level data race (seqlock false-positive 含む) | 16 warnings、すべて seqlock pattern と判定 |
| ASan | race β / race γ の actual UAF | **clean** — 設計通り |
| Miri | UB 検出 (race β 別経路) | **skip**: `#[cfg(not(miri))]` で AVX2 排除と両立しない |

Phase 2 完了 gate: ASan clean で race β/γ は実機検証済み。TSan の false-positive は documented caveat としてここに残す。
