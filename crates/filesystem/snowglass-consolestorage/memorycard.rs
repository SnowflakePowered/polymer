use crate::error::Error;
// use crate::savegame::SaveGame;

//
// pub trait RawMemoryCard<R: SaveGame> {
//     const BYTES_PER_BLOCK: u32;
//     const BLOCK_SIZES: &'static [u32];
//
//     fn savegames(&self) -> impl Iterator<Item = R>;
//     fn append_savegame(&self, savegame: &[u8]) -> Result<R, Error>;
// }

// pub trait MemoryCard<R: RawSaveGame> {
//     fn import_from_bytes(bytes: &[u8]) -> Result<Self, Error>
//     where
//         Self: Sized;
//
//     fn add_raw_save(&mut self, bytes: &[u8]) -> Result<S, Error>;
//
//     fn savegames(&self) -> impl Iterator<Item = S>;
// }
