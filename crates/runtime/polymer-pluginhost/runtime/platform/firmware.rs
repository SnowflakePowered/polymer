use super::super::{
    PluginInvocationError, PluginRuntimeHandle,
    js::ExtensionFunction,
    worker::{WorkerOptions, run_worker},
};
use crate::platform::PlatformPluginInstance;
use polymer_library::firmware::{FirmwareDescription, FirmwareInstallDescriptor, FirmwareStore};
use std::{collections::BTreeMap, path::Path, sync::Arc};

impl PluginRuntimeHandle {
    /// First valid nonempty result wins. The same immutable input is checked and installed.
    pub async fn install_firmware(
        &self,
        path: &Path,
    ) -> Result<Option<Vec<FirmwareDescription>>, PluginInvocationError> {
        if !tokio::fs::metadata(path)
            .await
            .map_err(PluginInvocationError::worker)?
            .is_file()
        {
            return Err(PluginInvocationError::worker(
                "firmware input must be a regular file",
            ));
        }
        let name = path
            .file_name()
            .ok_or_else(|| PluginInvocationError::worker("firmware filename is missing"))?
            .to_string_lossy()
            .into_owned();
        let input: Arc<[u8]> = tokio::fs::read(path)
            .await
            .map_err(PluginInvocationError::worker)?
            .into();
        let mut errors = Vec::new();
        for plugin in self.registry().plugins::<dyn PlatformPluginInstance>() {
            let result = self
                .check_firmware(plugin.clone(), name.clone(), input.clone())
                .await;
            let batch = match result {
                Ok(batch) => batch,
                Err(error) => {
                    tracing::warn!(plugin = plugin.id(), %error, "firmware recognizer failed");
                    errors.push(format!("{}: {error}", plugin.id()));
                    continue;
                }
            };
            if batch.is_empty() {
                continue;
            }
            let store = FirmwareStore::open(self.directories().firmware())
                .await
                .map_err(PluginInvocationError::worker)?;
            return store
                .install(&batch, plugin.platforms())
                .await
                .map(Some)
                .map_err(PluginInvocationError::worker);
        }
        if errors.is_empty() {
            Ok(None)
        } else {
            Err(PluginInvocationError::worker(format!(
                "No platform accepted the firmware. {}",
                errors.join("; ")
            )))
        }
    }

    async fn check_firmware(
        &self,
        plugin: Arc<dyn PlatformPluginInstance>,
        name: String,
        input: Arc<[u8]>,
    ) -> Result<Vec<FirmwareInstallDescriptor>, PluginInvocationError> {
        let mut worker =
            WorkerOptions::for_plugin(plugin.as_ref(), self.directories(), BTreeMap::new(), None)
                .map_err(PluginInvocationError::worker)?;
        worker.firmware.input = Some((name, input));
        let results = worker.firmware.results.clone();
        let entrypoint = plugin.entrypoint().to_string();
        let invocation_entrypoint = entrypoint.clone();
        let lifecycle = self.lifecycle(plugin.id());
        let cancellation = self.cancellation_token();
        self.run_with_control(cancellation.control, move |control| {
            run_worker(
                entrypoint,
                worker,
                lifecycle,
                control,
                move |mut runtime| async move {
                    let runtime = runtime.js_runtime_mut();
                    let check = ExtensionFunction::get(runtime, "__polymerFirmware", "check")
                        .map_err(PluginInvocationError::worker)?;
                    let argument = ExtensionFunction::argument(runtime, invocation_entrypoint)
                        .map_err(PluginInvocationError::worker)?;
                    check
                        .call(runtime, &[argument])
                        .await
                        .map_err(PluginInvocationError::worker)?;
                    Ok(())
                },
            )
        })
        .await?;
        let batch = std::mem::take(&mut *results.lock());
        FirmwareInstallDescriptor::validate_batch(&batch, plugin.platforms())
            .map_err(PluginInvocationError::worker)?;
        Ok(batch)
    }
}
