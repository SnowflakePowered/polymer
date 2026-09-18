use super::*;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JsResource {
    name: Option<String>,
    kind: &'static str,
    handle: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsQuickResume {
    state: JsResource,
    screenshot: Option<JsResource>,
}

impl JsResource {
    pub(crate) fn readonly_file(
        state: &mut OpState,
        name: String,
        factory: Arc<dyn ResourceFactory<dyn FileNode>>,
        origin: &str,
    ) -> Self {
        let handle = state
            .borrow_mut::<FilesystemState>()
            .insert_file(Resource::new(
                factory,
                Permissions::READ,
                ResourceOrigin::Other(origin.into()),
            ));
        Self::new(Some(name), ResourceHandle::File(handle))
    }

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

#[op2]
#[serde]
fn op_vfs_import_package(
    state: &mut OpState,
    #[string] source: String,
) -> Result<JsResource, JsErrorBox> {
    let vfs = state.borrow_mut::<FilesystemState>();
    let path = vfs.package_path(&source).map_err(js_error)?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let handle = vfs.insert_path_resource(path).map_err(js_error)?;
    Ok(JsResource::new(name, handle))
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
fn op_vfs_cache_resource(
    state: &mut OpState,
    #[string] key: String,
) -> Result<JsResource, JsErrorBox> {
    let handle = state
        .borrow_mut::<FilesystemState>()
        .cache(&key)
        .map_err(js_error)?;
    Ok(JsResource::new(None, handle))
}

#[op2]
#[string]
fn op_vfs_mount_package_source(
    state: &mut OpState,
    #[string] target: String,
    #[string] source: String,
) -> Result<String, JsErrorBox> {
    state
        .borrow_mut::<FilesystemState>()
        .mount_package_source(target, PathBuf::from(source))
        .map_err(js_error)
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
    if !metadata.is_dir()
        && !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("asar"))
    {
        return Err(js_error(
            "mountTree pkg: source must be a directory or .asar file",
        ));
    }
    let vfs = state.borrow_mut::<FilesystemState>();
    let handle = vfs.insert_path_resource(path).map_err(js_error)?;
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
    let handle = vfs.insert_file(Resource::new(
        Arc::new(HostFileFactory::new(path.clone(), false)),
        authority,
        ResourceOrigin::Package(path.display().to_string()),
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
    if !resource.authority().contains(permissions) {
        return Err(js_error(VfsError::AuthorityExceeded {
            authority: resource.authority(),
            requested: permissions,
        }));
    }
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
    let vfs = state.borrow_mut::<FilesystemState>();
    let ResourceHandle::Tree(handle) = vfs.scratch_resource(&backing).map_err(js_error)? else {
        unreachable!("scratch resources are always trees")
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

deno_core::extension!(polymer_filesystem,
    ops = [
        op_vfs_import_package, op_vfs_storage, op_vfs_quick_resume,
        op_vfs_resource_entries, op_vfs_resource_bytes,
        op_vfs_resource_remove, op_vfs_scratch_resource,
        op_vfs_cache_resource, op_vfs_mount_host_tree, op_vfs_mount_host_file, op_vfs_mount_package_source,
        op_vfs_mount_tree_pkg, op_vfs_mount_tree_resource, op_vfs_mount_file_pkg,
        op_vfs_mount_file_text, op_vfs_mount_file_bytes, op_vfs_mount_file_resource,
        op_vfs_mount_scratch, op_vfs_mount_storage, op_vfs_path,
        op_vfs_set_working_directory, op_vfs_set_profile, op_vfs_flush,
    ],
    esm_entry_point = "ext:polymer_filesystem/00_filesystem.js",
    esm = [dir "filesystem/js", "00_filesystem.js"],
    options = { options: FilesystemOptions },
    state = |state, options| { state.put(FilesystemState::new(options.options).expect("validated filesystem options")); },
);

pub(super) fn extension(options: FilesystemOptions) -> deno_core::Extension {
    polymer_filesystem::init(options)
}
