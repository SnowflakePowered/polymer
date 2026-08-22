use std::path::PathBuf;

use futures::Stream;
use image::RgbaImage;

use crate::BoxFuture;
use crate::error::Error;
use crate::storage::ChangeSignal;
pub use id::SaveDepotId;

pub mod id;

pub struct SaveDescriptor {
    pub id: SaveDepotId,
    pub banner: Option<RgbaImage>,
    pub title: String,
    pub description: String,
    pub timestamp: jiff::Timestamp,
    pub root_path: String,
}

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

pub struct SaveWatcher {
    saves: Vec<SaveDepotId>,
    signal: Box<dyn ChangeSignal>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Package {
    serial: Option<String>,
    title_id: Option<String>,
}

impl Package {
    pub fn new(serial: Option<String>, title_id: Option<String>) -> Self {
        Self { serial, title_id }
    }

    pub fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }

    pub fn title_id(&self) -> Option<&str> {
        self.title_id.as_deref()
    }
}

impl SaveWatcher {
    pub fn new(saves: Vec<SaveDepotId>, signal: Box<dyn ChangeSignal>) -> Self {
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
    fn infer_save_depot(&self, package: &Package) -> SaveDepotId;

    fn enumerate_saves(&self) -> impl Iterator<Item = SaveDescriptor> + '_;

    fn export_save<'a>(&self, save_descriptor: &SaveDepotId) -> Result<SaveDepot, Error>;

    fn install_save<'a>(
        &'a self,
        blobs: impl Stream<Item = SaveBlob> + Send + 'a,
    ) -> BoxFuture<'a, Result<SaveDescriptor, Error>>;

    fn watch_saves(&self, saves: &[SaveDepotId]) -> Result<SaveWatcher, Error>;
}
