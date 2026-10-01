use super::super::{
    PluginCancellationToken, PluginInvocationError, PluginRuntimeHandle,
    worker::{WorkerOptions, invoke_export, run_worker},
};
use polymer_library::jobs::JobHandle;
use polymer_library::{StorageInspection, StorageManager};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use uuid::Uuid;

/// The NAND an update check sees installed updates in. Without one, nothing counts as
/// installed.
pub struct UpdateTarget {
    pub manager: Arc<StorageManager>,
    pub platform: String,
    pub storage: Option<Uuid>,
}

impl PluginRuntimeHandle {
    /// Whether a platform plugin checks for and fetches title updates: it exports both
    /// `onCheckUpdates` and `onFetchUpdate`.
    pub async fn supports_updates(&self, plugin_id: &str) -> Result<bool, PluginInvocationError> {
        let exports = self.exports(plugin_id).await?;
        Ok(exports.contains("onCheckUpdates") && exports.contains("onFetchUpdate"))
    }

    /// Run the platform plugin's `onCheckUpdates` for one title, returning its candidates.
    /// The plugin can neither install nor download while checking.
    pub async fn check_updates(
        &self,
        target: UpdateTarget,
        title: Value,
        cancellation: PluginCancellationToken,
        job: Option<Arc<JobHandle>>,
    ) -> Result<Value, PluginInvocationError> {
        let UpdateTarget {
            manager,
            platform,
            storage,
        } = target;
        let inspection = match storage {
            Some(storage) => manager
                .inspect(&platform, storage)
                .await
                .map_err(PluginInvocationError::worker)?,
            None => StorageInspection {
                id: String::new(),
                platform: platform.clone(),
                minimal: false,
                dirty: false,
                artifacts: Vec::new(),
            },
        };
        let arguments = vec![json!(inspection), title, json!({})];
        self.update(
            &manager,
            &platform,
            "onCheckUpdates",
            arguments,
            cancellation,
            job,
        )
        .await
    }

    /// Run the platform plugin's `onFetchUpdate` for one candidate, which downloads the
    /// update into the plugin's cache, returning the depot to install: a file, or a
    /// directory holding every file of the depot.
    pub async fn fetch_update(
        &self,
        manager: &StorageManager,
        platform: &str,
        candidate: Value,
        cancellation: PluginCancellationToken,
        job: Option<Arc<JobHandle>>,
    ) -> Result<PathBuf, PluginInvocationError> {
        let arguments = vec![candidate, json!({})];
        let depot = self
            .update(
                manager,
                platform,
                "onFetchUpdate",
                arguments,
                cancellation,
                job,
            )
            .await?;
        depot
            .as_str()
            .map(PathBuf::from)
            .ok_or_else(|| PluginInvocationError::worker("onFetchUpdate returned no depot"))
    }

    async fn update(
        &self,
        manager: &StorageManager,
        platform: &str,
        export: &'static str,
        arguments: Vec<Value>,
        cancellation: PluginCancellationToken,
        job: Option<Arc<JobHandle>>,
    ) -> Result<Value, PluginInvocationError> {
        let provider = self
            .registry()
            .platform(platform)
            .ok_or_else(|| PluginInvocationError::worker("No platform plugin provides updates"))?;
        if !self.supports_updates(provider.id()).await? {
            return Err(PluginInvocationError::worker(format!(
                "{} does not provide updates",
                provider.id()
            )));
        }
        let mut worker =
            WorkerOptions::for_plugin(provider.as_ref(), self.directories(), BTreeMap::new(), None)
                .map_err(PluginInvocationError::worker)?;
        worker.firmware.platform = Some(platform.to_owned());
        worker.keys.root = manager.root().join("keys");
        worker.keys.platform = Some(platform.to_owned());
        worker.keys.allow_write = export == "onFetchUpdate";
        let entrypoint = provider.entrypoint().to_string();
        let invocation_entrypoint = entrypoint.clone();
        let lifecycle = self.lifecycle(provider.id());
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
                        &[export.into()],
                        arguments,
                        job,
                    )
                    .await
                },
            )
        })
        .await
    }
}
