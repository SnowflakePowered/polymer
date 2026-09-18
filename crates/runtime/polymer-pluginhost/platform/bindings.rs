use super::{DepotSource, Device};
use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use polymer_library::StorageManager;
use serde_json::json;
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};

#[derive(Clone)]
pub(crate) struct PlatformBinding {
    pub manager: Arc<StorageManager>,
    pub root: PathBuf,
    pub device: Option<Device>,
}

impl PlatformBinding {
    fn target(&self) -> Result<&Device, JsErrorBox> {
        self.device.as_ref().ok_or_else(|| {
            JsErrorBox::type_error(
                "NAND installation is only available during host-owned bootstrap",
            )
        })
    }
    fn path(&self, state: &OpState, source: &DepotSource) -> Result<PathBuf, JsErrorBox> {
        if let Some(cache) = &source.cache {
            return state
                .borrow::<crate::cache::CacheState>()
                .path(cache, &source.path);
        }
        if let Some(relative) = source.path.strip_prefix("pkg:") {
            let path = self
                .root
                .join(relative)
                .canonicalize()
                .map_err(JsErrorBox::from_err)?;
            if !path.starts_with(&self.root) {
                return Err(JsErrorBox::type_error("Resource escapes plugin package"));
            }
            return Ok(path);
        }
        let path = PathBuf::from(&source.path);
        Ok(if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        })
    }
}

#[op2]
#[serde]
fn op_platform_resource(
    state: &mut OpState,
    #[string] path: String,
) -> Result<serde_json::Value, JsErrorBox> {
    let binding = state.borrow::<PlatformBinding>();
    let path = binding.path(state, &DepotSource { path, cache: None })?;
    match std::fs::metadata(path) {
        Ok(metadata) => {
            Ok(json!({"exists": true, "size": metadata.len(), "directory": metadata.is_dir()}))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({"exists": false})),
        Err(error) => Err(JsErrorBox::from_err(error)),
    }
}

#[op2]
#[serde]
async fn op_platform_install(
    state: Rc<RefCell<OpState>>,
    #[serde] source: DepotSource,
) -> Result<serde_json::Value, JsErrorBox> {
    let (binding, path) = {
        let state = state.borrow();
        let binding = state.borrow::<PlatformBinding>().clone();
        let path = binding.path(&state, &source)?;
        (binding, path)
    };
    let events = state
        .borrow()
        .borrow::<Arc<crate::runtime::EventEmitter>>()
        .clone();
    let device = binding.target()?;
    let nand = device
        .open(&binding.manager)
        .await
        .map_err(JsErrorBox::generic)?;
    let key_root = state
        .borrow()
        .borrow::<crate::keys::KeyBinding>()
        .root
        .clone();
    let installation = nand
        .try_install(&path)
        .map_err(|error| JsErrorBox::generic(error.to_string()))?;
    let records = installation.embedded_keys();
    if !records.is_empty() {
        use polymer_consolestorage::keys::KeyWriter;
        let store = polymer_library::keys::KeyStore::open(key_root)
            .await
            .map_err(|error| JsErrorBox::generic(error.to_string()))?;
        store
            .for_platform(&device.platform)
            .map_err(|error| JsErrorBox::generic(error.to_string()))?
            .put(records)
            .await
            .map_err(|error| JsErrorBox::generic(error.to_string()))?;
    }
    DepotSource::finish_install(installation, |message| {
        events.publish(
            "loadprogress",
            json!({"state": "loading", "message": message}),
        );
    })
    .await
    .map_err(JsErrorBox::generic)
}

deno_core::extension!(polymer_platform,
    ops = [op_platform_resource, op_platform_install],
    esm_entry_point = "ext:polymer_platform/00_platform.js",
    esm = [dir "platform", "00_platform.js"],
    options = { binding: PlatformBinding },
    state = |state, options| { state.put(options.binding); },
);

pub(crate) fn extension(binding: PlatformBinding) -> deno_core::Extension {
    polymer_platform::init(binding)
}
