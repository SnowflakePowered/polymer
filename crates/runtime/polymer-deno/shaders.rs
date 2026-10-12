use super::PolymerHost;
use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use polymer_library::{ShaderLibrary, ShaderPresetEntry};
use std::{cell::RefCell, rc::Rc, sync::Arc};

fn shader_library(state: &Rc<RefCell<OpState>>) -> Result<ShaderLibrary, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.shaders().map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_shaders_search(
    state: Rc<RefCell<OpState>>,
    #[string] query: String,
    limit: u32,
) -> Result<Vec<ShaderPresetEntry>, JsErrorBox> {
    let shaders = shader_library(&state)?;
    tokio::task::spawn_blocking(move || shaders.search(&query, limit as usize))
        .await
        .map_err(JsErrorBox::from_err)
}

#[op2]
#[serde]
pub(super) async fn op_shaders_resolve(
    state: Rc<RefCell<OpState>>,
    #[string] relative_path: String,
) -> Result<Option<ShaderPresetEntry>, JsErrorBox> {
    let shaders = shader_library(&state)?;
    tokio::task::spawn_blocking(move || shaders.resolve(&relative_path))
        .await
        .map_err(JsErrorBox::from_err)
}
