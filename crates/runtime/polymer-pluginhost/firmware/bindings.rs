use super::FirmwareBinding;
use crate::filesystem::JsResource;
use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use polymer_library::firmware::FirmwareDescription;
use polymer_vfs::HostFileFactory;
use std::{cell::RefCell, rc::Rc, sync::Arc};

impl FirmwareBinding {
    fn platform(&self) -> Result<&str, JsErrorBox> {
        self.platform.as_deref().ok_or_else(|| {
            JsErrorBox::type_error(
                "firmwareStore requires a host-bound launch or bootstrap platform",
            )
        })
    }
}

#[op2]
#[serde]
async fn op_firmware_list(
    state: Rc<RefCell<OpState>>,
) -> Result<Vec<FirmwareDescription>, JsErrorBox> {
    let binding = state.borrow().borrow::<FirmwareBinding>().clone();
    let platform = binding.platform()?;
    binding
        .store()
        .await
        .map_err(|error| JsErrorBox::generic(error.to_string()))?
        .list(platform)
        .await
        .map_err(|error| JsErrorBox::generic(error.to_string()))
}

#[op2]
#[serde]
async fn op_firmware_open(
    state: Rc<RefCell<OpState>>,
    #[string] firmware_type: String,
    #[string] label: String,
) -> Result<JsResource, JsErrorBox> {
    let binding = state.borrow().borrow::<FirmwareBinding>().clone();
    let platform = binding.platform()?;
    let (name, path) = binding
        .store()
        .await
        .map_err(|error| JsErrorBox::generic(error.to_string()))?
        .file(platform, &firmware_type, &label)
        .await
        .map_err(|error| JsErrorBox::generic(error.to_string()))?;
    tokio::fs::metadata(&path)
        .await
        .map_err(|error| JsErrorBox::generic(error.to_string()))?;
    Ok(JsResource::readonly_file(
        &mut state.borrow_mut(),
        name,
        Arc::new(HostFileFactory::new(path, false)),
    ))
}

deno_core::extension!(polymer_firmware,
    ops = [op_firmware_list, op_firmware_open],
    esm_entry_point = "ext:polymer_firmware/00_firmware.js",
    esm = [dir "firmware", "00_firmware.js"],
    options = { binding: FirmwareBinding },
    state = |state, options| { state.put(options.binding); },
);

pub(crate) fn extension(binding: FirmwareBinding) -> deno_core::Extension {
    polymer_firmware::init(binding)
}
