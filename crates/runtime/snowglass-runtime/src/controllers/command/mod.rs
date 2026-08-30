use super::{
    ConnectionSession, ControllerCommandOutcome, ControllerManager, ControllerManagerEffect,
    DeviceMapping,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
enum ControllerCommand {
    #[serde(rename = "controllers.session.start")]
    StartSession,
    #[serde(rename = "controllers.session.finish")]
    FinishSession { session_id: String },
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
        bluetooth: &serde_json::Value,
    ) -> Result<ControllerCommandOutcome, String> {
        let command = serde_json::from_value(command)
            .map_err(|error| format!("invalid controller command: {error}"))?;
        match command {
            ControllerCommand::StartSession => self.start_session(bluetooth),
            ControllerCommand::FinishSession { session_id } => {
                self.finish_session(&session_id, bluetooth)
            }
            ControllerCommand::ConnectSession { session_id, id } => {
                self.connect_session(&session_id, id, bluetooth)
            }
            ControllerCommand::Disconnect { id } => self.disconnect(id, bluetooth),
            ControllerCommand::Forget { id } => self.forget(id, bluetooth),
            ControllerCommand::GetMapping { id, layout_id } => {
                self.mapping_outcome(&id, &layout_id, false)
            }
            ControllerCommand::SetMapping {
                id,
                layout_id,
                mapping,
            } => {
                self.set_mapping(&id, &layout_id, mapping).await?;
                self.mapping_outcome(&id, &layout_id, true)
            }
            ControllerCommand::ResetMapping { id, layout_id } => {
                self.reset_mapping(&id, &layout_id).await?;
                self.mapping_outcome(&id, &layout_id, true)
            }
        }
    }

    fn start_session(
        &mut self,
        bluetooth: &serde_json::Value,
    ) -> Result<ControllerCommandOutcome, String> {
        if self.session.is_some() {
            return Err("a controller connection session is already active".to_owned());
        }
        let bluetooth_sources = self
            .controllers
            .values()
            .filter(|controller| controller.source == snowglass_input::source::InputSourceKind::Bt)
            .flat_map(|controller| controller.route.sources.clone())
            .collect();
        self.players.clear();
        self.controllers.clear();
        self.source_controllers.clear();
        self.routes.clear();
        self.neutral.clear();
        self.session = Some(ConnectionSession {
            id: uuid::Uuid::new_v4().to_string(),
        });
        Ok(self.state_outcome(
            bluetooth,
            Some(self.source_assignments()),
            vec![
                ControllerManagerEffect::SessionDiscovery(true),
                ControllerManagerEffect::DisconnectSources(bluetooth_sources),
            ],
        ))
    }

    fn finish_session(
        &mut self,
        session_id: &str,
        bluetooth: &serde_json::Value,
    ) -> Result<ControllerCommandOutcome, String> {
        self.require_session(session_id)?;
        self.session = None;
        Ok(self.state_outcome(
            bluetooth,
            None,
            vec![ControllerManagerEffect::SessionDiscovery(false)],
        ))
    }

    fn connect_session(
        &self,
        session_id: &str,
        id: String,
        bluetooth: &serde_json::Value,
    ) -> Result<ControllerCommandOutcome, String> {
        self.require_session(session_id)?;
        Ok(self.state_outcome(bluetooth, None, vec![ControllerManagerEffect::Connect(id)]))
    }

    fn disconnect(
        &mut self,
        id: String,
        bluetooth: &serde_json::Value,
    ) -> Result<ControllerCommandOutcome, String> {
        let sources = self
            .controllers
            .get(&id)
            .ok_or_else(|| format!("unknown controller '{id}'"))?
            .route
            .sources
            .clone();
        self.clear_player(&id);
        Ok(self.state_outcome(
            bluetooth,
            Some(self.source_assignments()),
            vec![ControllerManagerEffect::DisconnectSources(sources)],
        ))
    }

    fn forget(
        &mut self,
        id: String,
        bluetooth: &serde_json::Value,
    ) -> Result<ControllerCommandOutcome, String> {
        let assignments = self.controllers.remove(&id).map(|controller| {
            self.clear_player(&id);
            for source in controller.route.sources {
                self.source_controllers.remove(&source);
            }
            self.source_assignments()
        });
        Ok(self.state_outcome(
            bluetooth,
            assignments,
            vec![ControllerManagerEffect::Forget(id)],
        ))
    }

    fn clear_player(&mut self, id: &str) {
        for assigned in self.players.values_mut() {
            if assigned.as_deref() == Some(id) {
                *assigned = None;
            }
        }
    }

    fn state_outcome(
        &self,
        bluetooth: &serde_json::Value,
        assignments: Option<Vec<snowglass_input::hub::SourceAssignment>>,
        effects: Vec<ControllerManagerEffect>,
    ) -> ControllerCommandOutcome {
        ControllerCommandOutcome {
            response: self.snapshot(bluetooth),
            assignments,
            effects,
        }
    }

    fn mapping_outcome(
        &self,
        id: &str,
        layout_id: &str,
        changed: bool,
    ) -> Result<ControllerCommandOutcome, String> {
        Ok(ControllerCommandOutcome {
            response: self.mapping_response(id, layout_id)?,
            assignments: None,
            effects: changed
                .then_some(ControllerManagerEffect::ReapplyMappings)
                .into_iter()
                .collect(),
        })
    }

    async fn reset_mapping(&mut self, id: &str, layout_id: &str) -> Result<(), String> {
        let (identity, layout) = self.mapping_key(id, layout_id)?;
        self.mapping_store
            .as_mut()
            .ok_or_else(|| "the input mapping store is unavailable".to_owned())?
            .remove_mapping(&identity, &layout)
            .await
            .map_err(|error| error.to_string())?;
        self.mappings.remove(&identity, &layout);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_commands_deserialize_the_canonical_camel_case_shape() {
        let command = serde_json::from_value::<ControllerCommand>(serde_json::json!({
            "type": "controllers.mapping.set",
            "id": "pad",
            "layoutId": "XBOX_CONTROLLER",
            "mapping": {
                "mode": "custom",
                "mapping": { "button.a": "button.b" },
            },
        }))
        .unwrap();
        let ControllerCommand::SetMapping {
            id,
            layout_id,
            mapping,
        } = command
        else {
            panic!("wrong command variant");
        };
        assert_eq!(id, "pad");
        assert_eq!(layout_id, "XBOX_CONTROLLER");
        assert_eq!(
            mapping.mode,
            snowglass_input::layout::ProjectionMode::Custom
        );
        assert_eq!(mapping.mapping["button.a"], "button.b");
    }

    #[test]
    fn session_commands_require_their_typed_fields() {
        let error = serde_json::from_value::<ControllerCommand>(serde_json::json!({
            "type": "controllers.session.connect",
            "id": "pad",
        }))
        .unwrap_err();
        assert!(error.to_string().contains("sessionId"));
    }
}
