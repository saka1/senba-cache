use super::super::*;

#[test]
fn tags_storage_is_32_byte_aligned_for_each_slot_size() {
    // `find_avx2` issues `_mm256_load_si256` against `tags.as_ptr()`. The
    // soundness of that load relies on `Vec<TagsChunk>` inheriting
    // `repr(C, align(32))` from its element type. Verify the contract holds
    // for every public `SlotSize` and at non-trivial cap (so multiple chunks
    // are allocated). Pairs with the const-eval `_TAGSCHUNK_ALIGN_OK` guard
    // by giving `cargo test --release` (where `debug_assert!` does not fire)
    // the same coverage.
    fn check<S: SlotSize>(cap: usize, shards: usize, label: &str) {
        let cache: Cache<u64, u64, S> = Cache::with_shards(cap, shards);
        for (i, sh) in cache.shards.iter().enumerate() {
            let addr = sh.tags.as_ptr() as usize;
            assert_eq!(
                addr & 31,
                0,
                "{label}: shard {i} tags base 0x{addr:x} not 32-byte aligned"
            );
            // Flat-view byte length must be a 32B multiple — the layout
            // claim that `AlignedTags::Deref`'s SAFETY argument relies on
            // (chunks × LANE × sizeof(u16) = chunks × 32).
            let bytes = sh.tags.len() * std::mem::size_of::<u16>();
            assert!(
                bytes.is_multiple_of(32),
                "{label}: tags byte len {bytes} not a 32B multiple"
            );
        }
    }
    check::<Slot16>(384, 8, "Slot16");
    check::<Slot32>(384, 8, "Slot32");
    check::<Slot64>(256, 8, "Slot64");
    // Edge case: tiny per-shard rounds up to a single LANE.
    // cap=8 / shards=1 ⇒ per_shard=8, order_cap = max(LANE, round_up(8, LANE)) = 16.
    check::<Slot32>(8, 1, "Slot32_min");
}
