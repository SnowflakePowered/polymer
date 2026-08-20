use std::path::PathBuf;

use futures::Stream;
use image::RgbaImage;

use crate::BoxFuture;
use crate::error::Error;
use crate::storage::ChangeSignal;

pub struct SaveDescriptor {
    pub id: SaveDepotId,
    pub banner: Option<RgbaImage>,
    pub title: String,
    pub description: String,
    pub timestamp: jiff::Timestamp,
    pub root_path: String,
}

/// A unique ID for the save. This ID only has meaning
/// relative to the NAND it came from.
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

pub struct SaveWatcher<'a> {
    saves: Vec<SaveDepotId>,
    signal: Box<dyn ChangeSignal + 'a>,
}

impl<'a> SaveWatcher<'a> {
    pub fn new(saves: Vec<SaveDepotId>, signal: Box<dyn ChangeSignal + 'a>) -> Self {
        Self { saves, signal }
    }

    pub fn saves(&self) -> &[SaveDepotId] {
        &self.saves
    }

    pub fn changed(&self) -> BoxFuture<'_, ()> {
        self.signal.changed()
    }
}

pub trait SaveGameSink {
    fn enumerate_saves(&self) -> impl Iterator<Item = SaveDescriptor> + '_;

    fn export_save<'a>(&self, save_descriptor: &SaveDepotId) -> Result<SaveDepot, Error>;

    fn install_save<'a>(
        &'a self,
        blobs: impl Stream<Item = SaveBlob> + Send + 'a,
    ) -> BoxFuture<'a, Result<SaveDescriptor, Error>>;
    
    fn watch_saves(&self, saves: &[SaveDepotId]) -> Result<SaveWatcher<'_>, Error>;
}
