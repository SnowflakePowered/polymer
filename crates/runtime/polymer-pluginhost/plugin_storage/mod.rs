use dashmap::DashMap;
use deno_core::{OpState, op2};
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct PluginStorage {
    values: DashMap<Box<str>, serde_json::Value>,
}

impl PluginStorage {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.values.get(key).map(|value| value.value().clone())
    }

    fn set(&self, key: Box<str>, value: serde_json::Value) {
        self.values.insert(key, value);
    }

    pub(crate) fn clear(&self) {
        self.values.clear();
    }
}

#[op2]
#[serde]
fn op_plugin_storage_get(state: &mut OpState, #[string] key: String) -> Option<serde_json::Value> {
    state.borrow::<Arc<PluginStorage>>().get(&key)
}

#[op2]
fn op_plugin_storage_set(
    state: &mut OpState,
    #[string] key: String,
    #[serde] value: serde_json::Value,
) {
    state
        .borrow::<Arc<PluginStorage>>()
        .set(key.into_boxed_str(), value);
}

deno_core::extension!(
    polymer_plugin_storage,
    ops = [op_plugin_storage_get, op_plugin_storage_set],
    esm_entry_point = "ext:polymer_plugin_storage/00_plugin_storage.js",
    esm = [dir "plugin_storage/js", "00_plugin_storage.js"],
    options = { storage: Arc<PluginStorage> },
    state = |state, options| state.put(options.storage),
);

pub(crate) fn extension(storage: Arc<PluginStorage>) -> deno_core::Extension {
    polymer_plugin_storage::init(storage)
}
