pub type Key = u64;

pub trait Trace: Iterator<Item = Key> {}
impl<T: Iterator<Item = Key>> Trace for T {}

pub mod file;
pub mod zipf;
