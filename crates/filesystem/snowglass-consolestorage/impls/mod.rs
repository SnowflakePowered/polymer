pub mod ps1;
pub mod ps2;
pub mod wiiu;

pub use crate::savegame::id::{NintendoTitleId, SonyProductCode};

pub trait RawMemoryCardImage<'a>: Sized {
    type Error;

    type Save<'card>;

    const BLOCK_SIZE: u64;

    const MAX_BLOCKS: u32;

    /// Create an empty, owned memory-card image.
    ///
    /// `blocks` must be at most [`Self::MAX_BLOCKS`].
    fn empty(blocks: u32) -> Result<Self, Self::Error>;

    /// Create an immutable memory-card view over `bytes`.
    ///
    /// [`Self::BLOCK_SIZE`] * `blocks` must fit into `bytes`.
    fn new(bytes: &'a [u8], blocks: u32) -> Result<Self, Self::Error>;

    /// Create a mutable memory-card view over `bytes`.
    ///
    /// [`Self::BLOCK_SIZE`] * `blocks` must fit into `bytes`.
    fn new_mut(bytes: &'a mut [u8], blocks: u32) -> Result<Self, Self::Error>;

    /// Enumerate the saves in the raw memory card.
    fn saves(&self) -> Result<Vec<Self::Save<'_>>, Self::Error>;

    /// Append a save to the memory card.
    fn append_save(&mut self, save: Self::Save<'_>) -> Result<(), Self::Error>;
}
