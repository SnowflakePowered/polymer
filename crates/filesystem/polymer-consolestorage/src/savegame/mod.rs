use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use futures::{FutureExt, Stream};
use parking_lot::Mutex;

use crate::BoxFuture;
use crate::error::Error;
use crate::storage::ChangeSignal;
pub use id::SaveDepotId;

pub mod icons;
pub mod id;

pub struct SaveDescriptor {
    pub id: SaveDepotId,
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
    binary_digest: Option<String>,
}

impl Package {
    pub fn new(serial: Option<String>, title_id: Option<String>) -> Self {
        Self {
            serial,
            title_id,
            binary_digest: None,
        }
    }

    pub fn with_binary_digest(mut self, binary_digest: impl Into<String>) -> Self {
        self.binary_digest = Some(binary_digest.into());
        self
    }

    pub fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }

    pub fn title_id(&self) -> Option<&str> {
        self.title_id.as_deref()
    }

    pub fn binary_digest(&self) -> Option<&str> {
        self.binary_digest.as_deref()
    }
}

impl SaveWatcher {
    pub fn new(saves: Vec<SaveDepotId>, signal: Box<dyn ChangeSignal>) -> Self {
        Self { saves, signal }
    }

    pub fn with_fingerprint<State>(
        saves: Vec<SaveDepotId>,
        activity: Box<dyn ChangeSignal>,
        settle: Duration,
        fingerprint: impl Fn() -> State + Send + Sync + 'static,
    ) -> Self
    where
        State: Eq + Send + 'static,
    {
        let last = fingerprint();
        Self::new(
            saves,
            Box::new(FingerprintSignal {
                activity,
                settle,
                fingerprint,
                last: Mutex::new(last),
            }),
        )
    }

    pub fn with_settle(
        saves: Vec<SaveDepotId>,
        activity: Box<dyn ChangeSignal>,
        settle: Duration,
    ) -> Self {
        Self::new(saves, Box::new(SettledActivitySignal { activity, settle }))
    }

    pub fn saves(&self) -> &[SaveDepotId] {
        &self.saves
    }

    pub fn changed(&self) -> BoxFuture<'_, ()> {
        self.signal.changed()
    }
}

struct SettledActivitySignal {
    activity: Box<dyn ChangeSignal>,
    settle: Duration,
}

impl ChangeSignal for SettledActivitySignal {
    fn changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.activity.changed().await;
            loop {
                tokio::time::sleep(self.settle).await;
                if self.activity.changed().now_or_never().is_none() {
                    return;
                }
            }
        })
    }
}

struct FingerprintSignal<State, Fingerprint> {
    activity: Box<dyn ChangeSignal>,
    settle: Duration,
    fingerprint: Fingerprint,
    last: Mutex<State>,
}

impl<State, Fingerprint> ChangeSignal for FingerprintSignal<State, Fingerprint>
where
    State: Eq + Send,
    Fingerprint: Fn() -> State + Send + Sync,
{
    fn changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            loop {
                self.activity.changed().await;
                tokio::time::sleep(self.settle).await;
                let current = (self.fingerprint)();
                let mut last = self.last.lock();
                if *last != current {
                    *last = current;
                    return;
                }
            }
        })
    }
}

pub type SaveDescriptors<'a> = Box<dyn Iterator<Item = SaveDescriptor> + 'a>;
pub type SaveBlobStream<'a> = Pin<Box<dyn Stream<Item = SaveBlob> + Send + 'a>>;

pub trait SaveGameSink: Send + Sync {
    fn infer_save_depot(&self, package: &Package) -> SaveDepotId;

    fn enumerate_saves(&self) -> SaveDescriptors<'_>;

    fn export_save(&self, save_descriptor: &SaveDepotId) -> Result<SaveDepot, Error>;

    fn install_save<'a>(
        &'a self,
        blobs: SaveBlobStream<'a>,
    ) -> BoxFuture<'a, Result<SaveDescriptor, Error>>;

    fn watch_saves(&self, saves: &[SaveDepotId]) -> Result<SaveWatcher, Error>;
}
