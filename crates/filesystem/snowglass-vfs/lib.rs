//! Platform-neutral filesystem composition for emulator launches.
//!
//! This crate deliberately contains no FUSE or WinFSP integration. It defines
//! the namespace, permissions, resource lifecycle, and backend contracts used
//! by those adapters and by emulator plugins.

mod backends;
mod error;
mod host;
mod path;
mod permissions;
mod plan;
mod resource;
mod traits;

pub use backends::{
    AsarVolumeFactory, HostDirectoryFactory, HostFileFactory, MemoryCardFileFactory,
    MemoryCardStorageFactory, MemoryCardVolumeFactory, MemoryFileFactory, NandVolumeFactory,
    PackageDirectoryFactory, ScratchBacking, ScratchVolumeFactory,
};
pub use error::{VfsError, VfsResult};
pub use host::{
    FilesystemEndpoint, FilesystemHost, MockFilesystemHost, MountedFilesystem,
    PreparedFilesystemLaunch, UnsupportedFilesystemHost,
};
pub use path::{
    Component, Components, NativePathSemantics, Path, PathOwned, PathSemantics, PathStr,
    PosixPathSemantics, U8Path, U8PathBuf, U16Path, U16PathBuf, VfsComponent, VfsPath, VfsPathBuf,
    VfsPathUnit, WindowsPathSemantics,
};
pub use permissions::Permissions;
pub use plan::{
    Binding, BindingKind, BindingSpec, CompiledFilesystem, FilesystemPlan, ProfileLayout,
};
pub(crate) use resource::NoopResourceLease;
pub use resource::{
    CompositeResourceLease, ExclusiveResourceKey, FinalizationReason, Materialized,
    PreparedFilesystem, Resource, ResourceFactory, ResourceLease, ResourceOrigin,
};
pub use traits::{
    AdoptHandle, CreateOptions, DirectoryEntry, DirectoryIterator, DirectoryNode, FileKind,
    FileNode, Metadata, MetadataChanges, Node, OpenFile, OpenOptions, RemoveKind, Volume,
};
