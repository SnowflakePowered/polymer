//! Platform-neutral filesystem composition for emulator launches.
//!
//! This crate deliberately contains no FUSE or WinFSP integration. It defines
//! the namespace, permissions, resource lifecycle, and backend contracts used
//! by those adapters and by emulator plugins.

mod backends;
mod error;
mod host;
mod path;
mod plan;
mod resource;
mod traits;

pub use backends::{
    AsarVolumeFactory, BatteryStorageFactory, HostDirectoryFactory, HostFileFactory,
    MemoryCardFileFactory, MemoryCardStorageFactory, MemoryFileFactory, NandVolumeFactory,
    PackageDirectoryFactory, TempDirectoryFactory,
};
pub use error::{VfsError, VfsResult};
pub use host::{
    FilesystemEndpoint, FilesystemHost, MountedFilesystem, PreparedFilesystemLaunch,
    UnsupportedFilesystemHost,
};
pub(crate) use path::Native;
pub use path::{PathStr, VfsComponent, VfsPath, VfsPathBuf, VfsPathUnit, VfsStr};
pub use plan::{Binding, BindingKind, BindingSpec, CompiledFilesystem, FilesystemPlan};
pub use polymer_consolestorage::Permissions;
pub use resource::{
    ExclusiveResourceKey, FinalizationReason, ManagedResourceLease, MaterializationScope,
    Materialized, PreparedFilesystem, Resource, ResourceFactory, ResourceFinalizer, ResourceLease,
    ResourceOrigin,
};
pub use traits::{
    AdoptHandle, CreateOptions, DirectoryEntry, DirectoryIterator, DirectoryNode, FileKind,
    FileNode, Metadata, MetadataChanges, Node, OpenFile, OpenOptions, RemoveKind, Volume,
};
