//! Invocation-scoped Deno adapter used only by pluginhost-owned workers.

use crate::{
    configuration::{
        ConfigurationAccessError, ConfigurationCheckpoint, ConfigurationId, ConfigurationInstances,
    },
    controller::PauseState,
    emulator::{EmulatorHostError, EmulatorLaunchArguments, EmulatorLaunchInvocation},
    runtime::js::ExtensionFunction,
};
use deno_core::{JsRuntime, serde_v8, v8};
use serde_bytes::ByteBuf;
use snowglass_config::ast::ConfigurationInstance;
use thiserror::Error;

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
    ) -> Result<RetainedEmulator, EmulatorHostError> {
        let save_states = invocation.plugin.save_states().cloned();
        let pending = self
            .launch(invocation)
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
        let state_save = self
            .supports(&pending, "stateSave")
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
        let state_resume = self
            .supports(&pending, "stateResume")
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
        let quit = self
            .supports(&pending, "quit")
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
        if save_states.is_some() && (!state_save || !state_resume) {
            return Err(EmulatorHostError::Runtime(
                "emulator.savestates requires onStateSave and onStateResume".to_owned(),
            ));
        }
        if save_states
            .as_ref()
            .is_some_and(|states| states.revision.is_some())
            && !quit
        {
            return Err(EmulatorHostError::Runtime(
                "emulator.savestates.revision requires onQuit".to_owned(),
            ));
        }
        let configuration_id = pending.configuration_id;
        let mut launch = self
            .finish(pending)
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
        launch.save_states = save_states;
        Ok(RetainedEmulator {
            launch,
            configuration_id,
            retained: controller_changes || pause_resume || state_save || state_resume || quit,
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
    ) -> Result<PendingDenoLaunch, DenoPluginError> {
        let EmulatorLaunchInvocation {
            plugin,
            arguments,
            configuration,
            storage: _,
            quick_resume: _,
            topology,
            explicit_default,
        } = invocation;
        if !arguments.is_object() {
            return Err(DenoPluginError::Adapter(
                "onLaunch arguments must be an object".to_owned(),
            ));
        }
        let configuration_id = self.insert_configuration(configuration, &topology);
        let result = async {
            let launch = ExtensionFunction::get(self.runtime, "__snowglassEmulator", "launch")
                .map_err(|error| DenoPluginError::Adapter(error.to_string()))?;
            let values = [
                ExtensionFunction::argument(self.runtime, plugin.entrypoint().as_str())
                    .map_err(|error| DenoPluginError::Adapter(error.to_string()))?,
                ExtensionFunction::argument(self.runtime, arguments)
                    .map_err(|error| DenoPluginError::Adapter(error.to_string()))?,
                ExtensionFunction::argument(self.runtime, configuration_id)
                    .map_err(|error| DenoPluginError::Adapter(error.to_string()))?,
                ExtensionFunction::argument(self.runtime, explicit_default)
                    .map_err(|error| DenoPluginError::Adapter(error.to_string()))?,
            ];
            let value = launch
                .call(self.runtime, &values)
                .await
                .map_err(|error| DenoPluginError::Adapter(error.to_string()))?;
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

    fn supports(
        &mut self,
        launch: &PendingDenoLaunch,
        name: &str,
    ) -> Result<bool, DenoPluginError> {
        deno_core::scope!(scope, self.runtime);
        let wrapper = local_object(scope, &launch.value, "pluginhost launch result")?;
        Ok(property(scope, wrapper, name)?.boolean_value(scope))
    }

    fn finish(
        &mut self,
        pending: PendingDenoLaunch,
    ) -> Result<EmulatorLaunchArguments, DenoPluginError> {
        let mut launch: EmulatorLaunchArguments = {
            deno_core::scope!(scope, self.runtime);
            let wrapper = local_object(scope, &pending.value, "pluginhost launch result")?;
            let value = property(scope, wrapper, "result")?;
            serde_v8::from_v8(scope, value)?
        };
        let (quick_resume_accepted, quick_resume_screenshot) = {
            deno_core::scope!(scope, self.runtime);
            let wrapper = local_object(scope, &pending.value, "pluginhost launch result")?;
            let accepted = property(scope, wrapper, "quickResumeAccepted")?.boolean_value(scope);
            let screenshot = property(scope, wrapper, "quickResumeScreenshot")?;
            let screenshot = if screenshot.is_null_or_undefined() {
                None
            } else {
                Some(serde_v8::from_v8::<ByteBuf>(scope, screenshot)?.into_vec())
            };
            (accepted, screenshot)
        };
        let filesystem = crate::filesystem::take_prepared(self.runtime)
            .map_err(|error| DenoPluginError::Adapter(error.to_string()))?;
        if let Some(filesystem) = &filesystem {
            filesystem
                .validate_command(&launch.command)
                .map_err(|error| DenoPluginError::Adapter(error.to_string()))?;
        }
        launch.filesystem = filesystem;
        launch.quick_resume_accepted = quick_resume_accepted;
        launch.quick_resume_screenshot = quick_resume_screenshot;
        Ok(launch)
    }
}

pub(crate) async fn execute_retained_emulator(
    runtime: &mut JsRuntime,
    invocation: EmulatorLaunchInvocation,
) -> Result<RetainedEmulator, EmulatorHostError> {
    EmulatorWorker::new(runtime)
        .execute_retained(invocation)
        .await
}

pub(crate) async fn execute_controller_change(
    runtime: &mut JsRuntime,
    topology: &snowglass_controllers::ControllerTopology,
    previous_topology: &snowglass_controllers::ControllerTopology,
) -> Result<snowglass_controllers::ControllerTopology, EmulatorHostError> {
    let callback = ExtensionFunction::get(runtime, "__snowglassEmulator", "controllerChanged")
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    let values = [
        ExtensionFunction::argument(runtime, topology)
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?,
        ExtensionFunction::argument(runtime, previous_topology)
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?,
    ];
    let value = callback
        .call(runtime, &values)
        .await
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    deno_core::scope!(scope, runtime);
    let value = v8::Local::new(scope, value);
    serde_v8::from_v8(scope, value).map_err(|error| EmulatorHostError::Runtime(error.to_string()))
}

pub(crate) async fn execute_pause_resume(
    runtime: &mut JsRuntime,
    pause_state: PauseState,
) -> Result<(), EmulatorHostError> {
    let callback = ExtensionFunction::get(runtime, "__snowglassEmulator", "pauseResume")
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    let values = [ExtensionFunction::argument(runtime, pause_state.as_str())
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?];
    callback
        .call(runtime, &values)
        .await
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    Ok(())
}

pub(crate) async fn execute_state_action(
    runtime: &mut JsRuntime,
    slot: u32,
    save: bool,
) -> Result<(), EmulatorHostError> {
    let callback = ExtensionFunction::get(runtime, "__snowglassEmulator", "stateAction")
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    let values = [
        ExtensionFunction::argument(runtime, slot)
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?,
        ExtensionFunction::argument(runtime, save)
            .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?,
    ];
    callback
        .call(runtime, &values)
        .await
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    Ok(())
}

pub(crate) async fn execute_quit(
    runtime: &mut JsRuntime,
) -> Result<Option<Vec<u8>>, EmulatorHostError> {
    let callback = ExtensionFunction::get(runtime, "__snowglassEmulator", "quit")
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    let value = callback
        .call(runtime, &[])
        .await
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))?;
    deno_core::scope!(scope, runtime);
    let value = v8::Local::new(scope, value);
    serde_v8::from_v8::<Option<ByteBuf>>(scope, value)
        .map(|value| value.map(ByteBuf::into_vec))
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))
}

pub(crate) struct RetainedEmulator {
    pub launch: EmulatorLaunchArguments,
    pub configuration_id: ConfigurationId,
    pub retained: bool,
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

pub(crate) fn refresh_configuration_mounts(
    runtime: &mut JsRuntime,
) -> Result<(), EmulatorHostError> {
    ExtensionFunction::get(runtime, "__snowglassEmulator", "refreshConfigurations")
        .and_then(|callback| callback.call_sync(runtime, &[]))
        .map(|_| ())
        .map_err(|error| EmulatorHostError::Runtime(error.to_string()))
}

pub(crate) fn discard_configuration(runtime: &mut JsRuntime, id: ConfigurationId) {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<ConfigurationInstances>()
        .discard(id);
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
    SerdeV8(#[from] serde_v8::Error),
    #[error("Deno plugin adapter: {0}")]
    Adapter(String),
    #[error(transparent)]
    Configuration(#[from] ConfigurationAccessError),
}
