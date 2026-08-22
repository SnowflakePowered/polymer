use crate::{NativePathSemantics, PathSemantics, VfsComponent, VfsPath, VfsResult};
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
pub enum Node<S: PathSemantics = NativePathSemantics> {
    File(Arc<dyn FileNode>),
    Directory(Arc<dyn DirectoryNode<S>>),
    SyntheticDirectory,
}

impl<S: PathSemantics> Node<S> {
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

impl<S: PathSemantics> fmt::Debug for Node<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("Node").field(&self.kind()).finish()
    }
}

#[derive(Clone, Debug)]
pub struct DirectoryEntry<S: PathSemantics = NativePathSemantics> {
    pub name: VfsComponent<S>,
    pub node: Node<S>,
}

impl<S: PathSemantics> DirectoryEntry<S> {
    pub const fn kind(&self) -> FileKind {
        self.node.kind()
    }
}

pub type DirectoryIterator<S = NativePathSemantics> =
    dyn Iterator<Item = VfsResult<DirectoryEntry<S>>> + Send;

pub trait OpenFile: Send + Sync {
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

pub trait DirectoryNode<S: PathSemantics = NativePathSemantics>: Send + Sync {
    fn metadata(&self) -> VfsResult<Metadata>;
    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()>;
    fn lookup(&self, name: &VfsComponent<S>) -> VfsResult<Option<Node<S>>>;
    fn read_dir(&self) -> VfsResult<Box<DirectoryIterator<S>>>;
    fn create_file(
        &self,
        name: &VfsComponent<S>,
        options: CreateOptions,
    ) -> VfsResult<Arc<dyn FileNode>>;
    fn create_dir(&self, name: &VfsComponent<S>) -> VfsResult<Arc<dyn DirectoryNode<S>>>;
    fn remove(&self, name: &VfsComponent<S>, kind: RemoveKind) -> VfsResult<()>;
}

pub trait Volume<S: PathSemantics = NativePathSemantics>: Send + Sync {
    fn root(&self) -> VfsResult<Arc<dyn DirectoryNode<S>>>;

    fn mount(&self, format: Option<&str>) -> VfsResult<Node<S>> {
        if let Some(format) = format {
            return Err(crate::VfsError::Unsupported(format!(
                "this storage does not support the '{format}' format"
            )));
        }
        Ok(Node::Directory(self.root()?))
    }

    /// Atomically renames two mount-relative paths within this volume.
    fn rename(&self, source: &VfsPath<S>, target: &VfsPath<S>) -> VfsResult<()>;

    /// Atomically renames `source` over `target` within this volume.
    ///
    /// Backends which cannot replace an existing target retain their normal
    /// rename behavior and should return `AlreadyExists`.
    fn replace(&self, source: &VfsPath<S>, target: &VfsPath<S>) -> VfsResult<()> {
        self.rename(source, target)
    }

    /// Removes a file from this volume's namespace while preserving the file
    /// object already returned by [`DirectoryNode::lookup`] or
    /// [`DirectoryNode::create_file`].
    fn detach(&self, _source: &VfsPath<S>) -> VfsResult<()> {
        Err(crate::VfsError::Unsupported(
            "the source volume cannot detach file nodes".into(),
        ))
    }
}
