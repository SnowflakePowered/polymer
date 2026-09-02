//! Invocation-scoped Deno adapter used only by pluginhost-owned workers.

use crate::actions::ActionsState;
use crate::automation::{AutomationState, op_ui_automation_request};
use crate::{
    ConfigurationAccessError, EmulatorHostError, EmulatorLaunchArguments, EmulatorLaunchInvocation,
    Hooks, TrackedSurface,
    configuration::{ConfigurationCheckpoint, ConfigurationId, ConfigurationInstances},
};
use deno_core::{JsRuntime, OpState, op2, serde_v8, v8};
use deno_error::JsErrorBox;
use serde::Serialize;
use serde_bytes::ByteBuf;
use snowglass_config::ast::ConfigurationInstance;
use thiserror::Error;

pub(crate) mod plugin_storage;

#[op2]
#[serde]
fn op_configuration_get(
    state: &mut OpState,
    id: ConfigurationId,
    #[string] path: String,
) -> Result<ConfigurationLookup, JsErrorBox> {
    let value = state
        .borrow::<ConfigurationInstances>()
        .get(id, &path)
        .map_err(|error| JsErrorBox::generic(error.to_string()))?;
    Ok(match value {
        Some(value) => ConfigurationLookup { found: true, value },
        None => ConfigurationLookup {
            found: false,
            value: serde_json::Value::Null,
        },
    })
}

#[op2]
fn op_configuration_set(
    state: &mut OpState,
    id: ConfigurationId,
    #[string] path: String,
    #[serde] value: serde_json::Value,
) -> Result<(), JsErrorBox> {
    state
        .borrow_mut::<ConfigurationInstances>()
        .set(id, &path, value)
        .map_err(|error| JsErrorBox::generic(error.to_string()))
}

#[op2]
fn op_configuration_repeats(
    state: &mut OpState,
    id: ConfigurationId,
    #[string] path: String,
) -> Result<bool, JsErrorBox> {
    state
        .borrow::<ConfigurationInstances>()
        .repeats(id, &path)
        .map_err(|error| JsErrorBox::generic(error.to_string()))
}

#[op2]
#[string]
fn op_configuration_append(
    state: &mut OpState,
    id: ConfigurationId,
    #[string] path: String,
) -> Result<String, JsErrorBox> {
    state
        .borrow_mut::<ConfigurationInstances>()
        .append(id, &path)
        .map(Into::into)
        .map_err(|error| JsErrorBox::generic(error.to_string()))
}

#[op2]
#[serde]
fn op_configuration_entries(
    state: &mut OpState,
    id: ConfigurationId,
) -> Result<Vec<(String, serde_json::Value)>, JsErrorBox> {
    state
        .borrow::<ConfigurationInstances>()
        .entries(id)
        .map_err(|error| JsErrorBox::generic(error.to_string()))
}

#[op2]
#[serde]
fn op_configuration_render(
    state: &mut OpState,
    id: ConfigurationId,
) -> Result<std::collections::BTreeMap<String, ByteBuf>, JsErrorBox> {
    Ok(state
        .borrow_mut::<ConfigurationInstances>()
        .render(id)
        .map_err(|error| JsErrorBox::generic(error.to_string()))?
        .into_iter()
        .map(|(path, bytes)| (path, ByteBuf::from(bytes)))
        .collect())
}

#[op2]
#[serde]
fn op_configuration_actions(
    state: &mut OpState,
    id: ConfigurationId,
) -> Result<std::collections::BTreeMap<String, String>, JsErrorBox> {
    state
        .borrow::<ConfigurationInstances>()
        .actions(id)
        .map_err(|error| JsErrorBox::generic(error.to_string()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigurationLookup {
    found: bool,
    value: serde_json::Value,
}

deno_core::extension!(
    snowglass_pluginhost_deno,
    ops = [
        op_configuration_get,
        op_configuration_set,
        op_configuration_repeats,
        op_configuration_append,
        op_configuration_entries,
        op_configuration_render,
        op_configuration_actions,
        crate::actions::op_actions_register,
        crate::actions::op_action_activate,
        op_ui_automation_request,
    ],
    esm_entry_point = "ext:snowglass_pluginhost_deno/configuration.js",
    esm = [dir "js", "configuration.js", "controller.js", "automation.js", "actions.js"],
    state = |state| {
        state.put(ConfigurationInstances::default());
        state.put(AutomationState::default());
        state.put(ActionsState::default());
    },
);

pub(crate) fn extension() -> deno_core::Extension {
    snowglass_pluginhost_deno::init()
}

struct EmulatorWorker<'runtime> {
    runtime: &'runtime mut JsRuntime,
}

impl<'runtime> EmulatorWorker<'runtime> {
    fn new(runtime: &'runtime mut JsRuntime) -> Self {
        Self { runtime }
    }

    pub async fn execute_retained(
        &mut self,
        invocation: EmulatorLaunchInvocation,
        explicit_default: bool,
    ) -> Result<RetainedEmulator, EmulatorHostError> {
        let pending = self
            .launch(invocation, explicit_default)
            .await
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
        self.validate_pending_configuration(&pending)
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
        let controller_changes = self
            .supports_controller_changes(&pending)
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
        let pause_resume = self
            .supports_pause_resume(&pending)
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
        let configuration_id = pending.configuration_id;
        let launch = self
            .finish(pending)
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
        Ok(RetainedEmulator {
            launch,
            configuration_id,
            controller_changes,
            pause_resume,
        })
    }

    fn insert_configuration(
        &mut self,
        configuration: ConfigurationInstance,
        topology: &snowglass_controllers::ControllerTopology,
    ) -> ConfigurationId {
        self.runtime
            .op_state()
            .borrow_mut()
            .borrow_mut::<ConfigurationInstances>()
            .insert(configuration, topology)
    }

    fn discard_configuration(&mut self, id: ConfigurationId) {
        self.runtime
            .op_state()
            .borrow_mut()
            .borrow_mut::<ConfigurationInstances>()
            .discard(id);
    }
    async fn launch(
        &mut self,
        invocation: EmulatorLaunchInvocation,
        explicit_default: bool,
    ) -> Result<PendingDenoLaunch, DenoPluginError> {
        let EmulatorLaunchInvocation {
            plugin,
            arguments,
            configuration,
            storage: _,
        } = invocation;
        if !arguments.is_object() {
            return Err(DenoPluginError::Adapter(
                "onLaunch arguments must be an object".to_owned(),
            ));
        }
        let topology = serde_json::from_value::<snowglass_controllers::ControllerTopology>(
            arguments
                .get("controllerTopology")
                .cloned()
                .ok_or_else(|| {
                    DenoPluginError::Adapter(
                        "onLaunch arguments require controllerTopology".to_owned(),
                    )
                })?,
        )?;
        let entrypoint = serde_json::to_string(plugin.entrypoint().as_str())?;
        let arguments = serde_json::to_string(&arguments)?;
        let configuration_id = self.insert_configuration(configuration, &topology);
        let script = format!(
            r#"(async () => {{
                    const module = await import({entrypoint});
                    const explicitDefault = {explicit_default};
                    const receiver = !explicitDefault && typeof module.onLaunch === "function"
                      ? module
                      : module.default;
                    const onLaunch = receiver?.onLaunch;
                    if (typeof onLaunch !== "function") {{
                      throw new TypeError("plugin export 'onLaunch' is not a function");
                    }}
                    const args = {arguments};
                    const configuration = __snowglassPluginHost.configuration({configuration_id});
                    args.configuration = configuration;
                    args.filesystem = __snowglassFilesystem.filesystem;
                    args.resources = __snowglassFilesystem.resources;
                    args.storage = __snowglassFilesystem.storage;
                    const parserReceiver = typeof module.onParseHotkey === "function"
                      ? module
                      : receiver;
                    const actions = __snowglassActions.create(
                      {configuration_id},
                      parserReceiver?.onParseHotkey,
                    );
                    args.actions = actions;
                    const controllerReceiver = typeof module.onControllerChanged === "function"
                      ? module
                      : receiver;
                    const onControllerChanged = controllerReceiver?.onControllerChanged;
                    const pauseReceiver = typeof module.onPauseResume === "function"
                      ? module
                      : receiver;
                    const onPauseResume = pauseReceiver?.onPauseResume;
                    const result = await Reflect.apply(onLaunch, receiver, [args]);
                    const controllerChanges = typeof onControllerChanged === "function"
                      || __snowglassFilesystem.hasConfigurationMounts();
                    const pauseResume = typeof onPauseResume === "function";
                    if (controllerChanges || pauseResume) {{
                      Object.defineProperty(globalThis, "__snowglassActiveEmulator", {{
                        value: Object.freeze({{
                          controllerReceiver,
                          onControllerChanged,
                          pauseReceiver,
                          onPauseResume,
                          configuration,
                          actions,
                        }}),
                        configurable: false,
                        enumerable: false,
                        writable: false,
                      }});
                    }}
                    return {{
                      result,
                      configuration,
                      controllerChanges,
                      pauseResume,
                    }};
                }})()"#,
            entrypoint = entrypoint,
            arguments = arguments,
            configuration_id = configuration_id,
            explicit_default = explicit_default,
        );
        let result = async {
            let promise = self
                .runtime
                .execute_script("snowglass:pluginhost/emulator-launch.js", script)?;
            #[allow(deprecated)]
            let value = self.runtime.resolve_value(promise).await?;
            Ok(PendingDenoLaunch {
                value,
                configuration_id,
            })
        }
        .await;
        if result.is_err() {
            self.discard_configuration(configuration_id);
        }
        result
    }

    fn validate_pending_configuration(
        &mut self,
        launch: &PendingDenoLaunch,
    ) -> Result<(), DenoPluginError> {
        self.runtime
            .op_state()
            .borrow_mut()
            .borrow::<ConfigurationInstances>()
            .get(launch.configuration_id, "")?;
        deno_core::scope!(scope, self.runtime);
        let wrapper = local_object(scope, &launch.value, "pluginhost launch result")?;
        let result_value = property(scope, wrapper, "result")?;
        let _ = local_object_value(result_value, "plugin onLaunch result")?;
        Ok(())
    }

    fn supports_controller_changes(
        &mut self,
        launch: &PendingDenoLaunch,
    ) -> Result<bool, DenoPluginError> {
        deno_core::scope!(scope, self.runtime);
        let wrapper = local_object(scope, &launch.value, "pluginhost launch result")?;
        Ok(property(scope, wrapper, "controllerChanges")?.boolean_value(scope))
    }

    fn supports_pause_resume(
        &mut self,
        launch: &PendingDenoLaunch,
    ) -> Result<bool, DenoPluginError> {
        deno_core::scope!(scope, self.runtime);
        let wrapper = local_object(scope, &launch.value, "pluginhost launch result")?;
        Ok(property(scope, wrapper, "pauseResume")?.boolean_value(scope))
    }

    fn finish(
        &mut self,
        launch: PendingDenoLaunch,
    ) -> Result<EmulatorLaunchArguments, DenoPluginError> {
        let plan: EmulatorLaunchPlan = {
            deno_core::scope!(scope, self.runtime);
            let wrapper = local_object(scope, &launch.value, "pluginhost launch result")?;
            let value = property(scope, wrapper, "result")?;
            serde_v8::from_v8(scope, value)?
        };
        let filesystem = crate::filesystem::take_prepared(self.runtime)
            .map_err(|error| DenoPluginError::Adapter(error.to_string()))?;
        if let Some(filesystem) = &filesystem {
            filesystem
                .validate_command(&plan.command)
                .map_err(|error| DenoPluginError::Adapter(error.to_string()))?;
        }
        Ok(EmulatorLaunchArguments {
            command: plan.command,
            argv: plan.argv,
            isolate: plan.isolate,
            hooks: plan.hooks,
            graph_tracking: plan.graph_tracking,
            surfaces: plan.surfaces,
            prepared_filesystem: None,
            filesystem,
            controller_session: None,
            session: None,
            controller_topology: plan.controller_topology,
        })
    }
}

pub(crate) async fn execute_retained_emulator(
    runtime: &mut JsRuntime,
    invocation: EmulatorLaunchInvocation,
    explicit_default: bool,
) -> Result<RetainedEmulator, EmulatorHostError> {
    EmulatorWorker::new(runtime)
        .execute_retained(invocation, explicit_default)
        .await
}

#[derive(serde::Deserialize)]
pub(crate) struct ControllerChangeOutput {
    #[serde(rename = "controllerTopology")]
    pub controller_topology: snowglass_controllers::ControllerTopology,
}

pub(crate) async fn execute_controller_change(
    runtime: &mut JsRuntime,
    topology: &snowglass_controllers::ControllerTopology,
    previous_topology: &snowglass_controllers::ControllerTopology,
) -> Result<ControllerChangeOutput, EmulatorHostError> {
    let topology = serde_json::to_string(topology)
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    let previous = serde_json::to_string(previous_topology)
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    let script = format!(
        r#"(async () => {{
          const active = globalThis.__snowglassActiveEmulator;
          if (active == null) throw new TypeError("plugin does not export onControllerChanged");
          const inputTopology = {topology};
          const context = __snowglassControllerChange.context(
            inputTopology,
            {previous},
            active.configuration,
            __snowglassFilesystem.filesystem,
            active.actions,
          );
          const result = typeof active.onControllerChanged === "function"
            ? await Reflect.apply(active.onControllerChanged, active.controllerReceiver, [context])
            : undefined;
          __snowglassFilesystem.refreshConfigurations();
          return {{
            controllerTopology: result?.controllerTopology ?? inputTopology,
          }};
        }})()"#
    );
    let promise = runtime
        .execute_script("snowglass:pluginhost/controller-changed.js", script)
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    #[allow(deprecated)]
    let value = runtime
        .resolve_value(promise)
        .await
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    deno_core::scope!(scope, runtime);
    let value = v8::Local::new(scope, value);
    serde_v8::from_v8(scope, value).map_err(|error| EmulatorHostError::Runtime(error.to_string()))
}

pub(crate) async fn execute_pause_resume(
    runtime: &mut JsRuntime,
    pause_state: crate::PauseState,
) -> Result<(), EmulatorHostError> {
    let pause_state = serde_json::to_string(pause_state.as_str())
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    let script = format!(
        r#"(async () => {{
          const active = globalThis.__snowglassActiveEmulator;
          if (active == null || typeof active.onPauseResume !== "function") return;
          const context = __snowglassControllerChange.pauseContext({pause_state}, active.actions);
          await Reflect.apply(active.onPauseResume, active.pauseReceiver, [context]);
        }})()"#
    );
    let promise = runtime
        .execute_script("snowglass:pluginhost/pause-resume.js", script)
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    #[allow(deprecated)]
    runtime
        .resolve_value(promise)
        .await
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    Ok(())
}

pub(crate) struct RetainedEmulator {
    pub launch: EmulatorLaunchArguments,
    pub configuration_id: ConfigurationId,
    pub controller_changes: bool,
    pub pause_resume: bool,
}

pub(crate) fn rematerialize_configuration(
    runtime: &mut JsRuntime,
    id: ConfigurationId,
    replacement: ConfigurationInstance,
    topology: &snowglass_controllers::ControllerTopology,
) -> Result<(), EmulatorHostError> {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<ConfigurationInstances>()
        .rematerialize(id, replacement, topology)?;
    Ok(())
}

pub(crate) fn checkpoint_configuration(
    runtime: &mut JsRuntime,
    id: ConfigurationId,
) -> Result<ConfigurationCheckpoint, EmulatorHostError> {
    Ok(runtime
        .op_state()
        .borrow()
        .borrow::<ConfigurationInstances>()
        .checkpoint(id)?)
}

pub(crate) fn restore_configuration(
    runtime: &mut JsRuntime,
    id: ConfigurationId,
    checkpoint: ConfigurationCheckpoint,
) {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<ConfigurationInstances>()
        .restore(id, checkpoint);
}

pub(crate) fn discard_configuration(runtime: &mut JsRuntime, id: ConfigurationId) {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<ConfigurationInstances>()
        .discard(id);
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmulatorLaunchPlan {
    command: String,
    #[serde(default)]
    argv: Vec<String>,
    #[serde(default)]
    isolate: bool,
    hooks: Hooks,
    #[serde(default)]
    controller_topology: Option<snowglass_controllers::ControllerTopology>,
    #[serde(default)]
    graph_tracking: bool,
    surfaces: Vec<TrackedSurface>,
}

struct PendingDenoLaunch {
    value: v8::Global<v8::Value>,
    configuration_id: ConfigurationId,
}

fn local_object_value<'scope>(
    value: v8::Local<'scope, v8::Value>,
    description: &str,
) -> Result<v8::Local<'scope, v8::Object>, DenoPluginError> {
    v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| DenoPluginError::Adapter(format!("{description} must be an object")))
}

fn local_object<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    value: &v8::Global<v8::Value>,
    description: &str,
) -> Result<v8::Local<'scope, v8::Object>, DenoPluginError> {
    let value = v8::Local::new(scope, value);
    v8::Local::<v8::Object>::try_from(value)
        .map_err(|_| DenoPluginError::Adapter(format!("{description} must be an object")))
}

fn property<'scope>(
    scope: &mut v8::PinScope<'scope, '_>,
    object: v8::Local<'scope, v8::Object>,
    name: &str,
) -> Result<v8::Local<'scope, v8::Value>, DenoPluginError> {
    let key = v8::String::new(scope, name)
        .ok_or_else(|| DenoPluginError::Adapter(format!("invalid property name '{name}'")))?;
    object
        .get(scope, key.into())
        .ok_or_else(|| DenoPluginError::Adapter(format!("property '{name}' is unavailable")))
}

#[derive(Debug, Error)]
pub enum DenoPluginError {
    #[error(transparent)]
    JavaScript(#[from] Box<deno_core::error::JsError>),
    #[error(transparent)]
    Core(#[from] deno_core::error::CoreError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    SerdeV8(#[from] serde_v8::Error),
    #[error("Deno plugin adapter: {0}")]
    Adapter(String),
    #[error(transparent)]
    Configuration(#[from] ConfigurationAccessError),
}
