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
    ) -> Option<PendingEmulatorCallback> {
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
            EmulatorCommand::Close(token) => {
                match self.game_host.begin_quit(token, Duration::from_secs(30)) {
                    Ok(Some((cache, screenshot, response))) => {
                        return Some(PendingEmulatorCallback::Quit(PendingQuit {
                            token,
                            cache,
                            screenshot,
                            response,
                        }));
                    }
                    Ok(None) => self.close_game(token),
                    Err(error) => {
                        self.emit_instance_event(
                            token,
                            "quickresumeerror",
                            &instance_events::error(&error),
                        );
                        self.close_game(token);
                    }
                }
            }
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
                        return Some(PendingEmulatorCallback::PauseResume(PendingPauseResume {
                            token,
                            state,
                            started: Instant::now(),
                            result,
                            response,
                        }));
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
            EmulatorCommand::SaveState {
                token,
                timeout,
                result,
            } if self.game_host.is_active(token) => match self.game_host.save_state(timeout) {
                Ok((slot, response)) => {
                    return Some(PendingEmulatorCallback::State(PendingStateAction {
                        slot,
                        save: true,
                        result,
                        response,
                    }));
                }
                Err(error) => {
                    let _ = result.send(Err(error));
                }
            },
            EmulatorCommand::ResumeState {
                token,
                slot,
                timeout,
                result,
            } if self.game_host.is_active(token) => {
                match self.game_host.resume_state(slot, timeout) {
                    Ok(response) => {
                        return Some(PendingEmulatorCallback::State(PendingStateAction {
                            slot,
                            save: false,
                            result,
                            response,
                        }));
                    }
                    Err(error) => {
                        let _ = result.send(Err(error));
                    }
                }
            }
            EmulatorCommand::SaveState { result, .. }
            | EmulatorCommand::ResumeState { result, .. } => {
                let _ = result.send(Err("emulator is no longer active".to_owned()));
            }
            EmulatorCommand::StateScreenshot {
                token,
                record_id,
                result,
            } if self.game_host.is_active(token) => {
                let _ = result.send(self.game_host.save_state_screenshot(&record_id));
            }
            EmulatorCommand::StateScreenshot { result, .. } => {
                let _ = result.send(Err("emulator is no longer active".to_owned()));
            }
        }
        None
    }

    pub(super) fn finish_state_action(
        &mut self,
        pending: PendingStateAction,
        callback: Result<(), String>,
    ) {
        let metadata = callback.and_then(|()| {
            if pending.save {
                self.game_host.commit_save_state(pending.slot)
            } else {
                self.game_host
                    .save_state_metadata(pending.slot)
                    .ok_or_else(|| format!("save-state slot {} no longer exists", pending.slot))
            }
        });
        let value = metadata
            .and_then(|metadata| serde_json::to_value(metadata).map_err(|error| error.to_string()));
        let _ = pending.result.send(value);
    }

    pub(super) fn finish_quit(
        &mut self,
        pending: PendingQuit,
        callback: Result<Option<Vec<u8>>, String>,
    ) {
        let key = snowglass_library::QuickResumeKey::from_storage_key(&pending.cache.key);
        let store = snowglass_library::QuickResumeStore::discover();
        let persisted = match (store, key, callback) {
            (Ok(store), Some(key), Ok(Some(state))) => store
                .commit(
                    &key,
                    &pending.cache.revision,
                    &state,
                    pending.screenshot.as_deref(),
                )
                .map_err(|error| error.to_string()),
            (Ok(store), Some(key), Ok(None)) => {
                store.remove(&key).map_err(|error| error.to_string())
            }
            (Ok(store), Some(key), Err(error)) => {
                let _ = store.remove(&key);
                Err(error)
            }
            (Err(error), _, _) => Err(error.to_string()),
            (_, None, _) => Err("Quick Resume cache key is invalid".to_owned()),
        };
        if let Err(error) = persisted {
            self.emit_instance_event(
                pending.token,
                "quickresumeerror",
                &instance_events::error(&error),
            );
        }
        self.close_game(pending.token);
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
                let response = self.input.controller_command(command).await;
                let response = match response {
                    Ok(response) => response,
                    Err(error) => {
                        let _ = result.send(Err(error));
                        return None;
                    }
                };
                let Some(commit) = response.session_commit else {
                    if response.reapply_topology {
                        let transaction = InputTransaction {
                            expected_revision: self.input.configuration_snapshot().revision,
                            operations: Vec::new(),
                        };
                        return match self.input.prepare_controller_transaction(transaction) {
                            Ok(transaction) => self.dispatch_controller_transaction(
                                transaction,
                                PendingInputChangeCompletion::ControllerApi {
                                    value: response.response,
                                    result,
                                },
                            ),
                            Err(error) => {
                                let _ = result.send(Err(error));
                                None
                            }
                        };
                    }
                    let _ = result.send(Ok(response.response));
                    return None;
                };
                let transaction = match self.input.prepare_controller_session_transaction(&commit) {
                    Ok(transaction) => transaction,
                    Err(error) => {
                        let error =
                            self.settle_controller_session_failure(&commit.session_id, error);
                        let _ = result.send(Err(error));
                        return None;
                    }
                };
                return self.dispatch_controller_transaction(
                    transaction,
                    PendingInputChangeCompletion::ConnectionSession {
                        session_id: commit.session_id,
                        result,
                    },
                );
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
        let transaction = match self.input.prepare_controller_transaction(transaction) {
            Ok(transaction) => transaction,
            Err(error) => {
                self.send_input_configuration_result(request_id, Err(error));
                return None;
            }
        };
        self.dispatch_controller_transaction(
            transaction,
            PendingInputChangeCompletion::Configuration { request_id },
        )
    }

    pub(super) fn finish_input_change(
        &mut self,
        pending: PendingInputChange,
        callback: Result<snowglass_controllers::ControllerTopology, String>,
    ) {
        let PendingInputChange {
            transaction,
            completion,
            ..
        } = pending;
        if let Ok(applied) = &callback {
            tracing::info!(
                applied = %applied.assignment_diagnostics(),
                "snowglass: host received controller plugin result"
            );
        }
        let result = self
            .input
            .finish_controller_transaction(transaction, callback);
        self.complete_controller_transaction(completion, result);
    }

    fn dispatch_controller_transaction(
        &mut self,
        transaction: snowglass_controllers::ControllerTopologyTransaction,
        completion: PendingInputChangeCompletion,
    ) -> Option<PendingInputChange> {
        if !transaction.topology_changed() {
            let applied = transaction.requested_topology().clone();
            let result = self
                .input
                .stage_controller_transaction(&transaction)
                .and_then(|()| {
                    self.input
                        .finish_controller_transaction(transaction, Ok(applied))
                });
            self.complete_controller_transaction(completion, result);
            return None;
        }

        if let Err(error) = self.input.stage_controller_transaction(&transaction) {
            self.complete_controller_transaction(completion, Err(error));
            return None;
        }
        let requested = transaction.requested_topology().clone();
        match self.game_host.controller_change(transaction.change()) {
            Ok(Some(response)) => Some(PendingInputChange {
                transaction,
                completion,
                response,
            }),
            Ok(None) => {
                let result = self
                    .input
                    .finish_controller_transaction(transaction, Ok(requested));
                self.complete_controller_transaction(completion, result);
                None
            }
            Err(error) => {
                let result = self
                    .input
                    .finish_controller_transaction(transaction, Err(error));
                self.complete_controller_transaction(completion, result);
                None
            }
        }
    }

    fn complete_controller_transaction(
        &mut self,
        completion: PendingInputChangeCompletion,
        result: Result<snowglass_config::input::InputConfiguration, String>,
    ) {
        if let Err(error) = &result
            && let Some(token) = self.game_host.active_token()
        {
            self.emit_instance_event(
                token,
                "controllerchangeerror",
                &instance_events::error(error),
            );
        }
        match completion {
            PendingInputChangeCompletion::Configuration { request_id } => {
                let configuration = result.inspect(|_| {
                    self.publish_input_configuration();
                });
                self.send_input_configuration_result(request_id, configuration);
            }
            PendingInputChangeCompletion::ConnectionSession {
                session_id,
                result: response,
            } => {
                let settled = match result {
                    Ok(_configuration) => self
                        .input
                        .settle_controller_session(&session_id, true)
                        .inspect(|_| {
                            self.publish_input_configuration();
                        }),
                    Err(error) => Err(self.settle_controller_session_failure(&session_id, error)),
                };
                let _ = response.send(settled);
            }
            PendingInputChangeCompletion::ControllerApi {
                value,
                result: response,
            } => {
                let settled = result.map(|_| {
                    self.publish_input_configuration();
                    value
                });
                let _ = response.send(settled);
            }
        }
    }

    fn settle_controller_session_failure(&mut self, session_id: &str, error: String) -> String {
        match self.input.settle_controller_session(session_id, false) {
            Ok(_) => error,
            Err(settle) => format!("{error}; restoring controller session failed: {settle}"),
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
