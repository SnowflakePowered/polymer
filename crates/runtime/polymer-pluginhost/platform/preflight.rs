use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub type Requirements = BTreeMap<String, Value>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreflightRequest {
    pub platform: String,
    pub operation: Operation,
    #[serde(default)]
    pub requirements: Requirements,
    #[serde(default)]
    pub devices: BTreeMap<String, Option<DeviceInspection>>,
    #[serde(default)]
    pub settings: Value,
    #[serde(default)]
    pub depot_kind: Option<polymer_consolestorage::installable::DepotType>,
    #[serde(default)]
    pub package: Option<Value>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Operation {
    Launch,
    Install,
    Bootstrap,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInspection {
    pub id: String,
    pub platform: String,
    pub minimal: bool,
    pub dirty: bool,
    pub artifacts: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreflightResult {
    pub issues: Vec<PreflightIssue>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreflightIssue {
    pub code: String,
    pub message: String,
    pub severity: Severity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    Blocking,
    Warning,
}

impl PreflightResult {
    pub fn ensure_ready(&self) -> Result<(), crate::runtime::PluginInvocationError> {
        if self
            .issues
            .iter()
            .any(|issue| issue.severity == Severity::Blocking)
        {
            let mut error = crate::runtime::PluginInvocationError::new(
                "PreflightError",
                "PREFLIGHT_BLOCKED",
                self.issues
                    .iter()
                    .filter(|issue| issue.severity == Severity::Blocking)
                    .map(|issue| issue.message.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            error.details = Some(serde_json::to_value(self).expect("preflight is serializable"));
            return Err(error);
        }
        Ok(())
    }
}

impl PreflightRequest {
    pub(crate) fn inspect_requirements(&self) -> PreflightResult {
        let mut report = PreflightResult::default();
        for (name, device) in &self.devices {
            if device.as_ref().is_some_and(|device| device.dirty) {
                report.issues.push(PreflightIssue {
                    code: "NAND_DIRTY".into(),
                    message: "This NAND has uncommitted changes. Recover it in Console storage before continuing.".into(),
                    severity: Severity::Blocking,
                    device: Some(name.clone()),
                });
            }
        }
        for (name, requirement) in &self.requirements {
            let issue = match self.devices.get(name).and_then(Option::as_ref) {
                None => Some((
                    "DEVICE_REQUIRED",
                    format!("Select or create a NAND for {name}"),
                )),
                Some(device) => device.check_requirement(self, requirement),
            };
            if let Some((code, message)) = issue {
                report.issues.push(PreflightIssue {
                    code: code.into(),
                    message,
                    severity: Severity::Blocking,
                    device: Some(name.clone()),
                });
            }
        }
        report
    }
}

impl DeviceInspection {
    fn check_requirement(
        &self,
        request: &PreflightRequest,
        requirement: &Value,
    ) -> Option<(&'static str, String)> {
        if self.platform != request.platform {
            return Some((
                "PLATFORM_MISMATCH",
                "NAND belongs to a different platform".into(),
            ));
        }
        let completion = requirement.get("completion").and_then(Value::as_str);
        if (request.operation == Operation::Launch || completion == Some("minimal"))
            && !self.minimal
        {
            return Some((
                "NAND_INCOMPLETE",
                "This NAND needs system files before it can be used".into(),
            ));
        }
        match completion {
            Some("full" | "verified") => Some((
                "VERIFICATION_UNSUPPORTED",
                "Full NAND verification is not implemented".into(),
            )),
            Some(other) if other != "minimal" => Some((
                "UNKNOWN_REQUIREMENT",
                format!("Unknown completion requirement {other}"),
            )),
            _ => None,
        }
    }
}
