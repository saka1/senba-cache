//! Process-global per-thread routing-hint allocator used by routing-affinity
//! variants (e.g. [`crate::experimental::sieve_r1`]).
//!
//! Each thread receives a `u32` routing hint on its first call to
//! [`routing_hint`]; the hint is stable for the thread's lifetime and is
//! never recycled. Uniqueness across threads is best-effort (the allocator
//! hands out monotonically increasing values), not a contract — routing
//! only needs each thread to land on *some* shard consistently, not on a
//! distinct one. Allocator design and trade-offs vs. CPU-id (rseq) are
//! discussed in `docs/reports/2026-05-12-r1-design.md` §3.2.

use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

/// Sentinel for "thread has not yet been assigned a hint". `u32::MAX` is
/// reserved so the allocator's hot path is a single TLS load + a
/// predicted-not-taken branch, with no enum tag overhead.
const UNASSIGNED: u32 = u32::MAX;

static NEXT_ROUTING_HINT: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static TLS_ROUTING_HINT: Cell<u32> = const { Cell::new(UNASSIGNED) };
}

/// Returns the calling thread's routing hint, allocating one on first access.
#[inline]
pub fn routing_hint() -> u32 {
    TLS_ROUTING_HINT.with(|cell| {
        let hint = cell.get();
        if hint != UNASSIGNED {
            hint
        } else {
            let new_hint = NEXT_ROUTING_HINT.fetch_add(1, Ordering::Relaxed);
            assert!(
                new_hint < UNASSIGNED,
                "routing_hint: allocator returned the reserved sentinel u32::MAX"
            );
            cell.set(new_hint);
            new_hint
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_thread_returns_same_hint() {
        let a = routing_hint();
        let b = routing_hint();
        assert_eq!(a, b);
    }

    /// Distinctness across threads is an observed property of the
    /// monotonic allocator, not a contract — but it should hold for any
    /// small spawn.
    #[test]
    fn distinct_threads_receive_distinct_hints() {
        let collected = std::sync::Mutex::new(Vec::<u32>::new());
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    let hint = routing_hint();
                    collected.lock().unwrap().push(hint);
                });
            }
        });
        let mut hints = collected.into_inner().unwrap();
        hints.sort();
        hints.dedup();
        assert_eq!(hints.len(), 8, "thread routing hints must be distinct");
    }
}
