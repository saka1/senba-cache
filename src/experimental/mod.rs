//! Experimental SIEVE variants.
//!
//! Library-grade implementation lives in [`crate::sieve_cache`]; the spec/oracle
//! is [`crate::sieve_orig`]. This module collects the historical / exploratory
//! variants kept around for benchmark and design comparison.

pub mod sieve_c8;
pub mod sieve_j3;
pub mod sieve_j4;
pub mod sieve_j5;
pub mod sieve_j6;
pub mod sieve_j7;
pub mod sieve_j8;
pub mod sieve_v0;
pub mod sieve_v1;
pub mod sieve_v2;
pub mod sieve_v3;
