//! Filesystem composition capabilities owned by plugin-worker isolates.

mod ops;
pub(crate) use ops::JsResource;
mod entry;
mod mount;
mod package;

use deno_core::{JsRuntime, OpState, op2};
use deno_error::JsErrorBox;
use polymer_vfs::{
    AsarVolumeFactory, DirectoryNode, FileKind, FileNode, FilesystemEndpoint, FilesystemPlan,
    HostDirectoryFactory, HostFileFactory, MemoryFileFactory, Node, OpenOptions,
    PackageDirectoryFactory, Permissions, PreparedFilesystemLaunch, RemoveKind, Resource,
    ResourceFactory, ResourceLease, ResourceOrigin, TempDirectoryFactory, VfsComponent, VfsError,
    VfsPathBuf, Volume,
};
use serde::Serialize;
use serde_bytes::ByteBuf;
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use crate::{emulator::QuickResumeData, handle::Handle};
use polymer_pluginmanifest::{PackageArchive, PackageEntry, PackageSource};

#[derive(Clone, Debug)]
pub struct NamedCache {
    pub key: String,
    pub path: PathBuf,
}

pub type StorageFactories = BTreeMap<String, Arc<dyn ResourceFactory<dyn Volume>>>;

type VolumeResource = Resource<dyn Volume>;
type FileResource = Resource<dyn FileNode>;

#[derive(Clone)]
pub struct FilesystemOptions {
    pub package: PackageSource,
    pub scratch_root: Option<PathBuf>,
    pub caches: Vec<NamedCache>,
    pub storage: StorageFactories,
    pub quick_resume: Option<QuickResumeData>,
}

impl std::fmt::Debug for FilesystemOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FilesystemOptions")
            .field("package", &self.package)
            .field("caches", &self.caches)
            .field("storage", &self.storage.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl FilesystemOptions {
    pub fn new(package: PackageSource) -> Self {
        Self {
            package,
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

#[allow(clippy::type_complexity)]
struct SharedResourceFactory<T: ?Sized> {
    source: Resource<T>,
    materialized: parking_lot::Mutex<Option<(Arc<T>, Arc<dyn ResourceLease>)>>,
}

impl<T: ?Sized> SharedResourceFactory<T> {
    fn shared(&self) -> Result<(Arc<T>, Arc<dyn ResourceLease>), VfsError> {
        let mut materialized = self.materialized.lock();
        if materialized.is_none() {
            let value = self.source.materialize()?;
            *materialized = Some((value.backend, Arc::new(value.lease)));
        }
        let (backend, lease) = materialized
            .as_ref()
            .expect("shared resource was materialized");
        Ok((backend.clone(), lease.clone()))
    }
}

impl<T: ?Sized + Send + Sync> ResourceFactory<T> for SharedResourceFactory<T> {
    fn materialize_in(
        &self,
        scope: &mut polymer_vfs::MaterializationScope,
    ) -> Result<Arc<T>, VfsError> {
        let (backend, lease) = self.shared()?;
        scope.retain(lease);
        Ok(backend)
    }
}

struct SharedResource<T: ?Sized> {
    resource: Arc<Resource<T>>,
    factory: Arc<SharedResourceFactory<T>>,
}

impl<T: ?Sized + Send + Sync + 'static> SharedResource<T> {
    fn new(source: Resource<T>) -> Self {
        let authority = source.authority();
        let origin = source.origin().clone();
        let factory = Arc::new(SharedResourceFactory {
            source,
            materialized: parking_lot::Mutex::new(None),
        });
        Self {
            resource: Arc::new(Resource::new(factory.clone(), authority, origin)),
            factory,
        }
    }
}

pub(crate) struct FilesystemState {
    package: PackageSource,
    scratch_root: Option<PathBuf>,
    endpoint: FilesystemEndpoint,
    plan: FilesystemPlan,
    volumes: HashMap<Handle<VolumeResource>, SharedResource<dyn Volume>>,
    files: HashMap<Handle<FileResource>, SharedResource<dyn FileNode>>,
    storage: BTreeMap<String, Handle<VolumeResource>>,
    storage_volumes: HashMap<Handle<VolumeResource>, Arc<VolumeResource>>,
    caches: BTreeMap<String, CacheResource>,
    used: bool,
    sealed: bool,
    prepared: Option<Arc<polymer_vfs::PreparedFilesystem>>,
    staged_files: Vec<(VfsPathBuf, Vec<u8>)>,
    quick_resume: Option<(ResourceHandle, Option<ResourceHandle>)>,
}

impl FilesystemState {
    fn new(options: FilesystemOptions) -> Result<Self, VfsError> {
        let endpoint = FilesystemEndpoint::random();
        let mut state = Self {
            package: options.package,
            scratch_root: options.scratch_root,
            endpoint,
            plan: FilesystemPlan::default(),
            volumes: HashMap::new(),
            files: HashMap::new(),
            storage: BTreeMap::new(),
            storage_volumes: HashMap::new(),
            caches: BTreeMap::new(),
            used: false,
            sealed: false,
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
            let handle = Handle::new();
            state.storage_volumes.insert(
                handle,
                Arc::new(Resource::new(factory, Permissions::ALL, origin)),
            );
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
            #[cfg(target_os = "macos")]
            if path.extension().is_some_and(|extension| extension == "app") {
                return Ok(ResourceHandle::Tree(self.insert_volume(Resource::new(
                    Arc::new(polymer_platform_macos::BundleFactory::new(path.clone())),
                    permissions,
                    ResourceOrigin::Package(path.display().to_string()),
                ))));
            }
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

    /// Mounts the package file or directory at the `/`-separated `relative` path read-only.
    fn insert_package_resource(&mut self, relative: &str) -> Result<ResourceHandle, VfsError> {
        match self.package.clone() {
            PackageSource::Directory(_) => {
                let path = self
                    .package
                    .host_path(relative)
                    .map_err(|error| VfsError::Unsupported(error.to_string()))?
                    .expect("directory packages have host paths");
                self.insert_path_resource(path)
            }
            PackageSource::Archive(archive) => self.insert_archived_resource(&archive, relative),
        }
    }

    fn insert_archived_resource(
        &mut self,
        archive: &PackageArchive,
        relative: &str,
    ) -> Result<ResourceHandle, VfsError> {
        let permissions = Permissions::from_str("rx").expect("canonical package permissions");
        let origin = ResourceOrigin::Asar(format!("{}/{relative}", archive.path().display()));
        let volume = match (self.package.entry(relative), archive.file(relative)) {
            (Some(PackageEntry::Directory), _) => {
                AsarVolumeFactory::memory(archive.bytes().clone(), relative)
            }
            (Some(PackageEntry::File), Some(bytes))
                if relative.to_ascii_lowercase().ends_with(".asar") =>
            {
                AsarVolumeFactory::memory(Arc::from(bytes), "")
            }
            (Some(PackageEntry::File), Some(bytes)) => {
                return Ok(ResourceHandle::File(self.insert_file(Resource::new(
                    Arc::new(MemoryFileFactory::immutable(bytes.to_vec())),
                    permissions,
                    origin,
                ))));
            }
            _ => return Err(std::io::Error::from(std::io::ErrorKind::NotFound).into()),
        };
        Ok(ResourceHandle::Tree(self.insert_volume(Resource::new(
            Arc::new(volume),
            permissions,
            origin,
        ))))
    }

    fn insert_volume(
        &mut self,
        resource: Resource<dyn polymer_vfs::Volume>,
    ) -> Handle<Resource<dyn polymer_vfs::Volume>> {
        let handle = Handle::new();
        let replaced = self.volumes.insert(handle, SharedResource::new(resource));
        debug_assert!(replaced.is_none());
        handle
    }

    fn insert_file(
        &mut self,
        resource: Resource<dyn polymer_vfs::FileNode>,
    ) -> Handle<Resource<dyn polymer_vfs::FileNode>> {
        let handle = Handle::new();
        let replaced = self.files.insert(handle, SharedResource::new(resource));
        debug_assert!(replaced.is_none());
        handle
    }

    fn volume(
        &self,
        handle: Handle<Resource<dyn polymer_vfs::Volume>>,
    ) -> Result<Arc<Resource<dyn polymer_vfs::Volume>>, VfsError> {
        self.volumes
            .get(&handle)
            .map(|shared| shared.resource.clone())
            .ok_or_else(invalid_handle)
    }

    fn volume_backend(
        &self,
        handle: Handle<Resource<dyn polymer_vfs::Volume>>,
    ) -> Result<Arc<dyn Volume>, VfsError> {
        let shared = self.volumes.get(&handle).ok_or_else(invalid_handle)?;
        Ok(shared.factory.shared()?.0)
    }

    fn file(
        &self,
        handle: Handle<Resource<dyn polymer_vfs::FileNode>>,
    ) -> Result<Arc<Resource<dyn polymer_vfs::FileNode>>, VfsError> {
        self.files
            .get(&handle)
            .map(|shared| shared.resource.clone())
            .ok_or_else(invalid_handle)
    }

    fn file_backend(
        &self,
        handle: Handle<Resource<dyn polymer_vfs::FileNode>>,
    ) -> Result<Arc<dyn FileNode>, VfsError> {
        let shared = self.files.get(&handle).ok_or_else(invalid_handle)?;
        Ok(shared.factory.shared()?.0)
    }

    fn package_path(&self, source: &str) -> Result<String, VfsError> {
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
        PackageSource::relative(Path::new(decoded.as_ref())).map_err(|_| {
            VfsError::Unsupported(format!(
                "pkg: source must contain only normal path components: {source}"
            ))
        })
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

    fn storage_volume(&self, kind: &str, token: &str) -> Result<Arc<VolumeResource>, VfsError> {
        if kind != "storage" {
            return Err(invalid_handle());
        }
        let token = u64::from_str_radix(token, 16).map_err(|_| invalid_handle())?;
        self.storage_volumes
            .get(&Handle::from_raw(token))
            .cloned()
            .ok_or_else(invalid_handle)
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
        let directory = Self::directory_at(self.volume_backend(handle)?.root()?, components)?;
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
        Self::read_file(self.resource_file(kind, token, components)?)
    }

    pub(crate) fn resource_file(
        &self,
        kind: &str,
        token: &str,
        components: &[String],
    ) -> Result<Arc<dyn FileNode>, VfsError> {
        match self.handle(kind, token)? {
            ResourceHandle::File(handle) if components.is_empty() => {
                let resource = self.file(handle)?;
                if !resource.authority().can_read() {
                    return Err(VfsError::PermissionDenied(VfsPathBuf::root()));
                }
                self.file_backend(handle)
            }
            ResourceHandle::Tree(handle) => {
                let resource = self.volume(handle)?;
                if !resource.authority().can_read() || !resource.authority().can_execute() {
                    return Err(VfsError::PermissionDenied(VfsPathBuf::root()));
                }
                let (name, parents) = components.split_last().ok_or_else(|| {
                    VfsError::Unsupported("cannot read a directory resource as bytes".into())
                })?;
                let directory = Self::directory_at(self.volume_backend(handle)?.root()?, parents)?;
                let component = VfsComponent::new(name)?;
                match directory.lookup(&component)? {
                    Some(Node::File(file)) => Ok(file),
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
        let directory = Self::directory_at(self.volume_backend(handle)?.root()?, parents)?;
        let component = VfsComponent::new(name)?;
        let kind = match directory.lookup(&component)? {
            Some(Node::File(_)) => RemoveKind::File,
            Some(Node::Directory(_) | Node::SyntheticDirectory) => RemoveKind::Directory,
            None => return Ok(()),
        };
        directory.remove(&component, kind)
    }

    fn scratch_resource(&mut self) -> Result<ResourceHandle, VfsError> {
        let factory =
            TempDirectoryFactory::in_root(self.scratch_root.clone().ok_or_else(|| {
                VfsError::Unsupported("temporary scratch storage is unavailable".into())
            })?);
        Ok(ResourceHandle::Tree(self.insert_volume(Resource::new(
            Arc::new(factory),
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
        self.sealed = true;
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
