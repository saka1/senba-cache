pub mod sieve_cache;
pub use sieve_cache::{Cache, Drain, Slot16, Slot32, Slot64, SlotSize, Stats};

#[cfg(feature = "experimental")]
pub mod experimental;
#[cfg(feature = "experimental")]
pub mod workload;
#[cfg(feature = "experimental")]
pub use experimental::CacheImpl;
