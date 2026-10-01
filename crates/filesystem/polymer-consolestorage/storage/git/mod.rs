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
use typed_path::Utf8UnixPath;
use uuid::Uuid;

use crate::installable::artifacts::{Artifact, ArtifactEntrypoint};
use crate::installable::{DepotFile, DepotType, EmbeddedIcon, IconHint, InstallContext, Reporting};
use crate::storage::manifest::{ArtifactRegistry, ArtifactSummary, VolumeInfo};
use crate::{
    BoxFuture, ChangeSignal, DirectoryStorage, EntryKind, Error, InstallResult, Metadata,
    MountableStorage, ReadDir, Storage, StorageFile, StorageKind, VolumeStorage,
};

mod artifact;
mod checkout;
mod commit;
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

#[derive(Clone, Copy, PartialEq, Eq)]
struct TreeKey(blake3::Hash);

impl TreeKey {
    fn new(value: &str) -> Self {
        Self(blake3::hash(value.as_bytes()))
    }
}

impl fmt::Display for TreeKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl ChangeSignal for Arc<Notify> {
    fn changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.notified())
    }
}
