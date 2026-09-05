//! Filesystem composition capabilities owned by plugin-worker isolates.

mod ops;

use deno_core::{JsRuntime, OpState, op2};
use deno_error::JsErrorBox;
use polymer_vfs::{
    AsarVolumeFactory, DirectoryNode, FileKind, FileNode, FilesystemEndpoint, FilesystemPlan,
    HostDirectoryFactory, HostFileFactory, Materialized, MemoryFileFactory, Node, OpenOptions,
    PackageDirectoryFactory, Permissions, PreparedFilesystemLaunch, ProfileLayout, RemoveKind,
    Resource, ResourceFactory, ResourceLease, ResourceOrigin, ScratchBacking, ScratchVolumeFactory,
    VfsComponent, VfsError, VfsPathBuf, Volume,
};
use serde::Serialize;
use serde_bytes::ByteBuf;
use std::{
    collections::{BTreeMap, HashMap},
    path::{Component as HostComponent, Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use crate::{emulator::QuickResumeData, handle::Handle};

#[derive(Clone, Debug)]
pub struct NamedCache {
    pub key: String,
    pub path: PathBuf,
}

pub type StorageFactories = BTreeMap<String, Arc<dyn ResourceFactory<dyn Volume>>>;

type VolumeResource = Resource<dyn Volume>;
type FileResource = Resource<dyn FileNode>;
type MaterializedVolume = (Arc<dyn Volume>, Arc<dyn ResourceLease>);

#[derive(Clone)]
pub struct FilesystemOptions {
    pub package_root: PathBuf,
    pub scratch_root: Option<PathBuf>,
    pub caches: Vec<NamedCache>,
    pub storage: StorageFactories,
    pub quick_resume: Option<QuickResumeData>,
}

impl std::fmt::Debug for FilesystemOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FilesystemOptions")
            .field("package_root", &self.package_root)
            .field("caches", &self.caches)
            .field("storage", &self.storage.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl FilesystemOptions {
    pub fn new(package_root: PathBuf) -> Self {
        Self {
            package_root,
            scratch_root: None,
            caches: Vec::new(),
            storage: BTreeMap::new(),
            quick_resume: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ResourceHandle {
    Tree(Handle<VolumeResource>),
    File(Handle<FileResource>),
}

#[derive(Serialize)]
struct JsDirectoryEntry {
    name: String,
    kind: &'static str,
}

fn invalid_handle() -> VfsError {
    VfsError::Unsupported("unknown or stale JavaScript resource handle".into())
}

struct CacheResource {
    path: PathBuf,
    handle: Option<Handle<VolumeResource>>,
}

struct SharedVolumeFactory {
    source: Arc<dyn ResourceFactory<dyn Volume>>,
    materialized: parking_lot::Mutex<Option<MaterializedVolume>>,
}

impl SharedVolumeFactory {
    fn new(source: Arc<dyn ResourceFactory<dyn Volume>>) -> Self {
        Self {
            source,
            materialized: parking_lot::Mutex::new(None),
        }
    }
}

impl ResourceFactory<dyn Volume> for SharedVolumeFactory {
    fn materialize(&self) -> Result<Materialized<dyn Volume>, VfsError> {
        let mut shared = self.materialized.lock();
        if shared.is_none() {
            let value = self.source.materialize()?;
            *shared = Some((value.backend, value.lease));
        }
        let (backend, lease) = shared.as_ref().expect("shared resource was initialized");
        Ok(Materialized {
            backend: backend.clone(),
            lease: lease.clone(),
        })
    }
}

struct FilesystemState {
    package_root: PathBuf,
    scratch_root: Option<PathBuf>,
    endpoint: FilesystemEndpoint,
    plan: FilesystemPlan,
    volumes: HashMap<Handle<VolumeResource>, Arc<VolumeResource>>,
    files: HashMap<Handle<FileResource>, Arc<FileResource>>,
    storage: BTreeMap<String, ResourceHandle>,
    caches: BTreeMap<String, CacheResource>,
    used: bool,
    working_directory_set: bool,
    profile_set: bool,
    prepared: Option<Arc<polymer_vfs::PreparedFilesystem>>,
    staged_files: Vec<(VfsPathBuf, Vec<u8>)>,
    quick_resume: Option<(ResourceHandle, Option<ResourceHandle>)>,
}

impl FilesystemState {
    fn new(options: FilesystemOptions) -> Result<Self, VfsError> {
        let package_root = options.package_root.canonicalize()?;
        let endpoint = FilesystemEndpoint::random();
        let mut state = Self {
            package_root,
            scratch_root: options.scratch_root,
            endpoint,
            plan: FilesystemPlan::default(),
            volumes: HashMap::new(),
            files: HashMap::new(),
            storage: BTreeMap::new(),
            caches: BTreeMap::new(),
            used: false,
            working_directory_set: false,
            profile_set: false,
            prepared: None,
            staged_files: Vec::new(),
            quick_resume: None,
        };
        for cache in options.caches {
            if !cache.path.is_absolute() {
                return Err(VfsError::Unsupported(format!(
                    "cache '{}' must have an absolute host path",
                    cache.key
                )));
            }
            if state
                .caches
                .insert(
                    cache.key.clone(),
                    CacheResource {
                        path: cache.path,
                        handle: None,
                    },
                )
                .is_some()
            {
                return Err(VfsError::Unsupported(format!(
                    "duplicate declared cache '{}'",
                    cache.key
                )));
            }
        }
        for (name, factory) in options.storage {
            let origin = ResourceOrigin::Other(format!("storage:{name}"));
            let handle = ResourceHandle::Tree(state.insert_volume(Resource::new(
                factory,
                Permissions::ALL,
                origin,
            )));
            state.storage.insert(name, handle);
        }
        if let Some(quick_resume) = options.quick_resume {
            let state_file = ResourceHandle::File(state.insert_file(Resource::new(
                Arc::new(MemoryFileFactory::immutable(quick_resume.state)),
                Permissions::READ,
                ResourceOrigin::Memory,
            )));
            let screenshot = quick_resume.screenshot.map(|bytes| {
                ResourceHandle::File(state.insert_file(Resource::new(
                    Arc::new(MemoryFileFactory::immutable(bytes)),
                    Permissions::READ,
                    ResourceOrigin::Memory,
                )))
            });
            state.quick_resume = Some((state_file, screenshot));
        }
        Ok(state)
    }

    fn cache(&mut self, key: &str) -> Result<ResourceHandle, VfsError> {
        let cache = self.caches.get(key).ok_or_else(|| {
            VfsError::Unsupported(format!("cache '{key}' is not declared by this plugin"))
        })?;
        if let Some(handle) = cache.handle {
            return Ok(ResourceHandle::Tree(handle));
        }

        let path = cache.path.clone();
        std::fs::create_dir_all(&path)?;
        let handle = self.insert_volume(Resource::new(
            Arc::new(HostDirectoryFactory::new(path, true)),
            Permissions::ALL,
            ResourceOrigin::Cache(key.to_owned()),
        ));
        self.caches
            .get_mut(key)
            .expect("cache was validated above")
            .handle = Some(handle);
        Ok(ResourceHandle::Tree(handle))
    }

    fn insert_path_resource(&mut self, path: PathBuf) -> Result<ResourceHandle, VfsError> {
        let permissions = Permissions::from_str("rx").expect("canonical package permissions");
        if path.is_dir() {
            return Ok(ResourceHandle::Tree(self.insert_volume(Resource::new(
                Arc::new(PackageDirectoryFactory::new(path.clone())),
                permissions,
                ResourceOrigin::Package(path.display().to_string()),
            ))));
        }
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("asar"))
        {
            return Ok(ResourceHandle::Tree(self.insert_volume(Resource::new(
                Arc::new(AsarVolumeFactory::new(path.clone())),
                permissions,
                ResourceOrigin::Asar(path.display().to_string()),
            ))));
        }
        Ok(ResourceHandle::File(self.insert_file(Resource::new(
            Arc::new(HostFileFactory::new(path.clone(), false)),
            permissions,
            ResourceOrigin::Package(path.display().to_string()),
        ))))
    }

    fn insert_volume(
        &mut self,
        resource: Resource<dyn polymer_vfs::Volume>,
    ) -> Handle<Resource<dyn polymer_vfs::Volume>> {
        let handle = Handle::new();
        let replaced = self.volumes.insert(handle, Arc::new(resource));
        debug_assert!(replaced.is_none());
        handle
    }

    fn insert_file(
        &mut self,
        resource: Resource<dyn polymer_vfs::FileNode>,
    ) -> Handle<Resource<dyn polymer_vfs::FileNode>> {
        let handle = Handle::new();
        let replaced = self.files.insert(handle, Arc::new(resource));
        debug_assert!(replaced.is_none());
        handle
    }

    fn volume(
        &self,
        handle: Handle<Resource<dyn polymer_vfs::Volume>>,
    ) -> Result<Arc<Resource<dyn polymer_vfs::Volume>>, VfsError> {
        self.volumes
            .get(&handle)
            .cloned()
            .ok_or_else(invalid_handle)
    }

    fn file(
        &self,
        handle: Handle<Resource<dyn polymer_vfs::FileNode>>,
    ) -> Result<Arc<Resource<dyn polymer_vfs::FileNode>>, VfsError> {
        self.files.get(&handle).cloned().ok_or_else(invalid_handle)
    }

    fn package_path(&self, source: &str) -> Result<PathBuf, VfsError> {
        let relative = source.strip_prefix("pkg:").ok_or_else(|| {
            VfsError::Unsupported("filesystem source must use the pkg: scheme".into())
        })?;
        if relative.is_empty() || relative.starts_with('/') || relative.contains(['\\', '?', '#']) {
            return Err(VfsError::Unsupported(format!(
                "pkg: source must be a plain package-relative URL: {source}"
            )));
        }
        let decoded = percent_encoding::percent_decode_str(relative)
            .decode_utf8()
            .map_err(|_| {
                VfsError::Unsupported(format!("pkg: source is not valid UTF-8: {source}"))
            })?;
        if decoded.contains('\\') {
            return Err(VfsError::Unsupported(format!(
                "pkg: source must use URL separators: {source}"
            )));
        }
        let relative = Path::new(decoded.as_ref());
        if relative
            .components()
            .any(|component| !matches!(component, HostComponent::Normal(_)))
        {
            return Err(VfsError::Unsupported(format!(
                "pkg: source must contain only normal path components: {source}"
            )));
        }
        Ok(self.package_root.join(relative))
    }

    fn handle(&self, kind: &str, token: &str) -> Result<ResourceHandle, VfsError> {
        let token = u64::from_str_radix(token, 16).map_err(|_| invalid_handle())?;
        match kind {
            "tree" => {
                let handle = Handle::from_raw(token);
                self.volume(handle)?;
                Ok(ResourceHandle::Tree(handle))
            }
            "file" => {
                let handle = Handle::from_raw(token);
                self.file(handle)?;
                Ok(ResourceHandle::File(handle))
            }
            _ => Err(invalid_handle()),
        }
    }

    fn storage_handle(
        &self,
        kind: &str,
        token: &str,
    ) -> Result<Handle<Resource<dyn polymer_vfs::Volume>>, VfsError> {
        if kind != "storage" {
            return Err(invalid_handle());
        }
        let token = u64::from_str_radix(token, 16).map_err(|_| invalid_handle())?;
        let handle = Handle::from_raw(token);
        self.volume(handle)?;
        if self
            .storage
            .values()
            .any(|candidate| matches!(candidate, ResourceHandle::Tree(candidate) if *candidate == handle))
        {
            Ok(handle)
        } else {
            Err(invalid_handle())
        }
    }

    fn resource_entries(
        &self,
        kind: &str,
        token: &str,
        components: &[String],
    ) -> Result<Vec<JsDirectoryEntry>, VfsError> {
        let ResourceHandle::Tree(handle) = self.handle(kind, token)? else {
            return Err(VfsError::Unsupported(
                "directory iteration requires a tree resource".into(),
            ));
        };
        let resource = self.volume(handle)?;
        if !resource.authority().can_read() || !resource.authority().can_execute() {
            return Err(VfsError::PermissionDenied(VfsPathBuf::root()));
        }
        let materialized = resource.materialize()?;
        let directory = Self::directory_at(materialized.backend.root()?, components)?;
        let mut entries = directory
            .read_dir()?
            .map(|entry| {
                let entry = entry?;
                Ok(JsDirectoryEntry {
                    name: entry.name.to_string(),
                    kind: match entry.kind() {
                        FileKind::Directory => "directory",
                        FileKind::File => "file",
                    },
                })
            })
            .collect::<Result<Vec<_>, VfsError>>()?;
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    fn resource_bytes(
        &self,
        kind: &str,
        token: &str,
        components: &[String],
    ) -> Result<Vec<u8>, VfsError> {
        match self.handle(kind, token)? {
            ResourceHandle::File(handle) if components.is_empty() => {
                let resource = self.file(handle)?;
                if !resource.authority().can_read() {
                    return Err(VfsError::PermissionDenied(VfsPathBuf::root()));
                }
                let materialized = resource.materialize()?;
                Self::read_file(materialized.backend)
            }
            ResourceHandle::Tree(handle) => {
                let resource = self.volume(handle)?;
                if !resource.authority().can_read() || !resource.authority().can_execute() {
                    return Err(VfsError::PermissionDenied(VfsPathBuf::root()));
                }
                let (name, parents) = components.split_last().ok_or_else(|| {
                    VfsError::Unsupported("cannot read a directory resource as bytes".into())
                })?;
                let materialized = resource.materialize()?;
                let directory = Self::directory_at(materialized.backend.root()?, parents)?;
                let component = VfsComponent::new(name)?;
                match directory.lookup(&component)? {
                    Some(Node::File(file)) => Self::read_file(file),
                    Some(Node::Directory(_) | Node::SyntheticDirectory) => {
                        Err(VfsError::IsDirectory(VfsPathBuf::root()))
                    }
                    None => Err(VfsError::Unsupported(format!(
                        "resource entry '{}' does not exist",
                        components.join("/")
                    ))),
                }
            }
            ResourceHandle::File(_) => Err(VfsError::Unsupported(
                "file resources do not contain child paths".into(),
            )),
        }
    }

    fn remove_resource_entry(
        &self,
        kind: &str,
        token: &str,
        components: &[String],
    ) -> Result<(), VfsError> {
        let ResourceHandle::Tree(handle) = self.handle(kind, token)? else {
            return Err(VfsError::Unsupported(
                "entry removal requires a tree resource".into(),
            ));
        };
        let resource = self.volume(handle)?;
        if !resource.authority().can_write() || !resource.authority().can_execute() {
            return Err(VfsError::PermissionDenied(VfsPathBuf::root()));
        }
        let (name, parents) = components
            .split_last()
            .ok_or_else(|| VfsError::Unsupported("cannot remove a resource root".into()))?;
        let materialized = resource.materialize()?;
        let directory = Self::directory_at(materialized.backend.root()?, parents)?;
        let component = VfsComponent::new(name)?;
        let kind = match directory.lookup(&component)? {
            Some(Node::File(_)) => RemoveKind::File,
            Some(Node::Directory(_) | Node::SyntheticDirectory) => RemoveKind::Directory,
            None => return Ok(()),
        };
        directory.remove(&component, kind)
    }

    fn scratch_resource(&mut self, backing: &str) -> Result<ResourceHandle, VfsError> {
        let factory = match backing {
            "memory" => ScratchVolumeFactory::new(ScratchBacking::Memory),
            "temporary" => {
                ScratchVolumeFactory::temporary_in(self.scratch_root.clone().ok_or_else(|| {
                    VfsError::Unsupported("temporary scratch storage is unavailable".into())
                })?)
            }
            _ => {
                return Err(VfsError::Unsupported(
                    "scratch backing must be 'memory' or 'temporary'".into(),
                ));
            }
        };
        Ok(ResourceHandle::Tree(self.insert_volume(Resource::new(
            Arc::new(SharedVolumeFactory::new(Arc::new(factory))),
            Permissions::ALL,
            ResourceOrigin::Scratch,
        ))))
    }

    fn directory_at(
        mut directory: Arc<dyn DirectoryNode>,
        components: &[String],
    ) -> Result<Arc<dyn DirectoryNode>, VfsError> {
        for name in components {
            let component = VfsComponent::new(name)?;
            directory = match directory.lookup(&component)? {
                Some(Node::Directory(directory)) => directory,
                Some(Node::File(_)) => return Err(VfsError::NotDirectory(VfsPathBuf::root())),
                Some(Node::SyntheticDirectory) => return Err(VfsError::StaleNode),
                None => {
                    return Err(VfsError::Unsupported(format!(
                        "resource directory '{}' does not exist",
                        components.join("/")
                    )));
                }
            };
        }
        Ok(directory)
    }

    fn read_file(file: Arc<dyn FileNode>) -> Result<Vec<u8>, VfsError> {
        let open = file.open(OpenOptions {
            read: true,
            write: false,
            truncate: false,
        })?;
        let len = usize::try_from(open.metadata()?.len)
            .map_err(|_| VfsError::Unsupported("resource file is too large to read".into()))?;
        let mut bytes = vec![0; len];
        let mut offset = 0;
        while offset < bytes.len() {
            let read = open.read(offset as u64, &mut bytes[offset..])?;
            if read == 0 {
                bytes.truncate(offset);
                break;
            }
            offset += read;
        }
        Ok(bytes)
    }

    fn seal(&mut self) -> Result<Option<PreparedFilesystemLaunch>, VfsError> {
        if !self.used {
            return Ok(None);
        }
        if let Some(filesystem) = &self.prepared {
            return Ok(Some(PreparedFilesystemLaunch::new(
                self.endpoint.clone(),
                filesystem.clone(),
            )));
        }
        let filesystem = Arc::new(std::mem::take(&mut self.plan).seal()?);
        self.prepared = Some(filesystem.clone());
        Ok(Some(PreparedFilesystemLaunch::new(
            self.endpoint.clone(),
            filesystem,
        )))
    }

    fn flush_staged(&mut self) -> Result<(), VfsError> {
        let filesystem = self
            .prepared
            .as_ref()
            .ok_or_else(|| VfsError::Unsupported("launch filesystem is not prepared".into()))?;
        for (path, contents) in self.staged_files.drain(..) {
            filesystem.replace_mounted_file(&path, contents)?;
        }
        Ok(())
    }
}

pub fn extension(options: FilesystemOptions) -> deno_core::Extension {
    ops::extension(options)
}

pub fn take_prepared(
    runtime: &mut JsRuntime,
) -> Result<Option<PreparedFilesystemLaunch>, VfsError> {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<FilesystemState>()
        .seal()
}

pub(crate) fn flush_staged(runtime: &mut JsRuntime) -> Result<(), VfsError> {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<FilesystemState>()
        .flush_staged()
}

pub(crate) fn discard_staged(runtime: &mut JsRuntime) {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<FilesystemState>()
        .staged_files
        .clear();
}

#[cfg(test)]
mod tests;
