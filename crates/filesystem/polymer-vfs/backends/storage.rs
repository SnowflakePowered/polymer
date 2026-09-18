use std::sync::Arc;

use parking_lot::Mutex;
use polymer_consolestorage::{EntryKind, Error as StorageError, MountableStorage, StorageFile};

use crate::{
    AdoptHandle, CreateOptions, DirectoryEntry, DirectoryIterator, DirectoryNode, FileKind,
    FileNode, Materialized, Metadata, MetadataChanges, Node, OpenFile, OpenOptions, Permissions,
    RemoveKind, ResourceLease, VfsComponent, VfsError, VfsPath, VfsPathBuf, VfsResult, Volume,
};

pub(crate) type StoragePermissions = Arc<dyn Fn(&str) -> Permissions + Send + Sync>;

pub(crate) fn unrestricted() -> StoragePermissions {
    Arc::new(|_| Permissions::ALL)
}

pub(crate) fn materialize_file(
    storage: Arc<dyn MountableStorage>,
    path: String,
    permissions: StoragePermissions,
    lease: Arc<dyn ResourceLease>,
) -> Materialized<dyn FileNode> {
    Materialized {
        backend: Arc::new(StorageFileNode {
            storage,
            path,
            permissions,
        }),
        lease,
    }
}

pub(crate) fn materialize_volume(
    storage: Arc<dyn MountableStorage>,
    root: String,
    permissions: StoragePermissions,
    lease: Arc<dyn ResourceLease>,
) -> Materialized<dyn Volume> {
    Materialized {
        backend: Arc::new(StorageVolume {
            storage,
            root,
            permissions,
        }),
        lease,
    }
}

struct StorageVolume {
    storage: Arc<dyn MountableStorage>,
    root: String,
    permissions: StoragePermissions,
}

impl Volume for StorageVolume {
    fn root(&self) -> VfsResult<Arc<dyn DirectoryNode>> {
        Ok(Arc::new(StorageDirectoryNode {
            storage: self.storage.clone(),
            path: self.root.clone(),
            permissions: self.permissions.clone(),
        }))
    }

    fn rename(&self, source: &VfsPath, target: &VfsPath) -> VfsResult<()> {
        let source = rebase(&self.root, source);
        let target = rebase(&self.root, target);
        require_write(&self.permissions, &source)?;
        require_write(&self.permissions, &target)?;
        self.storage
            .rename(&source, &target)
            .map_err(|error| storage_error(error, &source))
    }
}

struct StorageDirectoryNode {
    storage: Arc<dyn MountableStorage>,
    path: String,
    permissions: StoragePermissions,
}

impl DirectoryNode for StorageDirectoryNode {
    fn metadata(&self) -> VfsResult<Metadata> {
        let metadata = self
            .storage
            .metadata(&self.path)
            .map_err(|error| storage_error(error, &self.path))?;
        Ok(Metadata {
            kind: FileKind::Directory,
            len: metadata.len,
            readonly: !can_write(&self.permissions, &self.path),
        })
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        require_write(&self.permissions, &self.path)?;
        if changes.len.is_some() {
            return Err(VfsError::IsDirectory(vfs_path(&self.path)));
        }
        Ok(())
    }

    fn lookup(&self, name: &VfsComponent) -> VfsResult<Option<Node>> {
        let path = child(&self.path, &name.to_string_lossy());
        if !can_read(&self.permissions, &path) {
            return Ok(None);
        }
        match self.storage.metadata(&path) {
            Ok(metadata) => Ok(Some(node(
                &self.storage,
                &self.permissions,
                path,
                metadata.kind,
            ))),
            Err(StorageError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(None)
            }
            Err(error) => Err(storage_error(error, &path)),
        }
    }

    fn read_dir(&self) -> VfsResult<Box<DirectoryIterator>> {
        require_read(&self.permissions, &self.path)?;
        let listing = self
            .storage
            .read_dir(&self.path, false)
            .map_err(|error| storage_error(error, &self.path))?;
        let filter_permissions = self.permissions.clone();
        let node_permissions = self.permissions.clone();
        let entries = listing
            .filter(move |entry| can_read(&filter_permissions, &entry.path))
            .map(move |entry| {
                VfsComponent::new(&entry.name).map(|name| DirectoryEntry {
                    name,
                    metadata: Metadata {
                        kind: match entry.metadata.kind {
                            EntryKind::Directory => FileKind::Directory,
                            EntryKind::File => FileKind::File,
                        },
                        len: entry.metadata.len,
                        readonly: !can_write(&node_permissions, &entry.path),
                    },
                })
            })
            .collect::<Vec<_>>();
        Ok(Box::new(entries.into_iter()))
    }

    fn create_file(
        &self,
        name: &VfsComponent,
        options: CreateOptions,
    ) -> VfsResult<Arc<dyn FileNode>> {
        let path = child(&self.path, &name.to_string_lossy());
        require_write(&self.permissions, &path)?;
        let exists = self.storage.exists(&path);
        if options.exclusive && exists {
            return Err(VfsError::AlreadyExists(vfs_path(&path)));
        }
        if !exists {
            self.storage
                .create(&path)
                .map_err(|error| storage_error(error, &path))?;
        }
        Ok(Arc::new(StorageFileNode {
            storage: self.storage.clone(),
            path,
            permissions: self.permissions.clone(),
        }))
    }

    fn create_dir(&self, name: &VfsComponent) -> VfsResult<Arc<dyn DirectoryNode>> {
        let path = child(&self.path, &name.to_string_lossy());
        require_write(&self.permissions, &path)?;
        if self.storage.exists(&path) {
            return Err(VfsError::AlreadyExists(vfs_path(&path)));
        }
        self.storage
            .create_dir_all(&path)
            .map_err(|error| storage_error(error, &path))?;
        Ok(Arc::new(StorageDirectoryNode {
            storage: self.storage.clone(),
            path,
            permissions: self.permissions.clone(),
        }))
    }

    fn remove(&self, name: &VfsComponent, kind: RemoveKind) -> VfsResult<()> {
        let path = child(&self.path, &name.to_string_lossy());
        require_write(&self.permissions, &path)?;
        let metadata = self
            .storage
            .metadata(&path)
            .map_err(|error| storage_error(error, &path))?;
        match (kind, metadata.kind) {
            (RemoveKind::File, EntryKind::Directory) => Err(VfsError::IsDirectory(vfs_path(&path))),
            (RemoveKind::Directory, EntryKind::File) => {
                Err(VfsError::NotDirectory(vfs_path(&path)))
            }
            _ => self
                .storage
                .remove(&path)
                .map_err(|error| storage_error(error, &path)),
        }
    }
}

fn node(
    storage: &Arc<dyn MountableStorage>,
    permissions: &StoragePermissions,
    path: String,
    kind: EntryKind,
) -> Node {
    match kind {
        EntryKind::Directory => Node::Directory(Arc::new(StorageDirectoryNode {
            storage: storage.clone(),
            path,
            permissions: permissions.clone(),
        })),
        EntryKind::File => Node::File(Arc::new(StorageFileNode {
            storage: storage.clone(),
            path,
            permissions: permissions.clone(),
        })),
    }
}

struct StorageFileNode {
    storage: Arc<dyn MountableStorage>,
    path: String,
    permissions: StoragePermissions,
}

impl FileNode for StorageFileNode {
    fn metadata(&self) -> VfsResult<Metadata> {
        let metadata = self
            .storage
            .metadata(&self.path)
            .map_err(|error| storage_error(error, &self.path))?;
        Ok(Metadata {
            kind: FileKind::File,
            len: metadata.len,
            readonly: !can_write(&self.permissions, &self.path),
        })
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        require_write(&self.permissions, &self.path)?;
        if let Some(len) = changes.len {
            self.storage
                .truncate(&self.path, len)
                .map_err(|error| storage_error(error, &self.path))?;
        }
        Ok(())
    }

    fn open(&self, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
        require_read(&self.permissions, &self.path)?;
        if options.write {
            require_write(&self.permissions, &self.path)?;
        }
        let handle = self
            .storage
            .open(&self.path, options.write)
            .map_err(|error| storage_error(error, &self.path))?;
        if options.truncate {
            require_write(&self.permissions, &self.path)?;
            self.storage
                .truncate(&self.path, 0)
                .map_err(|error| storage_error(error, &self.path))?;
        }
        Ok(Box::new(StorageOpenFile {
            path: self.path.clone(),
            permissions: self.permissions.clone(),
            handle,
        }))
    }

    fn adopt(&self, source: Arc<dyn FileNode>) -> VfsResult<Arc<dyn AdoptHandle>> {
        let permissions = self.permissions.as_ref()(&self.path);
        if !permissions.contains(Permissions::WRITE | Permissions::ADOPT) {
            return Err(VfsError::Unsupported(
                "the storage file does not accept source-node adoption".into(),
            ));
        }
        let adoption = Arc::new(StorageFileAdoption {
            storage: self.storage.clone(),
            path: self.path.clone(),
            source,
            finalizing: Mutex::new(()),
        });
        adoption.finalize()?;
        Ok(adoption)
    }
}

struct StorageFileAdoption {
    storage: Arc<dyn MountableStorage>,
    path: String,
    source: Arc<dyn FileNode>,
    finalizing: Mutex<()>,
}

impl AdoptHandle for StorageFileAdoption {
    fn write(&self, offset: u64, buffer: &[u8]) -> VfsResult<()> {
        let _finalizing = self.finalizing.lock();
        let writer = self
            .storage
            .open(&self.path, true)
            .map_err(|error| storage_error(error, &self.path))?;
        let written = writer
            .write_at(offset, buffer)
            .map_err(|error| storage_error(error, &self.path))?;
        if written != buffer.len() {
            return Err(VfsError::Io(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "storage adoption write was incomplete",
            )));
        }
        Ok(())
    }

    fn truncate(&self, len: u64) -> VfsResult<()> {
        let _finalizing = self.finalizing.lock();
        self.storage
            .truncate(&self.path, len)
            .map_err(|error| storage_error(error, &self.path))
    }

    fn finalize(&self) -> VfsResult<()> {
        let _finalizing = self.finalizing.lock();
        let len = self.source.metadata()?.len;
        let reader = self.source.open(OpenOptions {
            read: true,
            write: false,
            truncate: false,
        })?;
        self.storage
            .truncate(&self.path, len)
            .map_err(|error| storage_error(error, &self.path))?;
        let writer = self
            .storage
            .open(&self.path, true)
            .map_err(|error| storage_error(error, &self.path))?;
        let mut offset = 0_u64;
        let mut buffer = vec![0_u8; 64 * 1024];
        while offset < len {
            let remaining = usize::try_from((len - offset).min(buffer.len() as u64))
                .expect("bounded by the transfer buffer");
            let read = reader.read(offset, &mut buffer[..remaining])?;
            if read == 0 {
                return Err(VfsError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "adopted source became shorter while reconciling",
                )));
            }
            let written = writer
                .write_at(offset, &buffer[..read])
                .map_err(|error| storage_error(error, &self.path))?;
            if written != read {
                return Err(VfsError::Io(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "storage adoption write was incomplete",
                )));
            }
            offset += read as u64;
        }
        writer
            .sync()
            .map_err(|error| storage_error(error, &self.path))
    }
}

struct StorageOpenFile {
    path: String,
    permissions: StoragePermissions,
    handle: Arc<dyn StorageFile>,
}

impl OpenFile for StorageOpenFile {
    fn metadata(&self) -> VfsResult<Metadata> {
        let metadata = self
            .handle
            .metadata()
            .map_err(|error| storage_error(error, &self.path))?;
        Ok(Metadata {
            kind: FileKind::File,
            len: metadata.len,
            readonly: !can_write(&self.permissions, &self.path),
        })
    }

    fn read(&self, offset: u64, buffer: &mut [u8]) -> VfsResult<usize> {
        self.handle
            .read_at(offset, buffer)
            .map_err(|error| storage_error(error, &self.path))
    }

    fn write(&self, offset: u64, buffer: &[u8]) -> VfsResult<usize> {
        if !can_write(&self.permissions, &self.path) {
            return Err(VfsError::PermissionDenied(vfs_path(&self.path)));
        }
        self.handle
            .write_at(offset, buffer)
            .map_err(|error| storage_error(error, &self.path))
    }

    fn truncate(&self, len: u64) -> VfsResult<()> {
        require_write(&self.permissions, &self.path)?;
        self.handle
            .set_len(len)
            .map_err(|error| storage_error(error, &self.path))
    }

    fn flush(&self) -> VfsResult<()> {
        self.handle
            .flush()
            .map_err(|error| storage_error(error, &self.path))
    }

    fn sync(&self) -> VfsResult<()> {
        self.handle
            .sync()
            .map_err(|error| storage_error(error, &self.path))
    }
}

fn can_read(permissions: &StoragePermissions, path: &str) -> bool {
    permissions(path).can_read()
}

fn can_write(permissions: &StoragePermissions, path: &str) -> bool {
    permissions(path).can_write()
}

fn require_read(permissions: &StoragePermissions, path: &str) -> VfsResult<()> {
    match can_read(permissions, path) {
        true => Ok(()),
        false => Err(VfsError::PermissionDenied(vfs_path(path))),
    }
}

fn require_write(permissions: &StoragePermissions, path: &str) -> VfsResult<()> {
    match can_write(permissions, path) {
        true => Ok(()),
        false => Err(VfsError::PermissionDenied(vfs_path(path))),
    }
}

fn child(parent: &str, name: &str) -> String {
    match parent.is_empty() {
        true => name.to_owned(),
        false => format!("{parent}/{name}"),
    }
}

fn rebase(root: &str, path: &VfsPath) -> String {
    let path = path.to_string_lossy();
    let path = path.trim_start_matches('/');
    match (root.is_empty(), path.is_empty()) {
        (true, _) => path.to_owned(),
        (_, true) => root.to_owned(),
        _ => format!("{root}/{path}"),
    }
}

fn vfs_path(path: &str) -> VfsPathBuf {
    VfsPathBuf::new(format!("/{path}")).unwrap_or_else(|_| VfsPathBuf::root())
}

pub(super) fn storage_error(error: StorageError, path: &str) -> VfsError {
    match error {
        StorageError::InvalidPath(path) => VfsError::InvalidPath {
            path,
            reason: "outside the mounted storage".to_owned(),
        },
        StorageError::Io { source, .. } => match source.kind() {
            std::io::ErrorKind::NotFound => VfsError::NotFound(vfs_path(path)),
            std::io::ErrorKind::PermissionDenied => VfsError::PermissionDenied(vfs_path(path)),
            std::io::ErrorKind::AlreadyExists => VfsError::AlreadyExists(vfs_path(path)),
            std::io::ErrorKind::DirectoryNotEmpty => VfsError::DirectoryNotEmpty(vfs_path(path)),
            _ => VfsError::Io(source),
        },
        other => VfsError::Unsupported(other.to_string()),
    }
}
