//! Deno adapter for a runtime owned by the embedding host.
//!
//! [`EmulatorLaunch`] only borrows a [`deno_core::JsRuntime`]. The embedder
//! creates the runtime and includes [`extension`] in its runtime options.

use crate::{
    ConfigurationAccessError, EmulatorHostError, EmulatorLaunchArguments, EmulatorSetupInvocation,
    GraphicsHook, SerializedConfiguration, TrackedSurface, configuration,
    configuration::{ConfigurationId, ConfigurationInstances},
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
    ],
    esm_entry_point = "ext:snowglass_pluginhost_deno/configuration.js",
    esm = [dir "js", "configuration.js"],
    state = |state| {
        state.put(ConfigurationInstances::default());
    },
);

/// Creates the pluginhost extension for a runtime that the caller owns.
pub fn extension() -> deno_core::Extension {
    snowglass_pluginhost_deno::init()
}

/// One concrete emulator-plugin launch over a Deno runtime owned by the
/// embedding application.
pub struct EmulatorLaunch<'runtime> {
    runtime: &'runtime mut JsRuntime,
}

impl<'runtime> EmulatorLaunch<'runtime> {
    pub fn attach(runtime: &'runtime mut JsRuntime) -> Self {
        Self { runtime }
    }

    pub async fn execute(
        &mut self,
        invocation: EmulatorSetupInvocation,
    ) -> Result<EmulatorLaunchArguments, EmulatorHostError> {
        let pending = self
            .setup(invocation)
            .await
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
        let configuration = self
            .take_pending_configuration(&pending)
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
        let serialized = configuration::serialize(configuration)?;
        self.finish(pending, serialized)
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))
    }

    fn insert_configuration(&mut self, configuration: ConfigurationInstance) -> ConfigurationId {
        self.runtime
            .op_state()
            .borrow_mut()
            .borrow_mut::<ConfigurationInstances>()
            .insert(configuration)
    }

    fn take_configuration(
        &mut self,
        id: ConfigurationId,
    ) -> Result<ConfigurationInstance, ConfigurationAccessError> {
        self.runtime
            .op_state()
            .borrow_mut()
            .borrow_mut::<ConfigurationInstances>()
            .take(id)
    }

    fn discard_configuration(&mut self, id: ConfigurationId) {
        self.runtime
            .op_state()
            .borrow_mut()
            .borrow_mut::<ConfigurationInstances>()
            .discard(id);
    }
    async fn setup(
        &mut self,
        invocation: EmulatorSetupInvocation,
    ) -> Result<PendingDenoLaunch, DenoPluginError> {
        let EmulatorSetupInvocation {
            plugin,
            arguments,
            configuration,
        } = invocation;
        if !arguments.is_object() {
            return Err(DenoPluginError::Adapter(
                "setupLaunch arguments must be an object".to_owned(),
            ));
        }
        let entrypoint = serde_json::to_string(plugin.entrypoint().as_str())?;
        let arguments = serde_json::to_string(&arguments)?;
        let configuration_id = self.insert_configuration(configuration);
        let script = format!(
            r#"(async () => {{
                    const module = await import({entrypoint});
                    const plugin = module.default;
                    if (!plugin || typeof plugin.setupLaunch !== "function") {{
                      throw new TypeError("plugin default export has no setupLaunch function");
                    }}
                    const args = {arguments};
                    const configuration = __snowglassPluginHost.configuration({configuration_id});
                    args.emulatorConfig = configuration;
                    return {{
                      result: await plugin.setupLaunch(args),
                      configuration,
                    }};
                }})()"#,
            entrypoint = entrypoint,
            arguments = arguments,
            configuration_id = configuration_id,
        );
        let result = async {
            let promise = self
                .runtime
                .execute_script("snowglass:pluginhost/setup-launch.js", script)?;
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

    fn take_pending_configuration(
        &mut self,
        launch: &PendingDenoLaunch,
    ) -> Result<ConfigurationInstance, DenoPluginError> {
        // Take first so every inspection failure also clears the active slot.
        let active = self.take_configuration(launch.configuration_id)?;
        deno_core::scope!(scope, self.runtime);
        let wrapper = local_object(scope, &launch.value, "pluginhost launch result")?;
        let result_value = property(scope, wrapper, "result")?;
        let result = local_object_value(result_value, "plugin setupLaunch result")?;
        let returned = property(scope, result, "config")?;
        let configuration = property(scope, wrapper, "configuration")?;
        if !returned.strict_equals(configuration) {
            return Err(DenoPluginError::ForeignConfiguration);
        }
        Ok(active)
    }

    fn finish(
        &mut self,
        launch: PendingDenoLaunch,
        configuration: SerializedConfiguration,
    ) -> Result<EmulatorLaunchArguments, DenoPluginError> {
        deno_core::scope!(scope, self.runtime);
        let wrapper = local_object(scope, &launch.value, "pluginhost launch result")?;
        let value = property(scope, wrapper, "result")?;
        let plan: EmulatorLaunchPlan = serde_v8::from_v8(scope, value)?;
        Ok(EmulatorLaunchArguments {
            command: plan.command,
            argv: plan.argv,
            config_root: plan.config_root,
            config: configuration
                .into_iter()
                .map(|(path, bytes)| (path, ByteBuf::from(bytes)))
                .collect(),
            isolate: plan.isolate,
            hooks: plan.hooks,
            surfaces: plan.surfaces,
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct EmulatorLaunchPlan {
    command: String,
    #[serde(default)]
    argv: Vec<String>,
    config_root: String,
    #[serde(default)]
    isolate: bool,
    hooks: Vec<GraphicsHook>,
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
    #[error("plugin returned a foreign configuration object")]
    ForeignConfiguration,
    #[error("Deno plugin adapter: {0}")]
    Adapter(String),
    #[error(transparent)]
    Configuration(#[from] ConfigurationAccessError),
}
