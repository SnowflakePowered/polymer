use deno_core::{OpState, op2};
use serde_json::{Map, Value};

struct PluginContext {
    config: Map<String, Value>,
    manifest: Value,
}

#[op2]
#[serde]
fn op_plugin_context(state: &mut OpState) -> serde_json::Value {
    let context = state.borrow::<PluginContext>();
    serde_json::json!({ "config": context.config, "manifest": context.manifest })
}

deno_core::extension!(
    polymer_plugin_context,
    ops = [op_plugin_context],
    esm_entry_point = "ext:polymer_plugin_context/00_plugin_context.js",
    esm = [dir "plugin_context/js", "00_plugin_context.js"],
    options = { config: Map<String, Value>, manifest: Value },
    state = |state, options| state.put(PluginContext { config: options.config, manifest: options.manifest }),
);

pub(crate) fn extension(config: Map<String, Value>, manifest: Value) -> deno_core::Extension {
    polymer_plugin_context::init(config, manifest)
}
