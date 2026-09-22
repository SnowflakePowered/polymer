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
fn op_vfs_scratch_resource(state: &mut OpState) -> Result<JsResource, JsErrorBox> {
    state
        .borrow_mut::<FilesystemState>()
        .scratch_resource()
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
    Ok(JsResource::new(Some(key), handle))
}

#[op2]
#[string]
fn op_vfs_mount(
    state: &mut OpState,
    #[string] target: String,
    #[serde] source: super::mount::MountSource,
    #[string] mode: String,
) -> Result<String, JsErrorBox> {
    let target = VfsPathBuf::new(target).map_err(js_error)?;
    let permissions = Permissions::from_str(&mode).map_err(js_error)?;
    let vfs = state.borrow_mut::<FilesystemState>();
    let mounted = vfs.mount(target, source, permissions).map_err(js_error)?;
    Ok(vfs.endpoint.project(mounted.as_path()))
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
        op_vfs_cache_resource, op_vfs_mount, op_vfs_path,
        op_vfs_flush,
    ],
    esm_entry_point = "ext:polymer_filesystem/00_filesystem.js",
    esm = [dir "filesystem/js", "00_filesystem.js"],
    options = { options: FilesystemOptions },
    state = |state, options| { state.put(FilesystemState::new(options.options).expect("validated filesystem options")); },
);

pub(super) fn extension(options: FilesystemOptions) -> deno_core::Extension {
    polymer_filesystem::init(options)
}
