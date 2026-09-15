mod bootstrap;
use super::{PluginInvocationError, PluginRuntimeHandle};
use crate::{
    emulator::EmulatorPluginInstance,
    platform::{Device, Operation, PreflightRequest, PreflightResult},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

impl PluginRuntimeHandle {
    pub async fn preflight(
        &self,
        request: PreflightRequest,
    ) -> Result<PreflightResult, PluginInvocationError> {
        self.preflight_with_cancellation(request, self.cancellation_token())
            .await
    }

    pub(crate) async fn preflight_with_cancellation(
        &self,
        request: PreflightRequest,
        cancellation: super::PluginCancellationToken,
    ) -> Result<PreflightResult, PluginInvocationError> {
        self.preflight_nested(request, cancellation, false).await
    }

    pub(crate) async fn preflight_nested(
        &self,
        request: PreflightRequest,
        cancellation: super::PluginCancellationToken,
        nested: bool,
    ) -> Result<PreflightResult, PluginInvocationError> {
        let mut report = request.inspect_requirements();
        if report.issues.iter().any(|issue| issue.code == "NAND_DIRTY") {
            return Ok(report);
        }
        let Some(provider) = self.registry().platform(&request.platform) else {
            return Ok(report);
        };
        let result = self
            .invoke_generic_nested(
                provider,
                &["onPreflight".into()],
                vec![serde_json::to_value(request).map_err(PluginInvocationError::worker)?],
                cancellation,
                nested,
            )
            .await?;
        let plugin: PreflightResult =
            serde_json::from_value(result.value).map_err(PluginInvocationError::worker)?;
        for issue in plugin.issues {
            if !report
                .issues
                .iter()
                .any(|current| current.code == issue.code && current.device == issue.device)
            {
                report.issues.push(issue);
            }
        }
        Ok(report)
    }

    pub(super) async fn launch_preflight(
        &self,
        emulator: &dyn EmulatorPluginInstance,
        arguments: &[Value],
    ) -> Result<(), PluginInvocationError> {
        let argument = arguments
            .first()
            .ok_or_else(|| PluginInvocationError::worker("Missing launch arguments"))?;
        let platform = argument["console"].as_str().unwrap_or_default();
        let mut requirements = emulator.preflight(platform);
        let mut devices = BTreeMap::new();
        let manager = polymer_library::StorageManager::new(self.directories().consolestorage());
        if let Some(selected) = argument.get("storage").and_then(Value::as_object) {
            for (name, selection) in selected {
                let selection =
                    polymer_library::StorageSelection::resolve(&manager, platform, selection)
                        .await
                        .map_err(PluginInvocationError::worker)?;
                let Some(id) = selection.nand else {
                    continue;
                };
                let device = Device {
                    platform: platform.into(),
                    id,
                };
                requirements
                    .entry(name.clone())
                    .or_insert_with(|| json!({"completion": "minimal"}));
                devices.insert(
                    name.clone(),
                    Some(
                        device
                            .inspect(&manager)
                            .await
                            .map_err(PluginInvocationError::worker)?,
                    ),
                );
            }
        }
        if requirements.is_empty() {
            return Ok(());
        }
        self.preflight(PreflightRequest {
            platform: platform.into(),
            operation: Operation::Launch,
            requirements,
            devices,
            package: argument.get("package").cloned(),
            depot_kind: None,
            settings: Value::Null,
        })
        .await?
        .ensure_ready()
    }
}
