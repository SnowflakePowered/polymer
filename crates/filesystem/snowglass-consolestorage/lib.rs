pub mod error;
pub mod impls;
pub mod installable;
pub mod structure;

mod memorycard;
pub mod nand;
pub mod savegame;
mod storage;

pub use error::Error;
pub use storage::manifest::{ArtifactRegistry, ArtifactSummary, NandMetadata};
pub use storage::{
    BoxFuture, ChangeSignal, DirEntry, EntryKind, ReadDir, DirectoryStorage, InstallResult, Metadata, Storage, StorageFile, StorageRead,
    permissions::*,
};
