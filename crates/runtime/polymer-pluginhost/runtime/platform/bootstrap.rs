use super::super::{
    PluginInvocationError, PluginRuntimeHandle,
    jobs::PluginJobs,
    worker::{WorkerOptions, invoke_export, run_worker},
};
use polymer_library::jobs::JobHandle;
use polymer_library::{StorageInspection, StorageManager};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use uuid::Uuid;

impl PluginRuntimeHandle {
    pub async fn bootstrap(
        &self,
        manager: Arc<StorageManager>,
        platform: &str,
        id: Uuid,
        settings: Value,
        cancellation: super::super::PluginCancellationToken,
        job: Option<Arc<JobHandle>>,
    ) -> Result<StorageInspection, PluginInvocationError> {
        let provider = self
            .registry()
            .platform(platform)
            .ok_or_else(|| PluginInvocationError::worker("Platform provider unavailable"))?;
        let mut worker =
            WorkerOptions::for_plugin(provider.as_ref(), self.directories(), BTreeMap::new(), None)
                .map_err(PluginInvocationError::worker)?;
        worker.platform.target = Some((manager.clone(), platform.to_owned(), id));
        worker.firmware.platform = Some(platform.to_owned());
        worker.keys.root = manager.root().join("keys");
        worker.keys.platform = Some(platform.to_owned());
        worker.keys.allow_write = true;
        let entrypoint = provider.entrypoint().to_string();
        let invocation_entrypoint = entrypoint.clone();
        let lifecycle = self.lifecycle(provider.id());
        let updates = lifecycle.updates.clone();
        let _bootstrap = tokio::select! {
            guard = updates.lock() => guard,
            () = cancellation.control.cancelled_notification() => return Err(PluginInvocationError::cancelled()),
        };
        let inspection = manager
            .inspect(platform, id)
            .await
            .map_err(PluginInvocationError::worker)?;
        self.run_with_control(cancellation.control, move |control| {
            run_worker(
                entrypoint,
                worker,
                lifecycle.clone(),
                control,
                move |mut runtime| async move {
                    PluginJobs::swap(&mut runtime.js_runtime_mut().op_state().borrow_mut(), job);
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
        let inspection = manager
            .inspect(platform, id)
            .await
            .map_err(PluginInvocationError::worker)?;
        Ok(inspection)
    }
}
