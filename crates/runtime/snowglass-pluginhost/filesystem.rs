//! Filesystem composition capabilities owned by plugin-worker isolates.

use deno_core::{JsRuntime, OpState, op2};
use deno_error::JsErrorBox;
use serde::Serialize;
use serde_bytes::ByteBuf;
use snowglass_vfs::{
    AsarVolumeFactory, DirectoryNode, FileKind, FileNode, FilesystemEndpoint, FilesystemPlan,
    HostDirectoryFactory, HostFileFactory, Materialized, MemoryFileFactory, Node, OpenOptions,
    PackageDirectoryFactory, Permissions, PreparedFilesystemLaunch, ProfileLayout, RemoveKind,
    Resource, ResourceFactory, ResourceLease, ResourceOrigin, ScratchBacking, ScratchVolumeFactory,
    VfsComponent, VfsError, VfsPathBuf, Volume,
};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Component as HostComponent, Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use crate::{PluginResource, PluginResourceFormat, handle::Handle};

#[derive(Clone, Debug)]
pub struct NamedCache {
    pub key: String,
    pub path: PathBuf,
}

pub type StorageFactory = Arc<dyn ResourceFactory<dyn Volume>>;
pub type StorageFactories = BTreeMap<String, StorageFactory>;

#[derive(Clone)]
pub struct FilesystemOptions {
    pub package_root: PathBuf,
    pub resources: Vec<PluginResource>,
    pub caches: Vec<NamedCache>,
    pub storage: StorageFactories,
    pub quick_resume: Option<crate::QuickResumeData>,
}

impl std::fmt::Debug for FilesystemOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FilesystemOptions")
            .field("package_root", &self.package_root)
            .field("resources", &self.resources)
            .field("caches", &self.caches)
            .field("storage", &self.storage.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl FilesystemOptions {
    pub fn new(package_root: PathBuf) -> Self {
        Self {
            package_root,
            resources: Vec::new(),
            caches: Vec::new(),
            storage: BTreeMap::new(),
            quick_resume: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ResourceHandle {
    Tree(Handle<Resource<dyn snowglass_vfs::Volume>>),
    File(Handle<Resource<dyn snowglass_vfs::FileNode>>),
}

struct CacheResource {
    path: PathBuf,
    handle: Option<Handle<Resource<dyn snowglass_vfs::Volume>>>,
}

struct SharedVolumeFactory {
    source: Arc<dyn ResourceFactory<dyn Volume>>,
    materialized: parking_lot::Mutex<Option<(Arc<dyn Volume>, Arc<dyn ResourceLease>)>>,
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
    endpoint: FilesystemEndpoint,
    plan: FilesystemPlan,
    volumes: HashMap<
        Handle<Resource<dyn snowglass_vfs::Volume>>,
        Arc<Resource<dyn snowglass_vfs::Volume>>,
    >,
    files: HashMap<
        Handle<Resource<dyn snowglass_vfs::FileNode>>,
        Arc<Resource<dyn snowglass_vfs::FileNode>>,
    >,
    named: BTreeMap<String, ResourceHandle>,
    storage: BTreeMap<String, ResourceHandle>,
    caches: BTreeMap<String, CacheResource>,
    used: bool,
    working_directory_set: bool,
    profile_set: bool,
    prepared: Option<Arc<snowglass_vfs::PreparedFilesystem>>,
    staged_files: Vec<(VfsPathBuf, Vec<u8>)>,
    quick_resume: Option<(ResourceHandle, Option<ResourceHandle>)>,
}

impl FilesystemState {
    fn new(options: FilesystemOptions) -> Result<Self, VfsError> {
        let package_root = options.package_root.canonicalize()?;
        let endpoint = FilesystemEndpoint::random();
        let mut state = Self {
            package_root,
            endpoint,
            plan: FilesystemPlan::default(),
            volumes: HashMap::new(),
            files: HashMap::new(),
            named: BTreeMap::new(),
            storage: BTreeMap::new(),
            caches: BTreeMap::new(),
            used: false,
            working_directory_set: false,
            profile_set: false,
            prepared: None,
            staged_files: Vec::new(),
            quick_resume: None,
        };
        for resource in options.resources {
            if state.named.contains_key(&resource.name) {
                return Err(VfsError::Unsupported(format!(
                    "duplicate named resource '{}'",
                    resource.name
                )));
            }
            let handle =
                state.insert_path_resource(resource.path, resource.format, resource.permissions)?;
            state.named.insert(resource.name, handle);
        }
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

    fn insert_path_resource(
        &mut self,
        path: PathBuf,
        kind: PluginResourceFormat,
        permissions: Permissions,
    ) -> Result<ResourceHandle, VfsError> {
        Ok(match kind {
            PluginResourceFormat::Directory => {
                ResourceHandle::Tree(self.insert_volume(Resource::new(
                    Arc::new(PackageDirectoryFactory::new(path.clone())),
                    permissions,
                    ResourceOrigin::Package(path.display().to_string()),
                )))
            }
            PluginResourceFormat::Asar => ResourceHandle::Tree(self.insert_volume(Resource::new(
                Arc::new(AsarVolumeFactory::new(path.clone())),
                permissions,
                ResourceOrigin::Asar(path.display().to_string()),
            ))),
            PluginResourceFormat::File => ResourceHandle::File(self.insert_file(Resource::new(
                Arc::new(HostFileFactory::new(path.clone(), false)),
                permissions,
                ResourceOrigin::Package(path.display().to_string()),
            ))),
        })
    }

    fn insert_volume(
        &mut self,
        resource: Resource<dyn snowglass_vfs::Volume>,
    ) -> Handle<Resource<dyn snowglass_vfs::Volume>> {
        let handle = Handle::new();
        let replaced = self.volumes.insert(handle, Arc::new(resource));
        debug_assert!(replaced.is_none());
        handle
    }

    fn insert_file(
        &mut self,
        resource: Resource<dyn snowglass_vfs::FileNode>,
    ) -> Handle<Resource<dyn snowglass_vfs::FileNode>> {
        let handle = Handle::new();
        let replaced = self.files.insert(handle, Arc::new(resource));
        debug_assert!(replaced.is_none());
        handle
    }

    fn volume(
        &self,
        handle: Handle<Resource<dyn snowglass_vfs::Volume>>,
    ) -> Result<Arc<Resource<dyn snowglass_vfs::Volume>>, VfsError> {
        self.volumes
            .get(&handle)
            .cloned()
            .ok_or_else(invalid_handle)
    }

    fn file(
        &self,
        handle: Handle<Resource<dyn snowglass_vfs::FileNode>>,
    ) -> Result<Arc<Resource<dyn snowglass_vfs::FileNode>>, VfsError> {
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
                "pkg: source escapes its package: {source}"
            )));
        }
        let candidate = self.package_root.join(relative);
        reject_links(&self.package_root, relative)?;
        let canonical = candidate.canonicalize()?;
        if !canonical.starts_with(&self.package_root) {
            return Err(VfsError::Unsupported(format!(
                "pkg: source escapes its package: {source}"
            )));
        }
        Ok(canonical)
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
    ) -> Result<Handle<Resource<dyn snowglass_vfs::Volume>>, VfsError> {
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
        let backing = match backing {
            "memory" => ScratchBacking::Memory,
            "temporary" => ScratchBacking::Temporary,
            _ => {
                return Err(VfsError::Unsupported(
                    "scratch backing must be 'memory' or 'temporary'".into(),
                ));
            }
        };
        Ok(ResourceHandle::Tree(self.insert_volume(Resource::new(
            Arc::new(SharedVolumeFactory::new(Arc::new(
                ScratchVolumeFactory::new(backing),
            ))),
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsResource {
    name: Option<String>,
    kind: &'static str,
    handle: String,
}

#[derive(Serialize)]
struct JsDirectoryEntry {
    name: String,
    kind: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsQuickResume {
    state: JsResource,
    screenshot: Option<JsResource>,
}

impl JsResource {
    fn new(name: Option<String>, handle: ResourceHandle) -> Self {
        match handle {
            ResourceHandle::Tree(handle) => Self {
                name,
                kind: "tree",
                handle: handle.to_string(),
            },
            ResourceHandle::File(handle) => Self {
                name,
                kind: "file",
                handle: handle.to_string(),
            },
        }
    }

    fn storage(name: String, handle: ResourceHandle) -> Self {
        let ResourceHandle::Tree(handle) = handle else {
            unreachable!("storage resources are volume factories")
        };
        Self {
            name: Some(name),
            kind: "storage",
            handle: handle.to_string(),
        }
    }
}

fn js_error(error: impl std::fmt::Display) -> JsErrorBox {
    JsErrorBox::generic(error.to_string())
}

fn invalid_handle() -> VfsError {
    VfsError::Unsupported("unknown or stale JavaScript resource handle".into())
}

#[op2]
#[serde]
fn op_vfs_resources(state: &mut OpState) -> Vec<JsResource> {
    state
        .borrow::<FilesystemState>()
        .named
        .iter()
        .map(|(name, handle)| JsResource::new(Some(name.clone()), *handle))
        .collect()
}

#[op2]
#[serde]
fn op_vfs_storage(state: &mut OpState) -> Vec<JsResource> {
    state
        .borrow::<FilesystemState>()
        .storage
        .iter()
        .map(|(name, handle)| JsResource::storage(name.clone(), *handle))
        .collect()
}

#[op2]
#[serde]
fn op_vfs_quick_resume(state: &mut OpState) -> Option<JsQuickResume> {
    state
        .borrow::<FilesystemState>()
        .quick_resume
        .map(|(file, screenshot)| JsQuickResume {
            state: JsResource::new(None, file),
            screenshot: screenshot.map(|file| JsResource::new(None, file)),
        })
}

#[op2]
#[serde]
fn op_vfs_resource_entries(
    state: &mut OpState,
    #[string] kind: String,
    #[string] token: String,
    #[serde] components: Vec<String>,
) -> Result<Vec<JsDirectoryEntry>, JsErrorBox> {
    state
        .borrow::<FilesystemState>()
        .resource_entries(&kind, &token, &components)
        .map_err(js_error)
}

#[op2]
#[serde]
fn op_vfs_resource_bytes(
    state: &mut OpState,
    #[string] kind: String,
    #[string] token: String,
    #[serde] components: Vec<String>,
) -> Result<ByteBuf, JsErrorBox> {
    state
        .borrow::<FilesystemState>()
        .resource_bytes(&kind, &token, &components)
        .map(ByteBuf::from)
        .map_err(js_error)
}

#[op2]
fn op_vfs_resource_remove(
    state: &mut OpState,
    #[string] kind: String,
    #[string] token: String,
    #[serde] components: Vec<String>,
) -> Result<(), JsErrorBox> {
    state
        .borrow::<FilesystemState>()
        .remove_resource_entry(&kind, &token, &components)
        .map_err(js_error)
}

#[op2]
#[serde]
fn op_vfs_scratch_resource(
    state: &mut OpState,
    #[string] backing: String,
) -> Result<JsResource, JsErrorBox> {
    state
        .borrow_mut::<FilesystemState>()
        .scratch_resource(&backing)
        .map(|handle| JsResource::new(None, handle))
        .map_err(js_error)
}

#[op2]
#[serde]
fn op_vfs_mount_cache(
    state: &mut OpState,
    #[string] key: String,
) -> Result<JsResource, JsErrorBox> {
    let handle = state
        .borrow_mut::<FilesystemState>()
        .cache(&key)
        .map_err(js_error)?;
    Ok(JsResource::new(None, handle))
}

#[op2(fast)]
fn op_vfs_mount_host_tree(
    state: &mut OpState,
    #[string] target: String,
    #[string] source: String,
    #[string] mode: String,
) -> Result<(), JsErrorBox> {
    let permissions = Permissions::from_str(&mode).map_err(js_error)?;
    let authority = Permissions::from_str("rx").expect("canonical permission constant");
    if !authority.contains(permissions) {
        return Err(js_error(VfsError::AuthorityExceeded {
            authority,
            requested: permissions,
        }));
    }
    let path = PathBuf::from(source);
    if !path.is_absolute() || !path.is_dir() {
        return Err(js_error(
            "mountHostTree source must be an absolute host directory path",
        ));
    }
    let vfs = state.borrow_mut::<FilesystemState>();
    let resource = Resource::new(
        Arc::new(PackageDirectoryFactory::new(path)),
        authority,
        ResourceOrigin::HostFile,
    );
    let handle = vfs.insert_volume(resource);
    let resource = vfs.volume(handle).map_err(js_error)?;
    vfs.plan.mount_tree(
        VfsPathBuf::new(target).map_err(js_error)?,
        resource,
        permissions,
    );
    vfs.used = true;
    Ok(())
}

#[op2(fast)]
fn op_vfs_mount_host_file(
    state: &mut OpState,
    #[string] target: String,
    #[string] source: String,
    #[string] mode: String,
) -> Result<(), JsErrorBox> {
    let permissions = Permissions::from_str(&mode).map_err(js_error)?;
    let authority = Permissions::from_str("rx").expect("canonical permission constant");
    if !authority.contains(permissions) {
        return Err(js_error(VfsError::AuthorityExceeded {
            authority,
            requested: permissions,
        }));
    }
    let path = PathBuf::from(source);
    if !path.is_absolute() || !path.is_file() {
        return Err(js_error(
            "mountHostFile source must be an absolute host file path",
        ));
    }
    let vfs = state.borrow_mut::<FilesystemState>();
    let resource = Resource::new(
        Arc::new(HostFileFactory::new(path, false)),
        authority,
        ResourceOrigin::HostFile,
    );
    let handle = vfs.insert_file(resource);
    let resource = vfs.file(handle).map_err(js_error)?;
    vfs.plan.mount_file(
        VfsPathBuf::new(target).map_err(js_error)?,
        resource,
        permissions,
    );
    vfs.used = true;
    Ok(())
}

#[op2(fast)]
fn op_vfs_mount_tree_pkg(
    state: &mut OpState,
    #[string] target: String,
    #[string] source: String,
    #[string] mode: String,
) -> Result<(), JsErrorBox> {
    let permissions = Permissions::from_str(&mode).map_err(js_error)?;
    let authority = Permissions::from_str("rx").expect("canonical permission constant");
    if !authority.contains(permissions) {
        return Err(js_error(VfsError::AuthorityExceeded {
            authority,
            requested: permissions,
        }));
    }
    let path = state
        .borrow::<FilesystemState>()
        .package_path(&source)
        .map_err(js_error)?;
    let metadata = std::fs::metadata(&path).map_err(js_error)?;
    let kind = if metadata.is_dir() {
        PluginResourceFormat::Directory
    } else if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("asar"))
    {
        PluginResourceFormat::Asar
    } else {
        return Err(js_error(
            "mountTree pkg: source must be a directory or .asar file",
        ));
    };
    let vfs = state.borrow_mut::<FilesystemState>();
    let handle = vfs
        .insert_path_resource(path, kind, authority)
        .map_err(js_error)?;
    let ResourceHandle::Tree(handle) = handle else {
        unreachable!()
    };
    let resource = vfs.volume(handle).map_err(js_error)?;
    vfs.plan.mount_tree(
        VfsPathBuf::new(target).map_err(js_error)?,
        resource,
        permissions,
    );
    vfs.used = true;
    Ok(())
}

#[op2(fast)]
fn op_vfs_mount_tree_resource(
    state: &mut OpState,
    #[string] target: String,
    #[string] kind: String,
    #[string] token: String,
    #[string] mode: String,
) -> Result<(), JsErrorBox> {
    let permissions = Permissions::from_str(&mode).map_err(js_error)?;
    let vfs = state.borrow_mut::<FilesystemState>();
    let ResourceHandle::Tree(handle) = vfs.handle(&kind, &token).map_err(js_error)? else {
        return Err(js_error("mountTree requires a tree resource"));
    };
    let resource = vfs.volume(handle).map_err(js_error)?;
    vfs.plan.mount_tree(
        VfsPathBuf::new(target).map_err(js_error)?,
        resource,
        permissions,
    );
    vfs.used = true;
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsStorageMount {
    mode: String,
    format: Option<String>,
}

#[op2]
fn op_vfs_mount_storage(
    state: &mut OpState,
    #[string] target: String,
    #[string] kind: String,
    #[string] token: String,
    #[serde] options: JsStorageMount,
) -> Result<(), JsErrorBox> {
    let permissions = Permissions::from_str(&options.mode).map_err(js_error)?;
    let vfs = state.borrow_mut::<FilesystemState>();
    let handle = vfs.storage_handle(&kind, &token).map_err(js_error)?;
    let resource = vfs.volume(handle).map_err(js_error)?;
    vfs.plan.mount_storage(
        VfsPathBuf::new(target).map_err(js_error)?,
        resource,
        permissions,
        options.format,
    );
    vfs.used = true;
    Ok(())
}

#[op2(fast)]
fn op_vfs_mount_file_pkg(
    state: &mut OpState,
    #[string] target: String,
    #[string] source: String,
    #[string] mode: String,
) -> Result<(), JsErrorBox> {
    let permissions = Permissions::from_str(&mode).map_err(js_error)?;
    let authority = Permissions::from_str("rx").expect("canonical permission constant");
    if !authority.contains(permissions) {
        return Err(js_error(VfsError::AuthorityExceeded {
            authority,
            requested: permissions,
        }));
    }
    let path = state
        .borrow::<FilesystemState>()
        .package_path(&source)
        .map_err(js_error)?;
    if !path.is_file() {
        return Err(js_error("mountFile pkg: source must be a regular file"));
    }
    let vfs = state.borrow_mut::<FilesystemState>();
    let ResourceHandle::File(handle) = vfs
        .insert_path_resource(path, PluginResourceFormat::File, authority)
        .map_err(js_error)?
    else {
        unreachable!()
    };
    let resource = vfs.file(handle).map_err(js_error)?;
    vfs.plan.mount_file(
        VfsPathBuf::new(target).map_err(js_error)?,
        resource,
        permissions,
    );
    vfs.used = true;
    Ok(())
}

#[op2(fast)]
fn op_vfs_mount_file_text(
    state: &mut OpState,
    #[string] target: String,
    #[string] contents: String,
    #[string] mode: String,
) -> Result<(), JsErrorBox> {
    mount_memory_file(state, target, contents.into_bytes(), mode)
}

#[op2]
fn op_vfs_mount_file_bytes(
    state: &mut OpState,
    #[string] target: String,
    #[serde] contents: ByteBuf,
    #[string] mode: String,
) -> Result<(), JsErrorBox> {
    mount_memory_file(state, target, contents.into_vec(), mode)
}

fn mount_memory_file(
    state: &mut OpState,
    target: String,
    contents: Vec<u8>,
    mode: String,
) -> Result<(), JsErrorBox> {
    let permissions = Permissions::from_str(&mode).map_err(js_error)?;
    let target = VfsPathBuf::new(target).map_err(js_error)?;
    if state.borrow::<FilesystemState>().prepared.is_some() {
        let vfs = state.borrow_mut::<FilesystemState>();
        vfs.staged_files.push((target, contents));
        return Ok(());
    }
    let factory = if permissions.can_write() {
        MemoryFileFactory::writable(contents)
    } else {
        MemoryFileFactory::immutable(contents)
    };
    let vfs = state.borrow_mut::<FilesystemState>();
    let handle = vfs.insert_file(Resource::new(
        Arc::new(factory),
        permissions,
        ResourceOrigin::Memory,
    ));
    let resource = vfs.file(handle).map_err(js_error)?;
    vfs.plan.mount_file(target, resource, permissions);
    vfs.used = true;
    Ok(())
}

#[op2(fast)]
fn op_vfs_mount_file_resource(
    state: &mut OpState,
    #[string] target: String,
    #[string] kind: String,
    #[string] token: String,
    #[string] mode: String,
) -> Result<(), JsErrorBox> {
    let permissions = Permissions::from_str(&mode).map_err(js_error)?;
    let vfs = state.borrow_mut::<FilesystemState>();
    let ResourceHandle::File(handle) = vfs.handle(&kind, &token).map_err(js_error)? else {
        return Err(js_error("mountFile requires a file resource"));
    };
    let resource = vfs.file(handle).map_err(js_error)?;
    vfs.plan.mount_file(
        VfsPathBuf::new(target).map_err(js_error)?,
        resource,
        permissions,
    );
    vfs.used = true;
    Ok(())
}

#[op2(fast)]
fn op_vfs_mount_scratch(
    state: &mut OpState,
    #[string] target: String,
    #[string] mode: String,
    #[string] backing: String,
) -> Result<(), JsErrorBox> {
    let permissions = Permissions::from_str(&mode).map_err(js_error)?;
    let backing = match backing.as_str() {
        "memory" => ScratchBacking::Memory,
        "temporary" => ScratchBacking::Temporary,
        _ => return Err(js_error("scratch backing must be 'memory' or 'temporary'")),
    };
    let vfs = state.borrow_mut::<FilesystemState>();
    let handle = vfs.insert_volume(Resource::new(
        Arc::new(ScratchVolumeFactory::new(backing)),
        Permissions::ALL,
        ResourceOrigin::Scratch,
    ));
    let resource = vfs.volume(handle).map_err(js_error)?;
    vfs.plan.mount_tree(
        VfsPathBuf::new(target).map_err(js_error)?,
        resource,
        permissions,
    );
    vfs.used = true;
    Ok(())
}

#[op2]
#[string]
fn op_vfs_path(state: &mut OpState, #[string] path: String) -> Result<String, JsErrorBox> {
    let path = VfsPathBuf::new(path).map_err(js_error)?;
    Ok(state
        .borrow::<FilesystemState>()
        .endpoint
        .project(path.as_path()))
}

#[op2(fast)]
fn op_vfs_set_working_directory(
    state: &mut OpState,
    #[string] path: String,
) -> Result<(), JsErrorBox> {
    let vfs = state.borrow_mut::<FilesystemState>();
    if vfs.working_directory_set {
        return Err(js_error("working directory has already been set"));
    }
    vfs.plan
        .set_working_directory(VfsPathBuf::new(path).map_err(js_error)?);
    vfs.working_directory_set = true;
    vfs.used = true;
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsProfile {
    home: Option<String>,
    roaming_app_data: Option<String>,
    local_app_data: Option<String>,
    temp: Option<String>,
}

#[op2]
fn op_vfs_set_profile(state: &mut OpState, #[serde] profile: JsProfile) -> Result<(), JsErrorBox> {
    let vfs = state.borrow_mut::<FilesystemState>();
    if vfs.profile_set {
        return Err(js_error("profile layout has already been set"));
    }
    let parse = |value: Option<String>| value.map(VfsPathBuf::new).transpose();
    vfs.plan.set_profile(ProfileLayout {
        home: parse(profile.home).map_err(js_error)?,
        roaming_app_data: parse(profile.roaming_app_data).map_err(js_error)?,
        local_app_data: parse(profile.local_app_data).map_err(js_error)?,
        temp: parse(profile.temp).map_err(js_error)?,
    });
    vfs.profile_set = true;
    vfs.used = true;
    Ok(())
}

#[op2(fast)]
fn op_vfs_flush(state: &mut OpState) -> Result<(), JsErrorBox> {
    state
        .borrow_mut::<FilesystemState>()
        .flush_staged()
        .map_err(js_error)
}

deno_core::extension!(snowglass_filesystem,
    ops = [
        op_vfs_resources, op_vfs_storage, op_vfs_quick_resume,
        op_vfs_resource_entries, op_vfs_resource_bytes,
        op_vfs_resource_remove, op_vfs_scratch_resource,
        op_vfs_mount_cache, op_vfs_mount_host_tree, op_vfs_mount_host_file,
        op_vfs_mount_tree_pkg, op_vfs_mount_tree_resource, op_vfs_mount_file_pkg,
        op_vfs_mount_file_text, op_vfs_mount_file_bytes, op_vfs_mount_file_resource,
        op_vfs_mount_scratch, op_vfs_mount_storage, op_vfs_path,
        op_vfs_set_working_directory, op_vfs_set_profile, op_vfs_flush,
    ],
    esm_entry_point = "ext:snowglass_filesystem/filesystem.js",
    esm = [dir "js", "filesystem.js"],
    options = { options: FilesystemOptions },
    state = |state, options| { state.put(FilesystemState::new(options.options).expect("validated filesystem options")); },
);

pub fn extension(options: FilesystemOptions) -> deno_core::Extension {
    snowglass_filesystem::init(options)
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

fn reject_links(root: &Path, relative: &Path) -> Result<(), VfsError> {
    let mut current = root.to_owned();
    for component in relative.components() {
        current.push(component.as_os_str());
        if std::fs::symlink_metadata(&current)?
            .file_type()
            .is_symlink()
        {
            return Err(VfsError::Unsupported(
                "pkg: sources cannot traverse links".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_cache_is_lazy_persistent_and_reused() {
        let package = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        let cache_path = storage.path().join("plugin-cache");
        let mut options = FilesystemOptions::new(package.path().to_owned());
        options.caches.push(NamedCache {
            key: "shaders".to_owned(),
            path: cache_path.clone(),
        });
        let mut state = FilesystemState::new(options).unwrap();

        assert!(!cache_path.exists());
        let ResourceHandle::Tree(first) = state.cache("shaders").unwrap() else {
            unreachable!()
        };
        let ResourceHandle::Tree(second) = state.cache("shaders").unwrap() else {
            unreachable!()
        };
        assert_eq!(first, second);
        assert!(cache_path.is_dir());

        let resource = state.volume(first).unwrap();
        assert_eq!(resource.authority(), Permissions::ALL);
        assert_eq!(
            resource.origin(),
            &ResourceOrigin::Cache("shaders".to_owned())
        );
        let materialized = resource.materialize().unwrap();
        assert!(
            !materialized
                .backend
                .root()
                .unwrap()
                .metadata()
                .unwrap()
                .readonly
        );
        assert!(state.cache("undeclared").is_err());
    }

    #[test]
    fn javascript_mount_cache_returns_a_mountable_tree_capability() {
        let package = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        let cache_path = storage.path().join("plugin-cache");
        let mut options = FilesystemOptions::new(package.path().to_owned());
        options.caches.push(NamedCache {
            key: "shaders".to_owned(),
            path: cache_path.clone(),
        });
        let mut runtime = JsRuntime::new(deno_core::RuntimeOptions {
            extensions: vec![extension(options)],
            ..Default::default()
        });

        runtime
            .execute_script(
                "snowglass:test/cache.js",
                r#"
                    const cache = __snowglassFilesystem.filesystem.mountCache("shaders");
                    __snowglassFilesystem.filesystem.mountTree(
                      "/Cache", cache, { mode: "rwx" });
                "#,
            )
            .unwrap();

        let prepared = take_prepared(&mut runtime).unwrap().unwrap();
        assert_eq!(prepared.filesystem().compiled().bindings().len(), 1);
        assert!(cache_path.is_dir());
    }

    #[test]
    fn javascript_mount_storage_preserves_root_and_format_requests() {
        fn runtime_with_storage(package: &Path) -> JsRuntime {
            let mut options = FilesystemOptions::new(package.to_owned());
            options.storage.insert(
                "card1".into(),
                Arc::new(ScratchVolumeFactory::new(ScratchBacking::Memory)),
            );
            JsRuntime::new(deno_core::RuntimeOptions {
                extensions: vec![extension(options)],
                ..Default::default()
            })
        }

        let package = tempfile::tempdir().unwrap();
        let mut root = runtime_with_storage(package.path());
        root.execute_script(
            "snowglass:test/storage-root.js",
            r#"
                const card = __snowglassFilesystem.storage.card1;
                __snowglassFilesystem.filesystem.mountStorage(
                  "/Storage", card, { mode: "rwx" });
            "#,
        )
        .unwrap();
        let prepared = take_prepared(&mut root).unwrap().unwrap();
        assert!(matches!(
            prepared.filesystem().compiled().bindings()[0].kind(),
            snowglass_vfs::BindingKind::Storage { format: None, .. }
        ));
        prepared.filesystem().materialize().unwrap();

        let mut formatted = runtime_with_storage(package.path());
        formatted
            .execute_script(
                "snowglass:test/storage-format.js",
                r#"
                    const card = __snowglassFilesystem.storage.card1;
                    __snowglassFilesystem.filesystem.mountStorage(
                      "/Storage/card.mcd", card, { mode: "rw", format: "raw" });
                "#,
            )
            .unwrap();
        let prepared = take_prepared(&mut formatted).unwrap().unwrap();
        assert!(matches!(
            prepared.filesystem().compiled().bindings()[0].kind(),
            snowglass_vfs::BindingKind::Storage { format: Some(format), .. } if format == "raw"
        ));
        assert!(matches!(
            prepared.filesystem().materialize(),
            Err(VfsError::Unsupported(reason)) if reason.contains("does not support")
        ));
    }
}
