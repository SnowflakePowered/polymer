mod console;
pub mod error;
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

pub use save_repo::SaveRepo;
pub use storage::manifest::{ArtifactRegistry, ArtifactSummary, VolumeInfo};
pub use storage::memorycard::{MemoryCard, MemoryCardView, Reconcile};
pub use storage::{
    BoxFuture, ChangeSignal, ComposedStorage, DirEntry, DirectoryStorage, EntryKind, InstallResult,
    Metadata, MountableStorage, ReadDir, Storage, StorageFile, StorageRead, StorageStream,
    VolumeStorage, permissions::*,
};
