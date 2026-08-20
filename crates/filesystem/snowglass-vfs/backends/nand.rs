use std::sync::Arc;

use parking_lot::Mutex;
use snowglass_consolestorage::nand::VirtualNand;
use snowglass_consolestorage::{
    EntryKind, Error as NandError, Permissions as NandPermissions, StorageFile, StorageRead,
};

use crate::{
    CreateOptions, DirectoryEntry, DirectoryIterator, DirectoryNode, FileKind, FileNode,
    Materialized, Metadata, MetadataChanges, Node, OpenFile, OpenOptions, Permissions, RemoveKind,
    ResourceFactory, VfsComponent, VfsError, VfsPath, VfsPathBuf, VfsResult, Volume,
};

impl From<NandPermissions> for Permissions {
    fn from(grants: NandPermissions) -> Self {
        Self::from_bits_truncate(grants.bits())
    }
}

pub struct NandVolumeFactory<N> {
    nand: Arc<N>,
}

impl<N> NandVolumeFactory<N> {
    pub fn new(nand: Arc<N>) -> Self {
        Self { nand }
    }
}

impl<N> ResourceFactory<dyn Volume> for NandVolumeFactory<N>
where
    N: VirtualNand + Send + Sync + 'static,
{
    fn materialize(&self) -> VfsResult<Materialized<dyn Volume>> {
        Ok(Materialized {
            backend: Arc::new(NandVolume {
                nand: self.nand.clone(),
            }),
            lease: Arc::new(crate::NoopResourceLease),
        })
    }
}

struct NandVolume<N> {
    nand: Arc<N>,
}

impl<N> Volume for NandVolume<N>
where
    N: VirtualNand + Send + Sync + 'static,
{
    fn root(&self) -> VfsResult<Arc<dyn DirectoryNode>> {
        Ok(Arc::new(NandDirectory {
            nand: self.nand.clone(),
            path: String::new(),
        }))
    }

    fn rename(&self, source: &VfsPath, target: &VfsPath) -> VfsResult<()> {
        let source = storage_path(source);
        let target = storage_path(target);
        require_write(self.nand.as_ref(), &source)?;
        require_write(self.nand.as_ref(), &target)?;

        self.nand
            .root()
            .rename(&source, &target)
            .map_err(|error| nand_error(error, &source))
    }
}

fn readable(nand: &impl VirtualNand, path: &str) -> bool {
    nand.permissions(path).can_read()
}

fn require_read(nand: &impl VirtualNand, path: &str) -> VfsResult<()> {
    match readable(nand, path) {
        true => Ok(()),
        false => Err(VfsError::PermissionDenied(vfs_path(path))),
    }
}

fn require_write(nand: &impl VirtualNand, path: &str) -> VfsResult<()> {
    match nand.permissions(path).can_write() {
        true => Ok(()),
        false => Err(VfsError::PermissionDenied(vfs_path(path))),
    }
}

fn node<N>(nand: &Arc<N>, path: String, kind: EntryKind) -> Node
where
    N: VirtualNand + Send + Sync + 'static,
{
    match kind {
        EntryKind::Directory => Node::Directory(Arc::new(NandDirectory {
            nand: nand.clone(),
            path,
        })),
        EntryKind::File => Node::File(Arc::new(NandFile {
            nand: nand.clone(),
            path,
        })),
    }
}

struct NandDirectory<N> {
    nand: Arc<N>,
    path: String,
}

impl<N> DirectoryNode for NandDirectory<N>
where
    N: VirtualNand + Send + Sync + 'static,
{
    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(Metadata {
            kind: FileKind::Directory,
            len: 0,
            readonly: !self.nand.permissions(&self.path).can_write(),
        })
    }

    fn set_metadata(&self, _changes: MetadataChanges) -> VfsResult<()> {
        require_write(self.nand.as_ref(), &self.path)
    }

    fn lookup(&self, name: &VfsComponent) -> VfsResult<Option<Node>> {
        let path = child(&self.path, &name.to_string_lossy());
        if !readable(self.nand.as_ref(), &path) {
            return Ok(None);
        }

        match self.nand.root().metadata(&path) {
            Ok(metadata) => Ok(Some(node(&self.nand, path, metadata.kind))),
            Err(_) => Ok(None),
        }
    }

    fn read_dir(&self) -> VfsResult<Box<DirectoryIterator>> {
        require_read(self.nand.as_ref(), &self.path)?;

        let storage = self.nand.root();
        let listing = storage
            .read_dir(&self.path, false)
            .map_err(|error| nand_error(error, &self.path))?;

        let mut entries = Vec::new();
        for entry in listing {
            if !readable(self.nand.as_ref(), &entry.path) {
                continue;
            }

            entries.push(VfsComponent::new(&entry.name).map(|name| DirectoryEntry {
                name,
                node: node(&self.nand, entry.path, entry.kind),
            }));
        }

        Ok(Box::new(entries.into_iter()))
    }

    fn create_file(
        &self,
        name: &VfsComponent,
        options: CreateOptions,
    ) -> VfsResult<Arc<dyn FileNode>> {
        let path = child(&self.path, &name.to_string_lossy());
        require_write(self.nand.as_ref(), &path)?;

        let storage = self.nand.root();
        let present = storage.exists(&path);
        if present && options.exclusive {
            return Err(VfsError::AlreadyExists(vfs_path(&path)));
        }

        if !present {
            storage
                .create(&path)
                .map_err(|error| nand_error(error, &path))?;
        }

        Ok(Arc::new(NandFile {
            nand: self.nand.clone(),
            path,
        }))
    }

    fn create_dir(&self, name: &VfsComponent) -> VfsResult<Arc<dyn DirectoryNode>> {
        let path = child(&self.path, &name.to_string_lossy());
        require_write(self.nand.as_ref(), &path)?;

        let storage = self.nand.root();
        if storage.exists(&path) {
            return Err(VfsError::AlreadyExists(vfs_path(&path)));
        }

        storage
            .create_dir_all(&path)
            .map_err(|error| nand_error(error, &path))?;

        Ok(Arc::new(NandDirectory {
            nand: self.nand.clone(),
            path,
        }))
    }

    fn remove(&self, name: &VfsComponent, kind: RemoveKind) -> VfsResult<()> {
        let path = child(&self.path, &name.to_string_lossy());
        require_write(self.nand.as_ref(), &path)?;

        let storage = self.nand.root();
        let metadata = storage
            .metadata(&path)
            .map_err(|error| nand_error(error, &path))?;

        match (kind, metadata.kind) {
            (RemoveKind::File, EntryKind::Directory) => Err(VfsError::IsDirectory(vfs_path(&path))),
            (RemoveKind::Directory, EntryKind::File) => Err(VfsError::NotDirectory(vfs_path(&path))),
            _ => storage
                .remove(&path)
                .map_err(|error| nand_error(error, &path)),
        }
    }
}

struct NandFile<N> {
    nand: Arc<N>,
    path: String,
}

impl<N> FileNode for NandFile<N>
where
    N: VirtualNand + Send + Sync + 'static,
{
    fn metadata(&self) -> VfsResult<Metadata> {
        let metadata = self
            .nand
            .root()
            .metadata(&self.path)
            .map_err(|error| nand_error(error, &self.path))?;

        Ok(Metadata {
            kind: FileKind::File,
            len: metadata.len,
            readonly: !self.nand.permissions(&self.path).can_write(),
        })
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        require_write(self.nand.as_ref(), &self.path)?;

        match changes.len {
            Some(len) => self
                .nand
                .root()
                .set_len(&self.path, len)
                .map_err(|error| nand_error(error, &self.path)),
            None => Ok(()),
        }
    }

    fn open(&self, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
        require_read(self.nand.as_ref(), &self.path)?;
        let storage = self.nand.root();

        let handle = match options.write {
            true => {
                require_write(self.nand.as_ref(), &self.path)?;
                Handle::Writable(Mutex::new(
                    storage
                        .open(&self.path)
                        .map_err(|error| nand_error(error, &self.path))?,
                ))
            }
            false => Handle::ReadOnly(Mutex::new(
                storage
                    .open_read(&self.path)
                    .map_err(|error| nand_error(error, &self.path))?,
            )),
        };

        if options.truncate {
            require_write(self.nand.as_ref(), &self.path)?;
            storage
                .set_len(&self.path, 0)
                .map_err(|error| nand_error(error, &self.path))?;
        }

        Ok(Box::new(NandOpen {
            nand: self.nand.clone(),
            path: self.path.clone(),
            handle,
        }))
    }
}

enum Handle {
    ReadOnly(Mutex<Box<dyn StorageRead>>),
    Writable(Mutex<Box<dyn StorageFile>>),
}

struct NandOpen<N> {
    nand: Arc<N>,
    path: String,
    handle: Handle,
}

impl<N> OpenFile for NandOpen<N>
where
    N: VirtualNand + Send + Sync + 'static,
{
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> VfsResult<usize> {
        use std::io::{Read, Seek, SeekFrom};

        match &self.handle {
            Handle::ReadOnly(file) => {
                let mut file = file.lock();
                file.seek(SeekFrom::Start(offset))?;
                Ok(file.read(buffer)?)
            }
            Handle::Writable(file) => {
                let mut file = file.lock();
                file.seek(SeekFrom::Start(offset))?;
                Ok(file.read(buffer)?)
            }
        }
    }

    fn write_at(&self, offset: u64, buffer: &[u8]) -> VfsResult<usize> {
        use std::io::{Seek, SeekFrom, Write};

        let Handle::Writable(file) = &self.handle else {
            return Err(VfsError::PermissionDenied(vfs_path(&self.path)));
        };

        let mut file = file.lock();
        file.seek(SeekFrom::Start(offset))?;
        Ok(file.write(buffer)?)
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        require_write(self.nand.as_ref(), &self.path)?;
        self.nand
            .root()
            .set_len(&self.path, len)
            .map_err(|error| nand_error(error, &self.path))
    }

    fn flush(&self) -> VfsResult<()> {
        use std::io::Write;

        match &self.handle {
            Handle::ReadOnly(_) => Ok(()),
            Handle::Writable(file) => Ok(file.lock().flush()?),
        }
    }

    fn sync(&self) -> VfsResult<()> {
        self.flush()?;
        self.nand
            .root()
            .sync(&self.path)
            .map_err(|error| nand_error(error, &self.path))
    }
}

fn child(parent: &str, name: &str) -> String {
    match parent.is_empty() {
        true => name.to_owned(),
        false => format!("{parent}/{name}"),
    }
}

fn storage_path(path: &VfsPath) -> String {
    path.to_string_lossy().trim_start_matches('/').to_owned()
}

fn vfs_path(path: &str) -> VfsPathBuf {
    VfsPathBuf::new(format!("/{path}")).unwrap_or_else(|_| VfsPathBuf::root())
}

fn nand_error(error: NandError, path: &str) -> VfsError {
    match error {
        NandError::InvalidPath(path) => VfsError::InvalidPath {
            path,
            reason: "outside the volume".to_owned(),
        },
        NandError::Io { source, .. } => match source.kind() {
            std::io::ErrorKind::NotFound => VfsError::NotFound(vfs_path(path)),
            std::io::ErrorKind::PermissionDenied => VfsError::PermissionDenied(vfs_path(path)),
            std::io::ErrorKind::AlreadyExists => VfsError::AlreadyExists(vfs_path(path)),
            std::io::ErrorKind::DirectoryNotEmpty => VfsError::DirectoryNotEmpty(vfs_path(path)),
            _ => VfsError::Io(source),
        },
        other => VfsError::Unsupported(other.to_string()),
    }
}
