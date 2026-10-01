use deno_core::{JsRuntime, serde_v8, v8};
use deno_error::JsErrorBox;
use serde::Serialize;

/// A function published by one of the pluginhost extension ESM modules.
pub(crate) struct ExtensionFunction(v8::Global<v8::Function>);

impl ExtensionFunction {
    pub(crate) fn get(
        runtime: &mut JsRuntime,
        namespace: &str,
        method: &str,
    ) -> Result<Self, JsErrorBox> {
        deno_core::scope!(scope, runtime);
        let global = scope.get_current_context().global(scope);
        let namespace_key = v8::String::new(scope, namespace)
            .ok_or_else(|| JsErrorBox::generic("invalid extension namespace"))?;
        let namespace = global
            .get(scope, namespace_key.into())
            .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok())
            .ok_or_else(|| JsErrorBox::generic("extension namespace is unavailable"))?;
        let method_key = v8::String::new(scope, method)
            .ok_or_else(|| JsErrorBox::generic("invalid extension method"))?;
        let function = namespace
            .get(scope, method_key.into())
            .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
            .ok_or_else(|| {
                JsErrorBox::generic(format!("extension method '{method}' is unavailable"))
            })?;
        Ok(Self(v8::Global::new(scope, function)))
    }

    pub(crate) fn argument(
        runtime: &mut JsRuntime,
        value: impl Serialize,
    ) -> Result<v8::Global<v8::Value>, JsErrorBox> {
        deno_core::scope!(scope, runtime);
        let value = serde_v8::to_v8(scope, value)
            .map_err(|error| JsErrorBox::generic(error.to_string()))?;
        Ok(v8::Global::new(scope, value))
    }

    /// Start a call without borrowing the runtime across promise resolution.
    pub(crate) fn start_call(
        &self,
        runtime: &mut JsRuntime,
        arguments: &[v8::Global<v8::Value>],
    ) -> impl std::future::Future<
        Output = Result<v8::Global<v8::Value>, Box<deno_core::error::JsError>>,
    > + use<> {
        runtime.call_with_args(&self.0, arguments)
    }

    pub(crate) async fn call(
        &self,
        runtime: &mut JsRuntime,
        arguments: &[v8::Global<v8::Value>],
    ) -> Result<v8::Global<v8::Value>, JsErrorBox> {
        #[allow(deprecated)]
        runtime
            .call_with_args_and_await(&self.0, arguments)
            .await
            .map_err(|error| JsErrorBox::generic(error.to_string()))
    }

    pub(crate) fn call_sync(
        &self,
        runtime: &mut JsRuntime,
        arguments: &[v8::Global<v8::Value>],
    ) -> Result<v8::Global<v8::Value>, JsErrorBox> {
        deno_core::scope!(scope, runtime);
        let function = v8::Local::new(scope, &self.0);
        let receiver = v8::undefined(scope).into();
        let arguments = arguments
            .iter()
            .map(|value| v8::Local::new(scope, value))
            .collect::<Vec<_>>();
        function
            .call(scope, receiver, &arguments)
            .map(|value| v8::Global::new(scope, value))
            .ok_or_else(|| JsErrorBox::generic("extension function threw an exception"))
    }
}

deno_core::extension!(
    polymer_plugin_runtime,
    ops = [super::events::op_plugin_emit_event, super::jobs::op_job_progress],
    esm_entry_point = "ext:polymer_plugin_runtime/00_invocation.js",
    esm = [dir "runtime/js", "00_invocation.js"],
);

pub(crate) fn extension() -> deno_core::Extension {
    polymer_plugin_runtime::init()
}
