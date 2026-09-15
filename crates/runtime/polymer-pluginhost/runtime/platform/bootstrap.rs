use super::super::{
    PluginInvocationError, PluginRuntimeHandle,
    worker::{WorkerOptions, invoke_export, run_worker},
};
use crate::platform::Device;
use serde_json::{Value, json};
use std::collections::BTreeMap;

impl PluginRuntimeHandle {
    pub async fn bootstrap(
        &self,
        device: Device,
        settings: Value,
        cancellation: super::super::PluginCancellationToken,
    ) -> Result<Value, PluginInvocationError> {
        let provider = self
            .registry()
            .platform(&device.platform)
            .ok_or_else(|| PluginInvocationError::worker("Platform provider unavailable"))?;
        let mut worker =
            WorkerOptions::for_plugin(provider.as_ref(), self.directories(), BTreeMap::new(), None)
                .map_err(PluginInvocationError::worker)?;
        worker.platform.device = Some(device.clone());
        let entrypoint = provider.entrypoint().to_string();
        let invocation_entrypoint = entrypoint.clone();
        let lifecycle = self.lifecycle(provider.id());
        let updates = lifecycle.updates.clone();
        let _bootstrap = tokio::select! {
            guard = updates.lock() => guard,
            () = cancellation.control.cancelled_notification() => return Err(PluginInvocationError::cancelled()),
        };
        let inspection = device
            .inspect(&polymer_library::StorageManager::new(
                self.directories().consolestorage(),
            ))
            .await
            .map_err(PluginInvocationError::worker)?;
        let events = lifecycle.events.clone();
        let progress = events.scope(
            "loadprogress",
            json!({"state": "loading", "message": "Installing system files"}),
            json!({"state": "failed", "message": "System setup interrupted"}),
        );
        self.run_with_control(cancellation.control, move |control| {
            run_worker(
                entrypoint,
                worker,
                lifecycle.clone(),
                control,
                move |mut runtime| async move {
                    invoke_export(
                        runtime.js_runtime_mut(),
                        &invocation_entrypoint,
                        &["onBootstrap".into()],
                        vec![json!(inspection), json!({"settings": settings})],
                    )
                    .await
                },
            )
        })
        .await?;
        let manager = polymer_library::StorageManager::new(self.directories().consolestorage());
        let inspection = device
            .inspect(&manager)
            .await
            .map_err(PluginInvocationError::worker)?;

        progress.finish(json!({"state": "ready", "message": "System setup complete"}));
        Ok(json!(inspection))
    }
}
