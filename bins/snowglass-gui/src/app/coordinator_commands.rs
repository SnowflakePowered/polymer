use super::*;

impl ShellCoordinator {
    pub(super) fn notify_snowglass(&self, notification: SnowglassNotification) {
        let Some(notifications) = &self.notifications else {
            return;
        };
        if let Err(error) = notifications.send(notification) {
            tracing::error!("snowglass: publishing notification failed: {error}");
        }
    }

    pub(super) fn apply_emulator_command(&mut self, command: EmulatorCommand) {
        match command {
            EmulatorCommand::Launch {
                token,
                options,
                prepared_filesystem,
                controller_session,
                result,
            } => {
                let options = serde_json::from_value::<AttachedLaunchPlan>(options)
                    .map_err(|error| format!("invalid emulator launch options: {error}"))
                    .and_then(|options| {
                        options.validate()?;
                        Ok(options)
                    });
                let launched = options.and_then(|options| {
                    self.launch_game(token, options, prepared_filesystem, controller_session)
                });
                let _ = result.send(launched);
            }
            EmulatorCommand::Close(token) => self.close_game(token),
            EmulatorCommand::SetVolume { token, volume } => self.set_game_volume(token, volume),
            EmulatorCommand::SetMuted { token, muted } => self.set_game_muted(token, muted),
            EmulatorCommand::Message { token, name, args } => self.game_message(token, &name, args),
            EmulatorCommand::Input { token, active } if self.game_host.is_active(token) => {
                self.set_input_priority(if active {
                    InputPriority::Game
                } else {
                    InputPriority::Ui
                });
            }
            EmulatorCommand::Input { .. } => {}
        }
    }

    pub(super) async fn apply_input_command(
        &mut self,
        command: InputCommand,
    ) -> Option<PendingInputChange> {
        match command {
            InputCommand::Configure {
                request_id,
                transaction,
            } => match serde_json::from_value::<InputTransaction>(transaction) {
                Ok(transaction) => {
                    return self.configure_input(u64::from(request_id), transaction);
                }
                Err(error) => self.send_input_configuration_result(
                    u64::from(request_id),
                    Err(format!("invalid input transaction: {error}")),
                ),
            },
            InputCommand::Api { command, result } => {
                let response = self
                    .input
                    .controller_command(command, self.game_host.controller_topology_locked())
                    .await;
                let response = response.map(|(value, configuration_changed)| {
                    if configuration_changed {
                        self.publish_input_configuration();
                    }
                    value
                });
                let _ = result.send(response);
            }
        }
        None
    }

    pub(super) fn apply_webrtc_command(&mut self, command: WebRtcCommand) {
        match command {
            WebRtcCommand::Start {
                token,
                bind,
                targets,
                result,
            } => {
                let _ = result.send(self.game_host.start_webrtc(token, bind, &targets));
            }
            WebRtcCommand::Stop { token, result } => {
                let stopped = self.game_host.stop_webrtc(token);
                let _ = result.send(if stopped {
                    Ok(())
                } else {
                    Err("the WebRTC publication is not active".to_owned())
                });
            }
        }
    }

    pub(super) fn configure_input(
        &mut self,
        request_id: u64,
        transaction: InputTransaction,
    ) -> Option<PendingInputChange> {
        let prepared = match self.input.prepare_configuration_transaction(
            transaction,
            self.game_host.controller_topology_locked(),
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.send_input_configuration_result(request_id, Err(error));
                return None;
            }
        };
        let topology = match self.input.controller_topology_for(prepared.candidate()) {
            Ok(topology) => topology,
            Err(error) => {
                self.send_input_configuration_result(request_id, Err(error));
                return None;
            }
        };
        match self.game_host.controller_change(topology) {
            Ok(Some(response)) => Some(PendingInputChange {
                request_id,
                prepared,
                response,
            }),
            Ok(None) => {
                self.commit_input_configuration(request_id, prepared);
                None
            }
            Err(error) => {
                self.send_input_configuration_result(request_id, Err(error));
                None
            }
        }
    }

    pub(super) fn finish_input_configuration(
        &mut self,
        pending: PendingInputChange,
        callback: Result<(), String>,
    ) {
        match callback {
            Ok(()) => self.commit_input_configuration(pending.request_id, pending.prepared),
            Err(error) => {
                if let Some(token) = self.game_host.active_token() {
                    self.emit_instance_event(
                        token,
                        "controllerchangeerror",
                        &instance_events::error(&error),
                    );
                }
                self.send_input_configuration_result(pending.request_id, Err(error));
            }
        }
    }

    fn commit_input_configuration(&mut self, request_id: u64, prepared: PreparedInputTransaction) {
        match self.input.commit_configuration_transaction(prepared) {
            Ok(configuration) => {
                self.publish_input_configuration();
                self.send_input_configuration_result(request_id, Ok(configuration));
            }
            Err(error) => self.send_input_configuration_result(request_id, Err(error)),
        }
    }

    pub(super) fn publish_input_configuration(&mut self) {
        let publication = self.input.configuration_publication();
        self.notify_snowglass(SnowglassNotification::InputConfigurationChanged {
            configuration: publication.configuration,
            layouts: publication.layouts,
        });
    }

    pub(super) fn send_input_configuration_result(
        &mut self,
        request_id: u64,
        result: Result<snowglass_controllers::InputConfiguration, String>,
    ) {
        let (result, _) = InputRouter::configuration_result(request_id, result);
        self.notify_snowglass(SnowglassNotification::InputConfigurationSettled {
            request_id,
            result,
        });
    }
}
