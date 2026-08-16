use std::{cell::RefCell, rc::Rc};

use deno_core::{OpState, op2};
use deno_error::JsErrorBox;

#[derive(Clone)]
pub(super) struct PluginBridge(pub snowglass_pluginhost::PluginRuntimeHandle);

#[op2]
#[serde]
pub(super) async fn op_plugin_invoke(
    state: Rc<RefCell<OpState>>,
    #[string] plugin: String,
    #[serde] path: Vec<String>,
    #[serde] arguments: Vec<serde_json::Value>,
) -> Result<serde_json::Value, JsErrorBox> {
    let runtime = state.borrow().borrow::<PluginBridge>().0.clone();
    Ok(match runtime.invoke(&plugin, &path, arguments).await {
        Ok(value) => serde_json::json!({ "ok": true, "value": value }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    })
}
