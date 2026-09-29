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
    MemoryCardFileFactory, MemoryCardStorageFactory, MemoryCardVolumeFactory, MemoryFileFactory,
    NandVolumeFactory, PackageDirectoryFactory, TempDirectoryFactory,
};
pub use error::{VfsError, VfsResult};
pub use host::{
    FilesystemEndpoint, FilesystemHost, MountedFilesystem, PreparedFilesystemLaunch,
    UnsupportedFilesystemHost,
};
pub use path::{
    Component, Components, NativePathSemantics, Path, PathOwned, PathSemantics, PathStr,
    PosixPathSemantics, U8Path, U8PathBuf, U16Path, U16PathBuf, VfsComponent, VfsPath, VfsPathBuf,
    VfsPathUnit, WindowsPathSemantics,
};
pub use polymer_consolestorage::Permissions;
pub use plan::{Binding, BindingKind, BindingSpec, CompiledFilesystem, FilesystemPlan};
pub(crate) use resource::NoopResourceLease;
pub use resource::{
    CompositeResourceLease, ExclusiveResourceKey, FinalizationReason, Materialized,
    PreparedFilesystem, Resource, ResourceFactory, ResourceLease, ResourceOrigin,
};
pub use traits::{
    AdoptHandle, CreateOptions, DirectoryEntry, DirectoryIterator, DirectoryNode, FileKind,
    FileNode, Metadata, MetadataChanges, Node, OpenFile, OpenOptions, RemoveKind, Volume,
};
