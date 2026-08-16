use std::{cell::RefCell, collections::HashMap, rc::Rc};

use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use snowglass_renderer::cli::ViewerOptions;

use crate::{configuration, input::GamepadMapping};

#[derive(Clone)]
pub(super) struct ConfigurationContext {
    pub plugins: snowglass_pluginhost::PluginRegistry,
    pub viewer: ViewerOptions,
}

#[derive(Default)]
pub(super) struct ConfigurationBridge(pub Option<ConfigurationContext>);

fn context(state: &OpState) -> Result<ConfigurationContext, JsErrorBox> {
    state
        .borrow::<ConfigurationBridge>()
        .0
        .clone()
        .ok_or_else(|| JsErrorBox::generic("configuration service is unavailable"))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PreparedConfiguration {
    plugin: String,
    arguments: serde_json::Value,
    gamepad_mapping: Vec<GamepadMapping>,
    machine: String,
    webrtc_bind: Option<String>,
    webrtc_targets: Vec<u32>,
}

#[op2]
#[serde]
pub(super) fn op_configuration_describe(
    state: &mut OpState,
    #[serde] package: snowglass_runtime::ScrapedPackage,
    #[string] preferred_emulator: Option<String>,
) -> Result<configuration::GameDescriptor, JsErrorBox> {
    let bridge = context(state)?;
    let detected = configuration::DetectedGame {
        path: package.path,
        platform: package.platform,
        preferred_emulator,
    };
    configuration::PreparedGame::new(
        detected,
        bridge
            .plugins
            .plugins::<dyn snowglass_pluginhost::EmulatorPluginInstance>(),
        bridge.viewer,
    )
    .map(|prepared| prepared.descriptor().clone())
    .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_configuration_prepare(
    state: Rc<RefCell<OpState>>,
    #[serde] package: snowglass_runtime::ScrapedPackage,
    #[serde] request: configuration::ConfigurationRequest,
) -> Result<PreparedConfiguration, JsErrorBox> {
    let bridge = context(&state.borrow())?;
    let settings = request.settings.clone();
    let emulator = request.emulator.clone();
    let prepared = configuration::PreparedGame::new(
        configuration::DetectedGame {
            path: package.path.clone(),
            platform: package.platform.clone(),
            preferred_emulator: Some(emulator.clone()),
        },
        bridge
            .plugins
            .plugins::<dyn snowglass_pluginhost::EmulatorPluginInstance>(),
        bridge.viewer,
    )
    .and_then(|prepared| prepared.prepare(request))
    .map_err(JsErrorBox::generic)?;
    let port_assignments = prepared
        .port_assignments
        .iter()
        .map(|(group, layouts)| {
            (
                group.to_string(),
                layouts.iter().map(ToString::to_string).collect::<Vec<_>>(),
            )
        })
        .collect::<HashMap<_, _>>();
    let webrtc_targets = prepared.viewer.webrtc.clone().unwrap_or_default();
    Ok(PreparedConfiguration {
        plugin: emulator,
        arguments: serde_json::json!({
            "launchPath": package.path.to_string_lossy(),
            "package": package,
            "console": prepared.viewer.machine,
            "portAssignments": port_assignments,
            "settings": settings,
        }),
        gamepad_mapping: prepared
            .viewer
            .gamepads
            .iter()
            .map(GamepadMapping::from)
            .collect(),
        machine: prepared.viewer.machine,
        webrtc_bind: prepared
            .viewer
            .webrtc
            .as_ref()
            .map(|_| prepared.viewer.bind_webrtc.to_string()),
        webrtc_targets,
    })
}
