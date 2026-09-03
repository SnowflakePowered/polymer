use super::{ControllerCommandOutcome, ControllerManager, ControllerManagerEffect, DeviceMapping};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
enum ControllerCommand {
    #[serde(rename = "controllers.session.start")]
    StartSession,
    #[serde(rename = "controllers.session.commit")]
    CommitSession { session_id: String },
    #[serde(rename = "controllers.session.connect")]
    ConnectSession { session_id: String, id: String },
    #[serde(rename = "controllers.disconnect")]
    Disconnect { id: String },
    #[serde(rename = "controllers.forget")]
    Forget { id: String },
    #[serde(rename = "controllers.mapping.get")]
    GetMapping { id: String, layout_id: String },
    #[serde(rename = "controllers.mapping.set")]
    SetMapping {
        id: String,
        layout_id: String,
        mapping: DeviceMapping,
    },
    #[serde(rename = "controllers.mapping.reset")]
    ResetMapping { id: String, layout_id: String },
}

impl ControllerManager {
    pub async fn command(
        &mut self,
        command: serde_json::Value,
    ) -> Result<ControllerCommandOutcome, String> {
        let command = serde_json::from_value(command)
            .map_err(|error| format!("invalid controller command: {error}"))?;
        match command {
            ControllerCommand::StartSession => {
                if self.session.is_some() {
                    return Err("a controller connection session is already active".to_owned());
                }
                let id = format!(
                    "{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|error| error.to_string())?
                        .as_nanos()
                );
                self.clear_assignments()?;
                self.session = Some(super::ControllerConnectionSession {
                    id,
                    committing: false,
                    assignments: Vec::new(),
                });
                Ok(ControllerCommandOutcome {
                    response: self.snapshot(),
                    session_commit: None,
                    reapply_topology: false,
                    effects: vec![ControllerManagerEffect::SessionDiscovery(true)],
                })
            }
            ControllerCommand::CommitSession { session_id } => {
                let session = self
                    .session
                    .as_mut()
                    .filter(|session| session.id == session_id)
                    .ok_or_else(|| "unknown controller connection session".to_owned())?;
                if session.committing {
                    return Err("controller connection session is already committing".to_owned());
                }
                let assignments = self.source_assignments();
                let session = self
                    .session
                    .as_mut()
                    .expect("the controller session was validated above");
                session.committing = true;
                session.assignments.clone_from(&assignments);
                Ok(ControllerCommandOutcome {
                    response: self.snapshot(),
                    session_commit: Some(super::ControllerSessionCommit {
                        session_id,
                        assignments,
                    }),
                    reapply_topology: false,
                    effects: vec![ControllerManagerEffect::SessionDiscovery(false)],
                })
            }
            ControllerCommand::ConnectSession { session_id, id } => {
                let session = self
                    .session
                    .as_ref()
                    .filter(|session| session.id == session_id)
                    .ok_or_else(|| "unknown controller connection session".to_owned())?;
                if session.committing {
                    return Err("controller connection session is committing".to_owned());
                }
                Ok(self.outcome(vec![ControllerManagerEffect::Connect(id)], false))
            }
            ControllerCommand::Disconnect { id } => {
                if let (Some(hub), Some((_, descriptor))) = (
                    self.hub.as_ref(),
                    self.hub.as_ref().and_then(|hub| {
                        hub.devices()
                            .into_iter()
                            .find(|(_, descriptor)| descriptor.id.as_str() == id)
                    }),
                ) {
                    hub.assign(&descriptor.id, None)
                        .map_err(|error| error.to_string())?;
                }
                Ok(self.outcome(
                    vec![ControllerManagerEffect::DisconnectSources(vec![id])],
                    false,
                ))
            }
            ControllerCommand::Forget { id } => {
                Ok(self.outcome(vec![ControllerManagerEffect::Forget(id)], false))
            }
            ControllerCommand::GetMapping { id, layout_id } => Ok(ControllerCommandOutcome {
                response: self.mapping_response(&id, &layout_id)?,
                session_commit: None,
                reapply_topology: false,
                effects: Vec::new(),
            }),
            ControllerCommand::SetMapping {
                id,
                layout_id,
                mapping,
            } => {
                self.set_mapping(&id, &layout_id, mapping).await?;
                Ok(self.outcome(Vec::new(), true))
            }
            ControllerCommand::ResetMapping { id, layout_id } => {
                let (identity, layout) = self.mapping_key(&id, &layout_id)?;
                self.mapping_store
                    .as_mut()
                    .ok_or_else(|| "the input mapping store is unavailable".to_owned())?
                    .remove_mapping(&identity, &layout)
                    .await
                    .map_err(|error| error.to_string())?;
                self.mappings.remove(&identity, &layout);
                Ok(self.outcome(Vec::new(), true))
            }
        }
    }

    fn outcome(
        &self,
        effects: Vec<ControllerManagerEffect>,
        reapply_topology: bool,
    ) -> ControllerCommandOutcome {
        ControllerCommandOutcome {
            response: self.snapshot(),
            session_commit: None,
            reapply_topology,
            effects,
        }
    }
}
