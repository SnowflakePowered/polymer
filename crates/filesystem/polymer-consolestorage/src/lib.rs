mod console;
pub mod error;
pub mod history;
pub mod impls;
pub mod installable;
pub mod keys;
pub mod structure;

pub mod nand;
pub mod save_repo;
pub mod savegame;
mod savegroups;
mod storage;

pub use console::{ConsoleStorage, StorageKind};
pub use error::Error;
pub use history::{
    AttachmentInfo, AttachmentScope, DeletedSaveInfo, Save, SaveAttachmentKind, SaveCommit,
    SaveCommitId, SaveCommitInfo, SaveCommitOperation, SaveVersioning, VersionedStorage,
};
pub use impls::battery::{
    BatteryIdentity, BatteryIdentitySource, BatterySaveRepo, BatterySaveView,
};

pub use save_repo::{ChangeKind, ChangedEntry, CheckpointGate, CheckpointRequest, SaveRepo};
pub use storage::manifest::{
    ArtifactComponent, ArtifactRegistry, ArtifactRevision, ArtifactSummary, VolumeInfo,
};
pub use storage::memorycard::{MemoryCard, MemoryCardView, Reconcile};
pub use storage::{
    BoxFuture, ChangeSignal, ComposedStorage, DirEntry, DirectoryStorage, EntryKind,
    ExternalTitleStorage, GitVolumeStorage, InstallResult, KeyProjection, KeyStoreStorage,
    MemoryFile, MemoryStorage, Metadata, MountableStorage, PlatformRepository, ReadDir,
    ReconcileStorage, ReconciliableStorage, ScratchStorage, Storage, StorageFile, StorageReader,
    TitleMount, VolumeStorage, permissions::*,
};
