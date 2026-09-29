use crate::{VfsComponent, VfsPath, VfsResult};
use std::{fmt, sync::Arc};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Metadata {
    pub kind: FileKind,
    pub len: u64,
    pub readonly: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MetadataChanges {
    pub len: Option<u64>,
    pub readonly: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OpenOptions {
    pub read: bool,
    pub write: bool,
    pub truncate: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CreateOptions {
    pub exclusive: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoveKind {
    File,
    Directory,
}

#[derive(Clone)]
pub enum Node {
    File(Arc<dyn FileNode>),
    Directory(Arc<dyn DirectoryNode>),
    SyntheticDirectory,
}

impl Node {
    pub const fn kind(&self) -> FileKind {
        match self {
            Self::File(_) => FileKind::File,
            Self::Directory(_) | Self::SyntheticDirectory => FileKind::Directory,
        }
    }

    pub fn metadata(&self) -> VfsResult<Metadata> {
        match self {
            Self::File(file) => file.metadata(),
            Self::Directory(directory) => directory.metadata(),
            Self::SyntheticDirectory => Ok(Metadata {
                kind: FileKind::Directory,
                len: 0,
                readonly: true,
            }),
        }
    }
}

impl fmt::Debug for Node {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Node").field(&self.kind()).finish()
    }
}

#[derive(Clone, Debug)]
pub struct DirectoryEntry {
    pub name: VfsComponent,
    pub metadata: Metadata,
}

impl DirectoryEntry {
    pub const fn kind(&self) -> FileKind {
        self.metadata.kind
    }
}

pub type DirectoryIterator = dyn Iterator<Item = VfsResult<DirectoryEntry>> + Send;

pub trait OpenFile: Send + Sync {
    fn metadata(&self) -> VfsResult<Metadata>;
    fn read(&self, offset: u64, buffer: &mut [u8]) -> VfsResult<usize>;
    fn write(&self, offset: u64, buffer: &[u8]) -> VfsResult<usize>;
    fn truncate(&self, len: u64) -> VfsResult<()>;
    fn flush(&self) -> VfsResult<()>;
    fn sync(&self) -> VfsResult<()>;
}

pub trait AdoptHandle: Send + Sync {
    fn write(&self, offset: u64, buffer: &[u8]) -> VfsResult<()>;
    fn truncate(&self, len: u64) -> VfsResult<()>;
    fn finalize(&self) -> VfsResult<()>;
}

pub trait FileNode: Send + Sync {
    fn metadata(&self) -> VfsResult<Metadata>;
    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()>;
    fn open(&self, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>>;

    /// Begin an atomic adoption operation. Requires [`Permissions::ADOPT`].
    fn adopt(&self, _source: Arc<dyn FileNode>) -> VfsResult<Arc<dyn AdoptHandle>> {
        Err(crate::VfsError::Unsupported(
            "the target file does not accept source-node adoption".into(),
        ))
    }
}

pub trait DirectoryNode: Send + Sync {
    fn metadata(&self) -> VfsResult<Metadata>;
    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()>;
    fn lookup(&self, name: &VfsComponent) -> VfsResult<Option<Node>>;
    fn read_dir(&self) -> VfsResult<Box<DirectoryIterator>>;
    fn create_file(
        &self,
        name: &VfsComponent,
        options: CreateOptions,
    ) -> VfsResult<Arc<dyn FileNode>>;
    fn create_dir(&self, name: &VfsComponent) -> VfsResult<Arc<dyn DirectoryNode>>;
    fn remove(&self, name: &VfsComponent, kind: RemoveKind) -> VfsResult<()>;
}

pub trait Volume: Send + Sync {
    fn root(&self) -> VfsResult<Arc<dyn DirectoryNode>>;

    fn mount(&self, format: Option<&str>) -> VfsResult<Node> {
        if let Some(format) = format {
            return Err(crate::VfsError::Unsupported(format!(
                "this storage does not support the '{format}' format"
            )));
        }
        Ok(Node::Directory(self.root()?))
    }

    /// Atomically renames two mount-relative paths within this volume.
    fn rename(&self, source: &VfsPath, target: &VfsPath) -> VfsResult<()>;

    /// Atomically renames `source` over `target` within this volume.
    ///
    /// Backends which cannot replace an existing target retain their normal
    /// rename behavior and should return `AlreadyExists`.
    fn replace(&self, source: &VfsPath, target: &VfsPath) -> VfsResult<()> {
        self.rename(source, target)
    }

    /// Removes a file from this volume's namespace while preserving the file
    /// object already returned by [`DirectoryNode::lookup`] or
    /// [`DirectoryNode::create_file`].
    fn detach(&self, _source: &VfsPath) -> VfsResult<()> {
        Err(crate::VfsError::Unsupported(
            "the source volume cannot detach file nodes".into(),
        ))
    }
}
