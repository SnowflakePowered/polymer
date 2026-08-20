//! Filesystem composition capabilities owned by plugin-worker isolates.

use deno_core::{JsRuntime, OpState, op2};
use deno_error::JsErrorBox;
use serde::Serialize;
use serde_bytes::ByteBuf;
use snowglass_vfs::{
    AsarVolumeFactory, FilesystemEndpoint, FilesystemPlan, HostDirectoryFactory, HostFileFactory,
    MemoryFileFactory, PackageDirectoryFactory, Permissions, PreparedFilesystemLaunch,
    ProfileLayout, Resource, ResourceFactory, ResourceOrigin, ScratchBacking, ScratchVolumeFactory,
    VfsError, VfsPathBuf, Volume,
};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Component as HostComponent, Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use crate::handle::Handle;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NamedResourceKind {
    Directory,
    Asar,
    File,
}

#[derive(Clone, Debug)]
pub struct NamedResource {
    pub name: String,
    pub path: PathBuf,
    pub kind: NamedResourceKind,
    pub permissions: Permissions,
}

#[derive(Clone, Debug)]
pub struct NamedCache {
    pub key: String,
    pub path: PathBuf,
}

pub type NandFactory = Arc<dyn ResourceFactory<dyn Volume>>;

#[derive(Clone)]
pub struct FilesystemOptions {
    pub package_root: PathBuf,
    pub resources: Vec<NamedResource>,
    pub caches: Vec<NamedCache>,
    pub nand: Option<NandFactory>,
}

impl std::fmt::Debug for FilesystemOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FilesystemOptions")
            .field("package_root", &self.package_root)
            .field("resources", &self.resources)
            .field("caches", &self.caches)
            .field("nand", &self.nand.is_some())
            .finish()
    }
}

impl FilesystemOptions {
    pub fn new(package_root: PathBuf) -> Self {
        Self {
            package_root,
            resources: Vec::new(),
            caches: Vec::new(),
            nand: None,
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
    caches: BTreeMap<String, CacheResource>,
    nand: Option<NandFactory>,
    used: bool,
    working_directory_set: bool,
    profile_set: bool,
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
            caches: BTreeMap::new(),
            nand: options.nand.clone(),
            used: false,
            working_directory_set: false,
            profile_set: false,
        };
        for resource in options.resources {
            if state.named.contains_key(&resource.name) {
                return Err(VfsError::Unsupported(format!(
                    "duplicate named resource '{}'",
                    resource.name
                )));
            }
            let handle =
                state.insert_path_resource(resource.path, resource.kind, resource.permissions)?;
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
        kind: NamedResourceKind,
        permissions: Permissions,
    ) -> Result<ResourceHandle, VfsError> {
        Ok(match kind {
            NamedResourceKind::Directory => {
                ResourceHandle::Tree(self.insert_volume(Resource::new(
                    Arc::new(PackageDirectoryFactory::new(path.clone())),
                    permissions,
                    ResourceOrigin::Package(path.display().to_string()),
                )))
            }
            NamedResourceKind::Asar => ResourceHandle::Tree(self.insert_volume(Resource::new(
                Arc::new(AsarVolumeFactory::new(path.clone())),
                permissions,
                ResourceOrigin::Asar(path.display().to_string()),
            ))),
            NamedResourceKind::File => ResourceHandle::File(self.insert_file(Resource::new(
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

    fn seal(self) -> Result<Option<PreparedFilesystemLaunch>, VfsError> {
        if !self.used {
            return Ok(None);
        }
        let filesystem = Arc::new(self.plan.seal()?);
        Ok(Some(PreparedFilesystemLaunch::new(
            self.endpoint,
            filesystem,
        )))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsResource {
    name: Option<String>,
    kind: &'static str,
    handle: String,
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
        NamedResourceKind::Directory
    } else if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("asar"))
    {
        NamedResourceKind::Asar
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
        .insert_path_resource(path, NamedResourceKind::File, authority)
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
    vfs.plan.mount_file(
        VfsPathBuf::new(target).map_err(js_error)?,
        resource,
        permissions,
    );
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
fn op_vfs_mount_nand(state: &mut OpState, #[string] target: String) -> Result<(), JsErrorBox> {
    let vfs = state.borrow_mut::<FilesystemState>();
    let factory = vfs
        .nand
        .clone()
        .ok_or_else(|| js_error("no nand was provided for this launch"))?;

    let resource = Resource::new(
        factory,
        Permissions::ALL,
        ResourceOrigin::Other("nand".to_owned()),
    );
    let handle = vfs.insert_volume(resource);
    let resource = vfs.volume(handle).map_err(js_error)?;
    vfs.plan.mount_tree(
        VfsPathBuf::new(target).map_err(js_error)?,
        resource,
        Permissions::ALL,
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

deno_core::extension!(snowglass_filesystem,
    ops = [
        op_vfs_resources, op_vfs_mount_cache, op_vfs_mount_host_tree, op_vfs_mount_host_file,
        op_vfs_mount_tree_pkg, op_vfs_mount_tree_resource, op_vfs_mount_file_pkg,
        op_vfs_mount_file_text, op_vfs_mount_file_bytes, op_vfs_mount_file_resource,
        op_vfs_mount_scratch, op_vfs_mount_nand, op_vfs_path, op_vfs_set_working_directory,
        op_vfs_set_profile,
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
        .take::<FilesystemState>()
        .seal()
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
}
