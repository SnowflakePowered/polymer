mod console;
pub mod error;
pub mod history;
pub mod impls;
pub mod installable;
pub mod structure;

pub mod nand;
pub mod save_repo;
pub mod savegame;
mod savegroups;
mod storage;

pub use console::{ConsoleStorage, Nand, StorageKind};
pub use error::Error;
pub use history::{
    AttachmentInfo, DeletedSaveInfo, Save, SaveAttachmentKind, SaveCommit, SaveCommitId,
    SaveCommitInfo, SaveCommitOperation, SaveVersioning, VersionedStorage,
};

pub use save_repo::SaveRepo;
pub use storage::manifest::{ArtifactRegistry, ArtifactSummary, VolumeInfo};
pub use storage::memorycard::{MemoryCard, MemoryCardView, Reconcile};
pub use storage::{
    BoxFuture, ChangeSignal, ComposedStorage, DirEntry, DirectoryStorage, EntryKind,
    GitVolumeStorage, InstallResult, Metadata, MountableStorage, PlatformRepository, ReadDir,
    Storage, StorageFile, StorageReader, VolumeStorage, permissions::*,
};
