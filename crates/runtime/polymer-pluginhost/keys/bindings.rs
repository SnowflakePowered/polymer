use super::KeyBinding;
use crate::filesystem::JsResource;
use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use polymer_consolestorage::keys::{
    KeyDescription, KeyIdentity, KeyQuery, KeyReader, KeyRecord, KeyWriter,
};
use polymer_vfs::MemoryFileFactory;
use std::{cell::RefCell, rc::Rc, sync::Arc};

#[op2]
#[serde]
async fn op_keys_list(
    state: Rc<RefCell<OpState>>,
    #[serde] query: KeyQuery,
) -> Result<Vec<KeyDescription>, JsErrorBox> {
    let binding = state.borrow().borrow::<KeyBinding>().clone();
    binding
        .platform_keys()
        .await
        .map_err(|e| JsErrorBox::generic(e.to_string()))?
        .list(&query)
        .await
        .map_err(|e| JsErrorBox::generic(e.to_string()))
}

#[op2]
#[serde]
async fn op_keys_open(
    state: Rc<RefCell<OpState>>,
    #[serde] identity: KeyIdentity,
) -> Result<JsResource, JsErrorBox> {
    let binding = state.borrow().borrow::<KeyBinding>().clone();
    let record = binding
        .platform_keys()
        .await
        .map_err(|e| JsErrorBox::generic(e.to_string()))?
        .read(&identity)
        .await
        .map_err(|e| JsErrorBox::generic(e.to_string()))?;
    Ok(JsResource::readonly_file(
        &mut state.borrow_mut(),
        identity.id,
        Arc::new(MemoryFileFactory::immutable(record.bytes)),
        "keys",
    ))
}

#[op2]
async fn op_keys_put(
    state: Rc<RefCell<OpState>>,
    #[serde] records: Vec<KeyRecord>,
) -> Result<(), JsErrorBox> {
    let binding = state.borrow().borrow::<KeyBinding>().clone();
    if !binding.allow_write {
        return Err(JsErrorBox::type_error(
            "keyStore is read-only in this context",
        ));
    }
    binding
        .platform_keys()
        .await
        .map_err(|e| JsErrorBox::generic(e.to_string()))?
        .put(&records)
        .await
        .map_err(|e| JsErrorBox::generic(e.to_string()))
}

#[op2]
async fn op_keys_remove(
    state: Rc<RefCell<OpState>>,
    #[serde] identity: KeyIdentity,
) -> Result<bool, JsErrorBox> {
    let binding = state.borrow().borrow::<KeyBinding>().clone();
    if !binding.allow_write {
        return Err(JsErrorBox::type_error(
            "keyStore is read-only in this context",
        ));
    }
    binding
        .platform_keys()
        .await
        .map_err(|e| JsErrorBox::generic(e.to_string()))?
        .remove(&identity)
        .await
        .map_err(|e| JsErrorBox::generic(e.to_string()))
}

deno_core::extension!(polymer_keys,
    ops = [op_keys_list, op_keys_open, op_keys_put, op_keys_remove],
    esm_entry_point = "ext:polymer_keys/00_keys.js",
    esm = [dir "keys", "00_keys.js"],
    options = { binding: KeyBinding },
    state = |state, options| { state.put(options.binding); },
);

pub(crate) fn extension(binding: KeyBinding) -> deno_core::Extension {
    polymer_keys::init(binding)
}
