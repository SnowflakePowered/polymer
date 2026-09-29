use super::HostOpen;
use crate::{
    CreateOptions, DirectoryEntry, DirectoryIterator, DirectoryNode, FileKind, FileNode, Metadata,
    MetadataChanges, Node, OpenFile, OpenOptions, RemoveKind, VfsComponent, VfsError, VfsPath,
    VfsPathBuf, VfsResult, Volume,
};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions as DirOpenOptions},
};
use std::{
    ffi::OsString,
    fs::{self, File},
    io,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) struct HostFile {
    handle: Arc<File>,
    writable: bool,
}

impl HostFile {
    pub(super) fn new(path: &Path, writable: bool) -> VfsResult<Self> {
        let handle = fs::OpenOptions::new()
            .read(true)
            .write(writable)
            .open(path)?;
        metadata_for(&handle, writable)?;
        Ok(Self {
            handle: Arc::new(handle),
            writable,
        })
    }
}

impl FileNode for HostFile {
    fn metadata(&self) -> VfsResult<Metadata> {
        metadata_for(&self.handle, self.writable)
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
        Ok(Box::new(HostOpen {
            file,
            writable: self.writable,
        }))
    }
}

pub(super) struct HostVolume {
    inner: Arc<HostVolumeInner>,
}

struct HostVolumeInner {
    root: Dir,
    internal_links: bool,
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
    pub(super) fn new(root: &Path, writable: bool, internal_links: bool) -> VfsResult<Self> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(VfsError::NotDirectory(VfsPathBuf::root()));
        }
        Ok(Self {
            inner: Arc::new(HostVolumeInner {
                root: Dir::open_ambient_dir(&root, ambient_authority())?,
                writable,
                internal_links,
            }),
        })
    }

    fn component(path: &VfsPath) -> VfsResult<VfsComponent> {
        path.components()
            .next_back()
            .map(|name| VfsComponent::from_units(crate::PathStr::as_slice(name).to_vec()))
            .transpose()?
            .ok_or_else(|| VfsError::Unsupported("the volume root has no name".into()))
    }

    fn relative(path: &VfsPath) -> VfsResult<PathBuf> {
        let mut relative = PathBuf::from(".");
        for component in path.components() {
            let component = VfsComponent::from_units(crate::PathStr::as_slice(component).to_vec())?;
            relative.push(component_os_string(&component));
        }
        Ok(relative)
    }
}

impl HostVolumeInner {
    fn confined(&self, path: &Path) -> VfsResult<cap_std::fs::Metadata> {
        let metadata = self.root.symlink_metadata(path)?;
        if !metadata.file_type().is_symlink() {
            return Ok(metadata);
        }
        if !self.internal_links {
            return Err(VfsError::Unsupported(
                "links are not exposed by confined host volumes".into(),
            ));
        }
        Ok(self.root.metadata(path)?)
    }

    fn node(self: &Arc<Self>, path: PathBuf) -> VfsResult<Node> {
        let metadata = self.confined(&path)?;
        if metadata.is_dir() {
            Ok(Node::Directory(Arc::new(HostDirectory {
                volume: self.clone(),
                path,
            })))
        } else if !self.writable {
            // Cached package metadata must not pin one descriptor per file.
            // Read-only trees cannot rename/unlink nodes through this volume;
            // acquire their descriptors only for the lifetime of an open file.
            Ok(Node::File(Arc::new(HostTreeFile {
                volume: self.clone(),
                path,
            })))
        } else {
            let handle = self
                .root
                .open_with(&path, DirOpenOptions::new().read(true).write(true))?
                .into_std();
            Ok(Node::File(Arc::new(HostFile {
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
            path: PathBuf::from("."),
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
        let source = Self::relative(source)?;
        if !self.inner.confined(&source)?.is_file() {
            return Err(VfsError::IsDirectory(VfsPathBuf::root()));
        }
        self.inner.root.remove_file(source)?;
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
        let source = Self::relative(source)?;
        self.inner.confined(&source)?;
        let target_parent = target
            .parent()
            .ok_or_else(|| VfsError::Unsupported("the volume root cannot be replaced".into()))?;
        let target_parent = Self::relative(target_parent.as_path())?;
        if !self.inner.confined(&target_parent)?.is_dir() {
            return Err(VfsError::NotDirectory(VfsPathBuf::root()));
        }
        let target = target_parent.join(component_os_string(&Self::component(target)?));
        if !replace_if_exists && self.inner.root.exists(&target) {
            return Err(VfsError::AlreadyExists(VfsPathBuf::root()));
        }
        self.inner.root.rename(source, &self.inner.root, target)?;
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
        VfsError::require_writable(self.volume.writable)?;
        if changes.len.is_some() {
            return Err(VfsError::IsDirectory(VfsPathBuf::root()));
        }
        if let Some(readonly) = changes.readonly {
            let mut permissions = self.volume.root.metadata(&self.path)?.permissions();
            permissions.set_readonly(readonly);
            self.volume.root.set_permissions(&self.path, permissions)?;
        }
        Ok(())
    }

    fn lookup(&self, name: &VfsComponent) -> VfsResult<Option<Node>> {
        let child = self.path.join(component_os_string(name));
        match self.volume.root.symlink_metadata(&child) {
            Ok(_) => self.volume.node(child).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn read_dir(&self) -> VfsResult<Box<DirectoryIterator>> {
        let mut entries = Vec::new();
        for entry in self.volume.root.read_dir(&self.path)? {
            let name = entry?.file_name();
            let metadata = self.volume.confined(&self.path.join(&name))?;
            entries.push(Ok(DirectoryEntry {
                name: component_from_os(name)?,
                metadata: Metadata {
                    kind: if metadata.is_dir() {
                        FileKind::Directory
                    } else if metadata.is_file() {
                        FileKind::File
                    } else {
                        return Err(VfsError::Unsupported(
                            "host volume exposes only files and directories".into(),
                        ));
                    },
                    len: metadata.len(),
                    readonly: !self.volume.writable,
                },
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
        let path = self.path.join(component_os_string(name));
        let handle = self.volume.root.open_with(
            &path,
            DirOpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .create_new(options.exclusive)
                .truncate(false),
        )?;
        self.volume.confined(&path)?;
        Ok(Arc::new(HostFile {
            handle: Arc::new(handle.into_std()),
            writable: self.volume.writable,
        }))
    }

    fn create_dir(&self, name: &VfsComponent) -> VfsResult<Arc<dyn DirectoryNode>> {
        VfsError::require_writable(self.volume.writable)?;
        let path = self.path.join(component_os_string(name));
        self.volume.root.create_dir(&path)?;
        self.volume.confined(&path)?;
        Ok(Arc::new(HostDirectory {
            volume: self.volume.clone(),
            path,
        }))
    }

    fn remove(&self, name: &VfsComponent, kind: RemoveKind) -> VfsResult<()> {
        VfsError::require_writable(self.volume.writable)?;
        let path = self.path.join(component_os_string(name));
        self.volume.confined(&path)?;
        match kind {
            RemoveKind::File => self.volume.root.remove_file(path)?,
            RemoveKind::Directory => self.volume.root.remove_dir(path)?,
        }
        Ok(())
    }
}

impl FileNode for HostTreeFile {
    fn metadata(&self) -> VfsResult<Metadata> {
        let metadata = self.volume.confined(&self.path)?;
        if !metadata.is_file() {
            return Err(VfsError::Unsupported(
                "host volume exposes only files and directories".into(),
            ));
        }
        Ok(Metadata {
            kind: FileKind::File,
            len: metadata.len(),
            readonly: true,
        })
    }

    fn set_metadata(&self, _changes: MetadataChanges) -> VfsResult<()> {
        VfsError::require_writable(false)
    }

    fn open(&self, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
        if options.write || options.truncate {
            VfsError::require_writable(false)?;
        }
        self.volume.confined(&self.path)?;
        Ok(Box::new(HostOpen {
            file: self.volume.root.open(&self.path)?.into_std(),
            writable: false,
        }))
    }
}

pub(super) fn metadata_for(file: &File, writable: bool) -> VfsResult<Metadata> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(VfsError::Unsupported(
            "host volume exposes only files and directories".into(),
        ));
    }
    Ok(Metadata {
        kind: FileKind::File,
        len: metadata.len(),
        readonly: !writable || metadata.permissions().readonly(),
    })
}

fn component_os_string(component: &VfsComponent) -> OsString {
    OsString::from_vec(component.as_slice().to_vec())
}

fn component_from_os(value: OsString) -> VfsResult<VfsComponent> {
    VfsComponent::from_units(value.into_vec())
}
