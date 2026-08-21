use crate::{
    CreateOptions, DirectoryEntry, DirectoryIterator, DirectoryNode, FileKind, FileNode, Metadata,
    MetadataChanges, Node, OpenFile, OpenOptions, RemoveKind, VfsComponent, VfsError, VfsPath,
    VfsPathBuf, VfsResult, Volume,
};
use std::{
    ffi::OsString,
    fs::{self, File},
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) struct HostFile {
    path: PathBuf,
    writable: bool,
}

impl HostFile {
    pub(super) fn new(path: &Path, writable: bool) -> VfsResult<Self> {
        fs::OpenOptions::new()
            .read(true)
            .write(writable)
            .open(path)?;
        Ok(Self {
            path: path.to_owned(),
            writable,
        })
    }
}

impl FileNode for HostFile {
    fn metadata(&self) -> VfsResult<Metadata> {
        file_metadata(&fs::metadata(&self.path)?, self.writable)
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        set_host_metadata(&self.path, self.writable, changes, false)
    }

    fn open(&self, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
        open_path(&self.path, self.writable, options)
    }
}

struct HostOpen {
    file: File,
}

impl OpenFile for HostOpen {
    fn read(&self, offset: u64, buffer: &mut [u8]) -> VfsResult<usize> {
        use std::os::unix::fs::FileExt;
        Ok(self.file.read_at(buffer, offset)?)
    }

    fn write(&self, offset: u64, buffer: &[u8]) -> VfsResult<usize> {
        use std::os::unix::fs::FileExt;
        Ok(self.file.write_at(buffer, offset)?)
    }

    fn truncate(&self, len: u64) -> VfsResult<()> {
        self.file.set_len(len)?;
        Ok(())
    }

    fn flush(&self) -> VfsResult<()> {
        Ok(())
    }

    fn sync(&self) -> VfsResult<()> {
        self.file.sync_all()?;
        Ok(())
    }
}

pub(super) struct HostVolume {
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
    handle: Arc<File>,
    writable: bool,
}

impl HostVolume {
    pub(super) fn new(root: &Path, writable: bool) -> VfsResult<Self> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(VfsError::NotDirectory(VfsPathBuf::root()));
        }
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
            host.push(component_os_string(&component));
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

    fn child(&self, parent: &Path, name: &VfsComponent) -> VfsResult<PathBuf> {
        if !parent.is_dir() {
            return Err(VfsError::NotDirectory(VfsPathBuf::root()));
        }
        Ok(parent.join(component_os_string(name)))
    }

    fn node(self: &Arc<Self>, path: PathBuf) -> VfsResult<Node> {
        let metadata = fs::metadata(&path)?;
        if metadata.is_dir() {
            Ok(Node::Directory(Arc::new(HostDirectory {
                volume: self.clone(),
                path,
            })))
        } else {
            let handle = fs::OpenOptions::new()
                .read(true)
                .write(self.writable)
                .open(path)?;
            Ok(Node::File(Arc::new(HostTreeFile {
                handle: Arc::new(handle),
                writable: self.writable,
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
        self.rename_inner(source, target, false)
    }

    fn replace(&self, source: &VfsPath, target: &VfsPath) -> VfsResult<()> {
        self.rename_inner(source, target, true)
    }

    fn detach(&self, source: &VfsPath) -> VfsResult<()> {
        VfsError::require_writable(self.inner.writable)?;
        let source = self
            .inner
            .confined_existing(Self::host_path(&self.inner, source)?)?;
        if !source.is_file() {
            return Err(VfsError::IsDirectory(VfsPathBuf::root()));
        }
        fs::remove_file(source)?;
        Ok(())
    }
}

impl HostVolume {
    fn rename_inner(
        &self,
        source: &VfsPath,
        target: &VfsPath,
        replace_if_exists: bool,
    ) -> VfsResult<()> {
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
        if !replace_if_exists && target.exists() {
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
        let handle = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .create_new(options.exclusive)
            .truncate(false)
            .open(&path)?;
        self.volume.confined_existing(path)?;
        Ok(Arc::new(HostTreeFile {
            handle: Arc::new(handle),
            writable: self.volume.writable,
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
        file_metadata(&self.handle.metadata()?, self.writable)
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        VfsError::require_writable(self.writable)?;
        if let Some(len) = changes.len {
            self.handle.set_len(len)?;
        }
        if let Some(readonly) = changes.readonly {
            let mut permissions = self.handle.metadata()?.permissions();
            permissions.set_readonly(readonly);
            self.handle.set_permissions(permissions)?;
        }
        Ok(())
    }

    fn open(&self, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
        if options.write || options.truncate {
            VfsError::require_writable(self.writable)?;
        }
        let file = self.handle.try_clone()?;
        if options.truncate {
            file.set_len(0)?;
        }
        Ok(Box::new(HostOpen { file }))
    }
}

fn open_path(path: &Path, writable: bool, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
    if options.write || options.truncate {
        VfsError::require_writable(writable)?;
    }
    let file = fs::OpenOptions::new()
        .read(options.read || !options.write)
        .write(options.write)
        .truncate(options.truncate)
        .open(path)?;
    Ok(Box::new(HostOpen { file }))
}

fn file_metadata(metadata: &fs::Metadata, writable: bool) -> VfsResult<Metadata> {
    if !metadata.is_file() {
        return Err(VfsError::Unsupported(
            "host volume exposes only files and directories".into(),
        ));
    }
    Ok(Metadata {
        kind: FileKind::File,
        len: metadata.len(),
        readonly: !writable,
    })
}

fn set_host_metadata(
    path: &Path,
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

fn component_os_string(component: &VfsComponent) -> OsString {
    OsString::from_vec(component.as_slice().to_vec())
}

fn component_from_os(value: OsString) -> VfsResult<VfsComponent> {
    VfsComponent::from_units(value.into_vec())
}
