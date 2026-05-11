//! Process-global TLS-id allocator used by routing-affinity variants
//! (e.g. [`crate::experimental::sieve_r1`]).
//!
//! Each thread receives a unique `u32` id on its first call to
//! [`current_tls_id`]; ids are never recycled. The allocator's design
//! and trade-offs vs. CPU-id (rseq) are discussed in
//! `docs/reports/2026-05-12-r1-design.md` §3.2.

use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

/// Sentinel for "thread has not yet been assigned an id". `u32::MAX` is
/// reserved so the allocator's hot path is a single TLS load + a
/// predicted-not-taken branch, with no enum tag overhead.
const UNASSIGNED: u32 = u32::MAX;

static NEXT_TLS_ID: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static TLS_ID: Cell<u32> = const { Cell::new(UNASSIGNED) };
}

/// Returns the calling thread's TLS id, allocating one on first access.
#[inline]
pub fn current_tls_id() -> u32 {
    TLS_ID.with(|cell| {
        let id = cell.get();
        if id != UNASSIGNED {
            id
        } else {
            let new_id = NEXT_TLS_ID.fetch_add(1, Ordering::Relaxed);
            assert!(
                new_id < UNASSIGNED,
                "tls_id: exhausted u32 id space (allocator returned the sentinel u32::MAX)"
            );
            cell.set(new_id);
            new_id
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_thread_returns_same_id() {
        let a = current_tls_id();
        let b = current_tls_id();
        assert_eq!(a, b);
    }

    #[test]
    fn distinct_threads_receive_distinct_ids() {
        let collected = std::sync::Mutex::new(Vec::<u32>::new());
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    let id = current_tls_id();
                    collected.lock().unwrap().push(id);
                });
            }
        });
        let mut ids = collected.into_inner().unwrap();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 8, "thread tls_ids must be distinct");
    }
}
