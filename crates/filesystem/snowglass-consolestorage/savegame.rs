use std::borrow::Cow;
use std::path::PathBuf;
use std::process::Output;
use futures::Stream;
use crate::error::Error;
use image::RgbaImage;
use crate::BoxFuture;

pub struct SaveDescriptor {
    pub id: SaveDepotId,
    pub banner: Option<RgbaImage>,
    pub title: String,
    pub description: String,
    pub timestamp: jiff::Timestamp,
    pub root_path: String,
}

/// Identifies a save within its volume. Wii U uses the title id it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveDepotId(pub u64);

pub struct SaveDepot {
    pub save_descriptor: SaveDescriptor,
    pub blobs: Vec<SaveBlob>,
}

pub struct SaveBlob {
    /// Where the blob belongs, relative to the volume root.
    pub target_path: PathBuf,
    pub blake3: [u8; 32],
    pub bytes: Vec<u8>,
}

pub trait SaveGameSink {
    fn enumerate_saves(&self) -> impl Iterator<Item = SaveDescriptor> + '_;

    fn export_save<'a>(&self, save_descriptor: &SaveDepotId) -> Result<SaveDepot, Error>;

    fn install_save<'a>(
        &'a self,
        blobs: impl Stream<Item = SaveBlob> + Send + 'a,
    ) -> BoxFuture<'a, Result<SaveDescriptor, Error>>;
}
