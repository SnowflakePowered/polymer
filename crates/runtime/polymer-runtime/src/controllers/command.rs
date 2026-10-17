use super::{
    ControllerCommandOutcome, ControllerManager, ControllerManagerEffect, MappingUpdate,
    profile::{MappingScope, MappingWrite},
};
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
    #[serde(rename = "controllers.session.join")]
    JoinSession {
        session_id: String,
        source: polymer_config::input::SourceSelector,
        index: u8,
        group: Option<String>,
    },
    #[serde(rename = "controllers.disconnect")]
    Disconnect { id: String },
    #[serde(rename = "controllers.forget")]
    Forget { id: String },
    #[serde(rename = "controllers.mapping.get")]
    GetMapping {
        scope: MappingScope,
        layout_id: String,
    },
    #[serde(rename = "controllers.mapping.set")]
    SetMapping {
        scope: MappingScope,
        layout_id: String,
        mapping: MappingUpdate,
    },
    #[serde(rename = "controllers.mapping.reset")]
    ResetMapping {
        scope: MappingScope,
        layout_id: String,
    },
}

impl ControllerManager {
    pub fn command(
        &mut self,
        command: serde_json::Value,
    ) -> Result<polymer_deno::polymer::ApiFuture<Result<ControllerCommandOutcome, String>>, String>
    {
        let command = serde_json::from_value(command)
            .map_err(|error| format!("invalid controller command: {error}"))?;
        let outcome = match command {
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
                    join_requests: Vec::new(),
                    connected: self
                        .hub
                        .as_ref()
                        .map(|hub| {
                            hub.device_snapshots()
                                .into_iter()
                                .filter(|(_, _, _, connected)| *connected)
                                .map(|(_, descriptor, _, _)| descriptor.id)
                                .collect()
                        })
                        .unwrap_or_default(),
                    keyboard: None,
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
            ControllerCommand::JoinSession {
                session_id,
                source,
                index,
                group,
            } => {
                self.session
                    .as_ref()
                    .filter(|session| session.id == session_id && !session.committing)
                    .ok_or_else(|| {
                        "unknown or committing controller connection session".to_owned()
                    })?;
                self.join(source, index, group.as_deref())?;
                Ok(self.outcome(Vec::new(), false))
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
            ControllerCommand::GetMapping { scope, layout_id } => Ok(ControllerCommandOutcome {
                response: self.mapping_response(&scope, &layout_id)?,
                session_commit: None,
                reapply_topology: false,
                effects: Vec::new(),
            }),
            ControllerCommand::SetMapping {
                scope,
                layout_id,
                mapping,
            } => {
                let change = self.set_mapping(scope, &layout_id, mapping)?;
                return Ok(self.mapping_change(change));
            }
            ControllerCommand::ResetMapping { scope, layout_id } => {
                let change = self.reset_mapping(scope, &layout_id)?;
                return Ok(self.mapping_change(change));
            }
        };
        Ok(Box::pin(async move { outcome }))
    }

    fn mapping_change(
        &self,
        (write, response): (MappingWrite, serde_json::Value),
    ) -> polymer_deno::polymer::ApiFuture<Result<ControllerCommandOutcome, String>> {
        let outcome = ControllerCommandOutcome {
            response,
            ..self.outcome(Vec::new(), true)
        };
        Box::pin(async move {
            write.await?;
            Ok(outcome)
        })
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
