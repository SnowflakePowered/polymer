use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::{BStr, ByteSlice};
use gix::objs::Write as _;
use gix::refs::Target;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use jiff::Timestamp;
use parking_lot::{Mutex, RwLock};
use tokio::io::AsyncRead;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::installable::artifacts::{Artifact, ArtifactEntrypoint};
use crate::installable::{DepotFile, DepotType, EmbeddedIcon, IconHint, InstallEvents, Reporting};
use crate::storage::manifest::{ArtifactRegistry, ArtifactSummary, VolumeInfo};
use crate::{
    BoxFuture, ChangeSignal, DirectoryStorage, EntryKind, Error, InstallResult, Metadata,
    MountableStorage, ReadDir, Storage, StorageFile, StorageKind, VolumeStorage,
};

mod artifact;
mod checkout;
mod commit;
mod history;
mod registry;
mod repository;
mod traits;
mod volume;

use artifact::*;
use registry::*;
pub use repository::PlatformRepository;
pub use volume::GitVolumeStorage;
use volume::*;

const SCHEMA_VERSION: u32 = 2;
const CHECKOUT_STAT: gix::index::entry::stat::Options = gix::index::entry::stat::Options {
    trust_ctime: false,
    check_stat: true,
    use_nsec: true,
    use_stdev: false,
};

impl StorageKind {
    fn ref_name(self, id: Uuid) -> String {
        let namespace = match self {
            Self::Nand => "nands",
            Self::SaveRepo | Self::MemoryCard | Self::Battery => "profiles",
        };
        format!(
            "refs/consoles/{namespace}/v{SCHEMA_VERSION}/{}",
            id.simple()
        )
    }

    fn metadata_name(self) -> &'static str {
        match self {
            Self::Nand => "nand",
            Self::SaveRepo => "save-repo",
            Self::MemoryCard => "memory-card",
            Self::Battery => "battery",
        }
    }

    fn checkout_namespace(self) -> &'static str {
        match self {
            Self::Nand => "nands",
            Self::SaveRepo => "save-repos",
            Self::MemoryCard => "memory-cards",
            Self::Battery => "battery-profiles",
        }
    }
}

impl ChangeSignal for Arc<Notify> {
    fn changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.notified())
    }
}

fn write_text(
    repo: &gix::Repository,
    editor: &mut gix::object::tree::Editor<'_>,
    path: &str,
    value: &str,
) -> Result<(), Error> {
    let blob = repo
        .write_blob(value.as_bytes())
        .map_err(|error| Error::git("write metadata value", error))?
        .detach();
    editor
        .upsert(path, gix::objs::tree::EntryKind::Blob, blob)
        .map(|_| ())
        .map_err(|error| Error::git("update metadata value", error))
}

fn signature() -> gix::actor::Signature {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let time = format!("{seconds} +0000");
    gix::actor::SignatureRef {
        name: BStr::new(b"Polymer"),
        email: BStr::new(b"storage@polymer.invalid"),
        time: &time,
    }
    .to_owned()
    .expect("Polymer generates a valid Git signature")
}

fn join(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_owned()
    } else if child.is_empty() {
        parent.to_owned()
    } else {
        format!("{parent}/{child}")
    }
}

fn directory_key(path: &str) -> String {
    blake3::hash(path.as_bytes()).to_hex().to_string()
}

fn artifact_key(id: &str) -> String {
    blake3::hash(id.as_bytes()).to_hex().to_string()
}
