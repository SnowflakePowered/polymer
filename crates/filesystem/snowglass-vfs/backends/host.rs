use crate::{
    CreateOptions, DirectoryEntry, DirectoryIterator, DirectoryNode, FileKind, FileNode,
    FinalizationReason, Materialized, Metadata, MetadataChanges, Node, OpenFile, OpenOptions,
    RemoveKind, ResourceFactory, ResourceLease, VfsComponent, VfsError, VfsPath, VfsPathBuf,
    VfsResult, Volume,
};
use parking_lot::Mutex;
use std::{
    ffi::OsString,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::Arc,
};

pub struct HostFileFactory {
    path: PathBuf,
    writable: bool,
}

impl HostFileFactory {
    pub fn new(path: PathBuf, writable: bool) -> Self {
        Self { path, writable }
    }
}

impl ResourceFactory<dyn FileNode> for HostFileFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn FileNode>> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(self.writable)
            .open(&self.path)?;
        Ok(Materialized {
            backend: Arc::new(HostFile {
                file: Arc::new(Mutex::new(file)),
                writable: self.writable,
            }),
            lease: Arc::new(crate::NoopResourceLease),
        })
    }
}

struct HostFile {
    file: Arc<Mutex<File>>,
    writable: bool,
}

impl FileNode for HostFile {
    fn metadata(&self) -> VfsResult<Metadata> {
        let metadata = self.file.lock().metadata()?;
        Ok(Metadata {
            kind: FileKind::File,
            len: metadata.len(),
            readonly: !self.writable,
        })
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        VfsError::require_writable(self.writable)?;
        if let Some(len) = changes.len {
            self.file.lock().set_len(len)?;
        }
        Ok(())
    }

    fn open(&self, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
        if options.write || options.truncate {
            VfsError::require_writable(self.writable)?;
        }
        if options.truncate {
            self.file.lock().set_len(0)?;
        }
        Ok(Box::new(HostOpen {
            file: self.file.clone(),
            writable: self.writable,
        }))
    }
}

struct HostOpen {
    file: Arc<Mutex<File>>,
    writable: bool,
}

impl OpenFile for HostOpen {
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> VfsResult<usize> {
        let mut file = self.file.lock();
        file.seek(SeekFrom::Start(offset))?;
        Ok(file.read(buffer)?)
    }

    fn write_at(&self, offset: u64, buffer: &[u8]) -> VfsResult<usize> {
        VfsError::require_writable(self.writable)?;
        let mut file = self.file.lock();
        file.seek(SeekFrom::Start(offset))?;
        Ok(file.write(buffer)?)
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        VfsError::require_writable(self.writable)?;
        self.file.lock().set_len(len)?;
        Ok(())
    }

    fn flush(&self) -> VfsResult<()> {
        self.file.lock().flush()?;
        Ok(())
    }

    fn sync(&self) -> VfsResult<()> {
        self.file.lock().sync_all()?;
        Ok(())
    }
}

pub struct PackageDirectoryFactory {
    root: PathBuf,
}

impl PackageDirectoryFactory {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl ResourceFactory<dyn Volume> for PackageDirectoryFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn Volume>> {
        let root = self.root.canonicalize()?;
        if !root.is_dir() {
            return Err(VfsError::NotDirectory(VfsPathBuf::root()));
        }
        Ok(Materialized {
            backend: Arc::new(HostVolume::new(root, false)?),
            lease: Arc::new(crate::NoopResourceLease),
        })
    }
}

pub(super) fn temporary_volume() -> VfsResult<Materialized<dyn Volume>> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    Ok(Materialized {
        backend: Arc::new(HostVolume::new(root, true)?),
        lease: Arc::new(TemporaryLease(Mutex::new(Some(directory)))),
    })
}

struct TemporaryLease(Mutex<Option<tempfile::TempDir>>);

impl ResourceLease for TemporaryLease {
    fn finalize(&self, _reason: FinalizationReason) {
        self.0.lock().take();
    }
}

struct HostVolume {
    inner: Arc<HostVolumeInner>,
}

struct HostVolumeInner {
    root: PathBuf,
    writable: bool,
}

struct HostDirectory {
    volume: Arc<HostVolumeInner>,
    path: PathBuf,
}

struct HostTreeFile {
    volume: Arc<HostVolumeInner>,
    path: PathBuf,
}

impl HostVolume {
    fn new(root: PathBuf, writable: bool) -> VfsResult<Self> {
        let metadata = fs::symlink_metadata(&root)?;
        if metadata.file_type().is_symlink() {
            return Err(VfsError::Unsupported("volume root cannot be a link".into()));
        }
        Ok(Self {
            inner: Arc::new(HostVolumeInner { root, writable }),
        })
    }

    fn component(path: &VfsPath) -> VfsResult<VfsComponent> {
        path.components()
            .next_back()
            .map(|name| VfsComponent::from_units(crate::PathStr::as_slice(name).to_vec()))
            .transpose()?
            .ok_or_else(|| VfsError::Unsupported("the volume root has no name".into()))
    }

    fn host_path(inner: &HostVolumeInner, path: &VfsPath) -> VfsResult<PathBuf> {
        let mut host = inner.root.clone();
        for component in path.components() {
            let component = VfsComponent::from_units(crate::PathStr::as_slice(component).to_vec())?;
            host.push(component_os_string(&component)?);
        }
        Ok(host)
    }
}

impl HostVolumeInner {
    fn confined_existing(&self, path: PathBuf) -> VfsResult<PathBuf> {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(VfsError::Unsupported(
                "links are not exposed by confined host volumes".into(),
            ));
        }
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(&self.root) {
            return Err(VfsError::Unsupported(
                "host path escapes confined volume".into(),
            ));
        }
        Ok(canonical)
    }

    fn child(&self, parent: &PathBuf, name: &VfsComponent) -> VfsResult<PathBuf> {
        if !parent.is_dir() {
            return Err(VfsError::NotDirectory(VfsPathBuf::root()));
        }
        Ok(parent.join(component_os_string(name)?))
    }

    fn node(self: &Arc<Self>, path: PathBuf) -> VfsResult<Node> {
        let metadata = fs::metadata(&path)?;
        if metadata.is_dir() {
            Ok(Node::Directory(Arc::new(HostDirectory {
                volume: self.clone(),
                path,
            })))
        } else {
            Ok(Node::File(Arc::new(HostTreeFile {
                volume: self.clone(),
                path,
            })))
        }
    }
}

impl Volume for HostVolume {
    fn root(&self) -> VfsResult<Arc<dyn DirectoryNode>> {
        Ok(Arc::new(HostDirectory {
            volume: self.inner.clone(),
            path: self.inner.root.clone(),
        }))
    }

    fn rename(&self, source: &VfsPath, target: &VfsPath) -> VfsResult<()> {
        VfsError::require_writable(self.inner.writable)?;
        let source = self
            .inner
            .confined_existing(Self::host_path(&self.inner, source)?)?;
        let target_parent = target
            .parent()
            .ok_or_else(|| VfsError::Unsupported("the volume root cannot be replaced".into()))?;
        let target_parent = self
            .inner
            .confined_existing(Self::host_path(&self.inner, target_parent.as_path())?)?;
        let target = self
            .inner
            .child(&target_parent, &Self::component(target)?)?;
        if target.exists() {
            return Err(VfsError::AlreadyExists(VfsPathBuf::root()));
        }
        fs::rename(source, target)?;
        Ok(())
    }
}

impl DirectoryNode for HostDirectory {
    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(Metadata {
            kind: FileKind::Directory,
            len: 0,
            readonly: !self.volume.writable,
        })
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        set_host_metadata(&self.path, self.volume.writable, changes, true)
    }

    fn lookup(&self, name: &VfsComponent) -> VfsResult<Option<Node>> {
        let child = self.volume.child(&self.path, name)?;
        if !child.exists() {
            return Ok(None);
        }
        let child = self.volume.confined_existing(child)?;
        self.volume.node(child).map(Some)
    }

    fn read_dir(&self) -> VfsResult<Box<DirectoryIterator>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(&self.path)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(VfsError::Unsupported(
                    "links are not exposed by confined host volumes".into(),
                ));
            }
            entries.push(Ok(DirectoryEntry {
                name: component_from_os(entry.file_name())?,
                node: self.volume.node(entry.path())?,
            }));
        }
        Ok(Box::new(entries.into_iter()))
    }

    fn create_file(
        &self,
        name: &VfsComponent,
        options: CreateOptions,
    ) -> VfsResult<Arc<dyn FileNode>> {
        VfsError::require_writable(self.volume.writable)?;
        let path = self.volume.child(&self.path, name)?;
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .create_new(options.exclusive)
            .open(&path)?;
        let path = self.volume.confined_existing(path)?;
        Ok(Arc::new(HostTreeFile {
            volume: self.volume.clone(),
            path,
        }))
    }

    fn create_dir(&self, name: &VfsComponent) -> VfsResult<Arc<dyn DirectoryNode>> {
        VfsError::require_writable(self.volume.writable)?;
        let path = self.volume.child(&self.path, name)?;
        fs::create_dir(&path)?;
        let path = self.volume.confined_existing(path)?;
        Ok(Arc::new(HostDirectory {
            volume: self.volume.clone(),
            path,
        }))
    }

    fn remove(&self, name: &VfsComponent, kind: RemoveKind) -> VfsResult<()> {
        VfsError::require_writable(self.volume.writable)?;
        let path = self
            .volume
            .confined_existing(self.volume.child(&self.path, name)?)?;
        match kind {
            RemoveKind::File => fs::remove_file(path)?,
            RemoveKind::Directory => fs::remove_dir(path)?,
        }
        Ok(())
    }
}

impl FileNode for HostTreeFile {
    fn metadata(&self) -> VfsResult<Metadata> {
        let metadata = fs::metadata(&self.path)?;
        Ok(Metadata {
            kind: FileKind::File,
            len: metadata.len(),
            readonly: !self.volume.writable,
        })
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        set_host_metadata(&self.path, self.volume.writable, changes, false)
    }

    fn open(&self, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
        if options.write || options.truncate {
            VfsError::require_writable(self.volume.writable)?;
        }
        let file = fs::OpenOptions::new()
            .read(options.read || !options.write)
            .write(options.write)
            .truncate(options.truncate)
            .open(&self.path)?;
        Ok(Box::new(HostOpen {
            file: Arc::new(Mutex::new(file)),
            writable: self.volume.writable && options.write,
        }))
    }
}

fn set_host_metadata(
    path: &PathBuf,
    writable: bool,
    changes: MetadataChanges,
    directory: bool,
) -> VfsResult<()> {
    VfsError::require_writable(writable)?;
    if let Some(len) = changes.len {
        if directory {
            return Err(VfsError::IsDirectory(VfsPathBuf::root()));
        }
        fs::OpenOptions::new()
            .write(true)
            .open(path)?
            .set_len(len)?;
    }
    if let Some(readonly) = changes.readonly {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_readonly(readonly);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(windows)]
fn component_os_string(component: &VfsComponent) -> VfsResult<OsString> {
    use std::os::windows::ffi::OsStringExt;
    Ok(OsString::from_wide(component.as_slice()))
}

#[cfg(unix)]
fn component_os_string(component: &VfsComponent) -> VfsResult<OsString> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(component.as_slice().to_vec()))
}

#[cfg(windows)]
fn component_from_os(value: OsString) -> VfsResult<VfsComponent> {
    use std::os::windows::ffi::OsStrExt;
    VfsComponent::from_units(value.as_os_str().encode_wide().collect())
}

#[cfg(unix)]
fn component_from_os(value: OsString) -> VfsResult<VfsComponent> {
    use std::os::unix::ffi::OsStringExt;
    VfsComponent::from_units(value.into_vec())
}
