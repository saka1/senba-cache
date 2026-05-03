//! 全 SIEVE variant 共通の hash 戦略。
//!
//! NSDI'24 SIEVE リファレンス C 実装 (`external/NSDI24-SIEVE/.../hash.h`) は
//! デフォルトで XXH3 を使う。Rust std `HashMap` のデフォルト (SipHash13) は
//! DoS 耐性のための選択であって cache 研究の慣行とはズレるため、本 repo の
//! HashMap 系 variant は全て XXH3 (`Xxh3Build`) で揃える。

use std::hash::BuildHasher;
use xxhash_rust::xxh3::Xxh3;

#[derive(Default, Clone, Copy)]
pub struct Xxh3Build;

impl BuildHasher for Xxh3Build {
    type Hasher = Xxh3;
    #[inline]
    fn build_hasher(&self) -> Xxh3 {
        Xxh3::new()
    }
}
