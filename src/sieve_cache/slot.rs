//! `SlotSize` sealed trait + `Slot16` / `Slot32` / `Slot64` ZST markers and
//! their backing `#[repr(C)] union` storage cells.

use std::mem::ManuallyDrop;

mod sealed {
    pub trait Sealed {}
}

/// Sealed trait that specifies the stride (in bytes) of one slot in the entries arena at the type level.
///
/// `S::SIZE` is always a power of two. `Storage<E>` uses a `#[repr(C)] union` internally,
/// placing the `entry` field at **offset 0**, so that reinterpreting `*const Storage<E>` as
/// `*const E` reaches `E` directly.
pub trait SlotSize: sealed::Sealed + 'static {
    /// Slot stride in bytes for this bracket. Always a power of two.
    const SIZE: usize;
    /// Per-bracket storage cell type. Each impl defines a union to ensure
    /// `size_of::<Storage<E>>() == SIZE`.
    type Storage<E>: Sized;
}

/// `Slot16` bracket: stride = 16 bytes.
/// Typical for small primitive pairs such as `(u32, u32)` or `(u64, u64)`.
pub struct Slot16;
/// `Slot32` (default) bracket: stride = 32 bytes.
/// Typical for string-cache use cases such as `(String, V_small)` or `(Arc<str>, Arc<str>)`.
pub struct Slot32;
/// `Slot64` bracket: stride = 64 bytes.
/// For heavier entries such as `(String, String)` or `(K, V_struct_up_to_56B)`.
pub struct Slot64;

impl sealed::Sealed for Slot16 {}
impl sealed::Sealed for Slot32 {}
impl sealed::Sealed for Slot64 {}

#[repr(C)]
pub union Slot16Storage<E> {
    entry: ManuallyDrop<E>,
    _pad: [u64; 2],
}

#[repr(C)]
pub union Slot32Storage<E> {
    entry: ManuallyDrop<E>,
    _pad: [u64; 4],
}

#[repr(C)]
pub union Slot64Storage<E> {
    entry: ManuallyDrop<E>,
    _pad: [u64; 8],
}

impl SlotSize for Slot16 {
    const SIZE: usize = 16;
    type Storage<E> = Slot16Storage<E>;
}
impl SlotSize for Slot32 {
    const SIZE: usize = 32;
    type Storage<E> = Slot32Storage<E>;
}
impl SlotSize for Slot64 {
    const SIZE: usize = 64;
    type Storage<E> = Slot64Storage<E>;
}
