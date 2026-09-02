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

    pub(super) fn apply_emulator_command(
        &mut self,
        command: EmulatorCommand,
    ) -> Option<PendingPauseResume> {
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
            EmulatorCommand::PauseState {
                token,
                pause,
                timeout,
                result,
            } if self.game_host.is_active(token) => {
                let state = if pause {
                    snowglass_pluginhost::PauseState::Pause
                } else {
                    snowglass_pluginhost::PauseState::Resume
                };
                match self.game_host.pause_resume(state, timeout) {
                    Ok(Some(response)) => {
                        tracing::info!(
                            ?state,
                            timeout_ms = timeout.as_millis(),
                            thread = ?std::thread::current().id(),
                            "snowglass: dispatched pause/resume plugin callback"
                        );
                        return Some(PendingPauseResume {
                            token,
                            state,
                            started: Instant::now(),
                            result,
                            response,
                        });
                    }
                    Ok(None) => {
                        let _ = result.send(Ok(()));
                    }
                    Err(error) => {
                        self.emit_instance_event(
                            token,
                            "pauseresumeerror",
                            &instance_events::error(&error),
                        );
                        let _ = result.send(Err(error));
                    }
                }
            }
            EmulatorCommand::PauseState { result, .. } => {
                let _ = result.send(Err("emulator is no longer active".to_owned()));
            }
        }
        None
    }

    pub(super) fn finish_pause_resume(
        &mut self,
        pending: PendingPauseResume,
        callback: Result<(), String>,
    ) {
        tracing::info!(
            state = ?pending.state,
            elapsed_ms = pending.started.elapsed().as_millis(),
            success = callback.is_ok(),
            thread = ?std::thread::current().id(),
            "snowglass: pause/resume plugin callback completed"
        );
        if let Err(error) = &callback {
            self.emit_instance_event(
                pending.token,
                "pauseresumeerror",
                &instance_events::error(error),
            );
        }
        let _ = pending.result.send(callback);
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
            InputCommand::ControllerApi { command, result } => {
                let finishes_session = command["type"] == "controllers.session.finish";
                let response = self
                    .input
                    .controller_command(command, self.game_host.controller_topology_locked())
                    .await;
                let (value, configuration_changed) = match response {
                    Ok(response) => response,
                    Err(error) => {
                        let _ = result.send(Err(error));
                        return None;
                    }
                };
                if configuration_changed {
                    self.publish_input_configuration();
                }
                if !finishes_session || !self.game_host.supports_controller_changes() {
                    let _ = result.send(Ok(value));
                    return None;
                }
                let topology = match self.input.controller_topology() {
                    Ok(topology) => topology,
                    Err(error) => {
                        let _ = result.send(Err(error));
                        return None;
                    }
                };
                if self.input.applied_controller_topology() == Some(&topology) {
                    let _ = result.send(Ok(value));
                    return None;
                }
                let configuration = self.input.configuration_snapshot();
                let previous_topology = match self
                    .input
                    .stage_controller_change(&configuration, topology.clone())
                {
                    Ok(previous) => previous,
                    Err(error) => {
                        let _ = result.send(Err(error));
                        return None;
                    }
                };
                match self.game_host.controller_change(topology) {
                    Ok(Some(response)) => {
                        return Some(PendingInputChange::ConnectionSession {
                            value,
                            result,
                            previous_topology,
                            response,
                        });
                    }
                    Ok(None) => {
                        let error =
                            "controller plugin session closed before topology change".to_owned();
                        let error = match self.input.rollback_controller_change(previous_topology) {
                            Ok(()) => error,
                            Err(rollback) => format!("{error}; rollback failed: {rollback}"),
                        };
                        let _ = result.send(Err(error));
                    }
                    Err(error) => {
                        let error = match self.input.rollback_controller_change(previous_topology) {
                            Ok(()) => error,
                            Err(rollback) => format!("{error}; rollback failed: {rollback}"),
                        };
                        let _ = result.send(Err(error));
                    }
                }
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
        if !self.game_host.supports_controller_changes() {
            self.commit_input_configuration(request_id, prepared);
            return None;
        }
        let previous_topology = match self
            .input
            .stage_controller_change(prepared.candidate(), topology.clone())
        {
            Ok(previous) => previous,
            Err(error) => {
                self.send_input_configuration_result(request_id, Err(error));
                return None;
            }
        };
        match self.game_host.controller_change(topology) {
            Ok(Some(response)) => Some(PendingInputChange::Configuration {
                request_id,
                prepared,
                previous_topology,
                response,
            }),
            Ok(None) => {
                let error = "controller plugin session closed before topology change".to_owned();
                let error = match self.input.rollback_controller_change(previous_topology) {
                    Ok(()) => error,
                    Err(rollback) => format!("{error}; rollback failed: {rollback}"),
                };
                self.send_input_configuration_result(request_id, Err(error));
                None
            }
            Err(error) => {
                let error = match self.input.rollback_controller_change(previous_topology) {
                    Ok(()) => error,
                    Err(rollback) => format!("{error}; rollback failed: {rollback}"),
                };
                self.send_input_configuration_result(request_id, Err(error));
                None
            }
        }
    }

    pub(super) fn finish_input_change(
        &mut self,
        pending: PendingInputChange,
        callback: Result<snowglass_controllers::ControllerTopology, String>,
    ) {
        match pending {
            PendingInputChange::Configuration {
                request_id,
                prepared,
                previous_topology,
                ..
            } => match callback {
                Ok(topology) => {
                    tracing::info!(
                        origin = "configuration",
                        applied = %topology.assignment_diagnostics(),
                        "snowglass: host received controller plugin result"
                    );
                    match self.input.commit_configuration_transaction(prepared) {
                        Ok(configuration) => {
                            let result = self
                                .input
                                .apply_controller_topology(topology.clone())
                                .map(|()| configuration);
                            if result.is_ok() {
                                self.game_host.update_controller_topology(topology);
                                self.publish_input_configuration();
                            }
                            self.send_input_configuration_result(request_id, result);
                        }
                        Err(error) => self.send_input_configuration_result(request_id, Err(error)),
                    }
                }
                Err(error) => {
                    let error = match self.input.rollback_controller_change(previous_topology) {
                        Ok(()) => error,
                        Err(rollback) => format!("{error}; rollback failed: {rollback}"),
                    };
                    if let Some(token) = self.game_host.active_token() {
                        self.emit_instance_event(
                            token,
                            "controllerchangeerror",
                            &instance_events::error(&error),
                        );
                    }
                    self.send_input_configuration_result(request_id, Err(error));
                }
            },
            PendingInputChange::ConnectionSession {
                value,
                result,
                previous_topology,
                ..
            } => match callback {
                Ok(topology) => {
                    tracing::info!(
                        origin = "connection-session",
                        applied = %topology.assignment_diagnostics(),
                        "snowglass: host received controller plugin result"
                    );
                    let applied = self.input.apply_controller_topology(topology.clone());
                    if applied.is_ok() {
                        self.game_host.update_controller_topology(topology);
                    }
                    let _ = result.send(applied.map(|()| value));
                }
                Err(error) => {
                    let error = match self.input.rollback_controller_change(previous_topology) {
                        Ok(()) => error,
                        Err(rollback) => format!("{error}; rollback failed: {rollback}"),
                    };
                    if let Some(token) = self.game_host.active_token() {
                        self.emit_instance_event(
                            token,
                            "controllerchangeerror",
                            &instance_events::error(&error),
                        );
                    }
                    let _ = result.send(Err(error));
                }
            },
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
        result: Result<snowglass_config::input::InputConfiguration, String>,
    ) {
        let (result, _) = InputRouter::configuration_result(request_id, result);
        self.notify_snowglass(SnowglassNotification::InputConfigurationSettled {
            request_id,
            result,
        });
    }
}
