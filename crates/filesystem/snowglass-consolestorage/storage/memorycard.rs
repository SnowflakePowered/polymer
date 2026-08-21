use crate::savegame::SaveDepotId;
use crate::Storage;


// Some mmapped memory that exposes a view of the memory card.
pub struct MemoryCardView {

}

pub trait MemoryCard : Storage {
  /// Expose the storage as a memory card, ensuring
  /// that the provided save depots are always present.
  ///
  const BLOCK_SIZE: u64;
  const MAX_SIZE_IN_BLOCKS: u64;

  fn expose(&self, primary: &[SaveDepotId], blocks: u32) -> MemoryCardView;
}