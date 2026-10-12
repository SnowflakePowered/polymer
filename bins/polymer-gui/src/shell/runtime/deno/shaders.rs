use blitz_dom::NodeId;
use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use polymer_composition::ShaderControl;
use polymer_renderer::ShaderViewport;
use serde::Serialize;
use std::{cell::RefCell, rc::Rc};

use super::dom::ScriptNodeId;
use crate::shell::document::{LoadedPreset, PresetOptions, PresetParameter, SharedShellDocument};

#[derive(Serialize)]
pub(super) struct ShaderPresetHandle {
    id: u32,
    parameters: Vec<PresetParameter>,
    viewport: &'static str,
}

#[op2]
#[serde]
pub(super) async fn op_shader_preset_load(
    state: Rc<RefCell<OpState>>,
    #[string] url: String,
    #[serde] options: PresetOptions,
) -> Result<ShaderPresetHandle, JsErrorBox> {
    let document = state.borrow().borrow::<SharedShellDocument>().clone();
    let path = document
        .0
        .borrow()
        .shader_preset_path(&url)
        .map_err(JsErrorBox::type_error)?;
    let preset = tokio::task::spawn_blocking(move || LoadedPreset::load(path, options))
        .await
        .map_err(JsErrorBox::from_err)?
        .map_err(JsErrorBox::generic)?;
    let parameters = preset.parameters().to_vec();
    let viewport = match preset.viewport() {
        ShaderViewport::Content => "content",
        ShaderViewport::Element => "element",
    };
    let id = document.0.borrow_mut().insert_shader_preset(preset);
    Ok(ShaderPresetHandle {
        id,
        parameters,
        viewport,
    })
}

#[op2(fast)]
pub(super) fn op_shader_attach(state: &OpState, node: f64, preset: u32) -> Result<(), JsErrorBox> {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell
        .attach_shader(NodeId::from_script(node), preset)
        .map_err(JsErrorBox::type_error)
}

#[op2(fast)]
pub(super) fn op_shader_preset_drop(state: &OpState, preset: u32) {
    let shared = state.borrow::<SharedShellDocument>();
    shared.0.borrow_mut().drop_shader_preset(preset);
}

#[op2(fast)]
pub(super) fn op_shader_attached(state: &OpState, node: f64, attachment: u32) -> bool {
    let shared = state.borrow::<SharedShellDocument>();
    let shell = shared.0.borrow();
    shell.shader_attached(NodeId::from_script(node), attachment)
}

#[op2(fast)]
pub(super) fn op_shader_parameter(
    state: &OpState,
    node: f64,
    attachment: u32,
    #[string] name: String,
    value: f32,
) {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.control_shader(
        NodeId::from_script(node),
        attachment,
        ShaderControl::Parameter(name, value),
    );
}

#[op2(fast)]
pub(super) fn op_shader_passes(state: &OpState, node: f64, attachment: u32, passes: u32) {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.control_shader(
        NodeId::from_script(node),
        attachment,
        ShaderControl::Passes(passes as usize),
    );
}

#[op2(fast)]
pub(super) fn op_shader_detach(state: &OpState, node: f64, attachment: u32) {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.control_shader(NodeId::from_script(node), attachment, ShaderControl::Detach);
}
