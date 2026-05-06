#[cfg(feature = "experimental")]
pub mod experimental;
#[cfg(feature = "experimental")]
pub mod sieve_orig;

pub mod hash;
pub mod sieve_cache;
pub mod workload;

#[cfg(feature = "experimental")]
pub use experimental::CacheImpl;
pub use sieve_cache::{Cache, Drain, Slot16, Slot32, Slot64, SlotSize, Stats};
