//! Invocation-scoped Deno adapter used only by pluginhost-owned workers.

use crate::automation::{AutomationState, op_ui_automation_request};
use crate::{
    ConfigurationAccessError, EmulatorHostError, EmulatorLaunchArguments, EmulatorLaunchInvocation,
    GraphicsHook, InputHook, InputProvider, TrackedSurface,
    configuration::{ConfigurationCheckpoint, ConfigurationId, ConfigurationInstances},
};
use deno_core::{JsRuntime, OpState, op2, serde_v8, v8};
use deno_error::JsErrorBox;
use serde::Serialize;
use serde_bytes::ByteBuf;
use snowglass_config::ast::ConfigurationInstance;
use thiserror::Error;

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
        op_configuration_entries,
        op_configuration_render,
        op_ui_automation_request,
    ],
    esm_entry_point = "ext:snowglass_pluginhost_deno/configuration.js",
    esm = [dir "js", "configuration.js", "controller.js", "automation.js"],
    state = |state| {
        state.put(ConfigurationInstances::default());
        state.put(AutomationState::default());
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
        let configuration_id = pending.configuration_id;
        let launch = self
            .finish(pending)
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
        Ok(RetainedEmulator {
            launch,
            configuration_id,
            controller_changes,
        })
    }

    fn insert_configuration(
        &mut self,
        configuration: ConfigurationInstance,
        topology: &snowglass_input::topology::ControllerTopology,
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
                "onEmulatorLaunch arguments must be an object".to_owned(),
            ));
        }
        let topology = serde_json::from_value::<snowglass_input::topology::ControllerTopology>(
            arguments
                .get("controllerTopology")
                .cloned()
                .ok_or_else(|| {
                    DenoPluginError::Adapter(
                        "onEmulatorLaunch arguments require controllerTopology".to_owned(),
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
                    const receiver = !explicitDefault && typeof module.onEmulatorLaunch === "function"
                      ? module
                      : module.default;
                    const onEmulatorLaunch = receiver?.onEmulatorLaunch;
                    if (typeof onEmulatorLaunch !== "function") {{
                      throw new TypeError("plugin export 'onEmulatorLaunch' is not a function");
                    }}
                    const args = {arguments};
                    const configuration = __snowglassPluginHost.configuration({configuration_id});
                    args.configuration = configuration;
                    args.filesystem = __snowglassFilesystem.filesystem;
                    args.resources = __snowglassFilesystem.resources;
                    args.storage = __snowglassFilesystem.storage;
                    const controllerReceiver = typeof module.onControllerChanged === "function"
                      ? module
                      : receiver;
                    const onControllerChanged = controllerReceiver?.onControllerChanged;
                    const result = await Reflect.apply(onEmulatorLaunch, receiver, [args]);
                    const controllerChanges = typeof onControllerChanged === "function"
                      || __snowglassFilesystem.hasConfigurationMounts();
                    if (controllerChanges) {{
                      Object.defineProperty(globalThis, "__snowglassActiveControllerChange", {{
                        value: Object.freeze({{ receiver: controllerReceiver, callback: onControllerChanged, configuration }}),
                        configurable: false,
                        enumerable: false,
                        writable: false,
                      }});
                    }}
                    return {{
                      result,
                      configuration,
                      controllerChanges,
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
        let _ = local_object_value(result_value, "plugin onEmulatorLaunch result")?;
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
            input_hooks: plan.input_hooks,
            input_providers: plan.input_providers,
            graph_tracking: plan.graph_tracking,
            surfaces: plan.surfaces,
            prepared_filesystem: None,
            filesystem,
            controller_session: None,
            session: None,
            controller_topology: None,
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

pub(crate) async fn execute_controller_change(
    runtime: &mut JsRuntime,
    topology: &snowglass_input::topology::ControllerTopology,
    previous_topology: &snowglass_input::topology::ControllerTopology,
) -> Result<Vec<InputProvider>, EmulatorHostError> {
    let topology = serde_json::to_string(topology)
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    let previous = serde_json::to_string(previous_topology)
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    let script = format!(
        r#"(async () => {{
          const active = globalThis.__snowglassActiveControllerChange;
          if (active == null) throw new TypeError("plugin does not export onControllerChanged");
          const context = __snowglassControllerChange.context(
            {topology},
            {previous},
            active.configuration,
            __snowglassFilesystem.filesystem,
          );
          const result = typeof active.callback === "function"
            ? await Reflect.apply(active.callback, active.receiver, [context])
            : undefined;
          __snowglassFilesystem.refreshConfigurations();
          return result?.inputProviders ?? [];
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

pub(crate) struct RetainedEmulator {
    pub launch: EmulatorLaunchArguments,
    pub configuration_id: ConfigurationId,
    pub controller_changes: bool,
}

pub(crate) fn rematerialize_configuration(
    runtime: &mut JsRuntime,
    id: ConfigurationId,
    replacement: ConfigurationInstance,
    topology: &snowglass_input::topology::ControllerTopology,
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
    hooks: Vec<GraphicsHook>,
    #[serde(default)]
    input_hooks: Vec<InputHook>,
    #[serde(default)]
    input_providers: Vec<InputProvider>,
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
