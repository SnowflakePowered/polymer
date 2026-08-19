use crate::error::Error;
use image::RgbaImage;

pub trait SaveGame {
    fn banner(&self) -> &RgbaImage;
    fn title(&self) -> &str;
    fn description(&self) -> &str;
    fn timestamp(&self) -> jiff::Timestamp;
}

pub trait RawSaveGame: SaveGame {
    /// The raw bytes of the raw saved game
    fn bytes(&self) -> &[u8];

    /// Try to parse raw bytes as a save game.
    fn try_from_bytes(bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized;
}

// todo: this could be a concrete impl, just delegate banner/title/desc?
pub trait TreeSaveGame: SaveGame {}

pub trait SaveGameLineage<R: SaveGame> {
    fn commit_snapshot(&self) -> Result<(), Error>;
    fn base(&self) -> R;
    fn snapshots(&self) -> impl Iterator<Item = R>;
}
