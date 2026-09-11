use super::*;

impl ShellCoordinator {
    pub(super) fn notify_polymer(&self, notification: PolymerNotification) {
        let Some(notifications) = &self.notifications else {
            return;
        };
        if let Err(error) = notifications.send(notification) {
            tracing::error!("polymer: publishing notification failed: {error}");
        }
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
        pending.result.send(value);
    }

    pub(super) fn finish_quit(
        &mut self,
        pending: PendingQuit,
        callback: Result<Option<Vec<u8>>, String>,
    ) {
        let key = polymer_library::QuickResumeKey::from_storage_key(&pending.cache.key);
        let store = polymer_library::QuickResumeStore::discover();
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
            "polymer: pause/resume plugin callback completed"
        );
        if let Err(error) = &callback {
            self.emit_instance_event(
                pending.token,
                "pauseresumeerror",
                &instance_events::error(error),
            );
        }
        pending.result.send(callback);
    }

    pub(super) fn finish_input_change(
        &mut self,
        pending: PendingInputChange,
        callback: Result<polymer_controllers::ControllerTopology, String>,
    ) {
        let PendingInputChange {
            transaction,
            completion,
            ..
        } = pending;
        if let Ok(applied) = &callback {
            tracing::info!(
                applied = %applied.assignment_diagnostics(),
                "polymer: host received controller plugin result"
            );
        }
        let result = self
            .input
            .finish_controller_transaction(transaction, callback);
        self.complete_controller_transaction(completion, result);
    }

    fn dispatch_controller_transaction(
        &mut self,
        transaction: polymer_runtime::ControllerTopologyTransaction,
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
        result: Result<polymer_config::input::InputConfiguration, String>,
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
            PendingInputChangeCompletion::Configuration { request_id, .. } => {
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
                response.send(settled);
            }
            PendingInputChangeCompletion::ControllerApi {
                value,
                result: response,
            } => {
                let settled = result.map(|_| {
                    self.publish_input_configuration();
                    value
                });
                response.send(settled);
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
        self.notify_polymer(PolymerNotification::InputConfigurationChanged {
            configuration: publication.configuration,
            layouts: publication.layouts,
        });
    }

    pub(super) fn send_input_configuration_result(
        &mut self,
        request_id: u64,
        result: Result<polymer_config::input::InputConfiguration, String>,
    ) {
        let (result, _) = InputRouter::configuration_result(request_id, result);
        self.notify_polymer(PolymerNotification::InputConfigurationSettled { request_id, result });
    }
}

#[command_handler]
impl EmulatorCommandHandler for ShellCoordinator {
    type Output<'handler>
        = Option<PendingEmulatorCallback>
    where
        Self: 'handler;

    fn launch(
        &mut self,
        token: InstanceToken,
        options: serde_json::Value,
        prepared_filesystem: Option<polymer_vfs::PreparedFilesystemLaunch>,
        controller_session: Option<polymer_pluginhost::controller::ControllerSession>,
    ) -> Result<(), String> {
        let options = serde_json::from_value::<AttachedLaunchPlan>(options)
            .map_err(|error| format!("invalid emulator launch options: {error}"))
            .and_then(|options| {
                options.validate()?;
                Ok(options)
            });
        options.and_then(|options| {
            self.launch_game(token, options, prepared_filesystem, controller_session)
        })
    }

    fn close<'handler>(
        &'handler mut self,
        token: InstanceToken,
        _completion: polymer_command::Completion<Result<(), std::convert::Infallible>>,
    ) -> Self::Output<'handler> {
        match self.game_host.begin_quit(token, Duration::from_secs(30)) {
            Ok(Some((cache, screenshot, response))) => {
                Some(PendingEmulatorCallback::Quit(PendingQuit {
                    token,
                    cache,
                    screenshot,
                    response,
                }))
            }
            Ok(None) => {
                self.close_game(token);
                None
            }
            Err(error) => {
                self.emit_instance_event(
                    token,
                    "quickresumeerror",
                    &instance_events::error(&error),
                );
                self.close_game(token);
                None
            }
        }
    }

    fn set_volume<'handler>(
        &'handler mut self,
        token: InstanceToken,
        volume: f64,
        completion: polymer_command::Completion<Result<(), std::convert::Infallible>>,
    ) -> Self::Output<'handler> {
        let _completion = completion;
        if self.game_host.set_volume(token, volume) {
            self.publish_audio_state(false);
        }
        None
    }

    fn set_muted<'handler>(
        &'handler mut self,
        token: InstanceToken,
        muted: bool,
        completion: polymer_command::Completion<Result<(), std::convert::Infallible>>,
    ) -> Self::Output<'handler> {
        let _completion = completion;
        if self.game_host.set_muted(token, muted) {
            self.publish_audio_state(false);
        }
        None
    }

    fn message<'handler>(
        &'handler mut self,
        token: InstanceToken,
        name: String,
        args: serde_json::Value,
        completion: polymer_command::Completion<Result<(), std::convert::Infallible>>,
    ) -> Self::Output<'handler> {
        let _completion = completion;
        if self.game_host.is_active(token) {
            self.emit_instance_event(token, "message", &instance_events::message(&name, args));
        }
        None
    }

    fn input<'handler>(
        &'handler mut self,
        token: InstanceToken,
        active: bool,
        completion: polymer_command::Completion<Result<(), std::convert::Infallible>>,
    ) -> Self::Output<'handler> {
        let _completion = completion;
        if self.game_host.is_active(token) {
            self.set_input_priority(if active {
                InputPriority::Game
            } else {
                InputPriority::Ui
            });
        }
        None
    }

    fn pause_state<'handler>(
        &'handler mut self,
        token: InstanceToken,
        pause: bool,
        timeout: Duration,
        response: Completion<Result<(), String>>,
    ) -> Self::Output<'handler> {
        if !self.game_host.is_active(token) {
            response.send(Err("emulator is no longer active".to_owned()));
            return None;
        }
        let state = if pause {
            polymer_pluginhost::controller::PauseState::Pause
        } else {
            polymer_pluginhost::controller::PauseState::Resume
        };
        match self.game_host.pause_resume(state, timeout) {
            Ok(Some(callback)) => {
                tracing::info!(
                    ?state,
                    timeout_ms = timeout.as_millis(),
                    thread = ?std::thread::current().id(),
                    "polymer: dispatched pause/resume plugin callback"
                );
                Some(PendingEmulatorCallback::PauseResume(PendingPauseResume {
                    token,
                    state,
                    started: Instant::now(),
                    result: response,
                    response: callback,
                }))
            }
            Ok(None) => {
                response.send(Ok(()));
                None
            }
            Err(error) => {
                self.emit_instance_event(
                    token,
                    "pauseresumeerror",
                    &instance_events::error(&error),
                );
                response.send(Err(error));
                None
            }
        }
    }

    fn save_state<'handler>(
        &'handler mut self,
        token: InstanceToken,
        timeout: Duration,
        response: Completion<Result<serde_json::Value, String>>,
    ) -> Self::Output<'handler> {
        if !self.game_host.is_active(token) {
            response.send(Err("emulator is no longer active".to_owned()));
            return None;
        }
        match self.game_host.save_state(timeout) {
            Ok((slot, callback)) => Some(PendingEmulatorCallback::State(PendingStateAction {
                slot,
                save: true,
                result: response,
                response: callback,
            })),
            Err(error) => {
                response.send(Err(error));
                None
            }
        }
    }

    fn resume_state<'handler>(
        &'handler mut self,
        token: InstanceToken,
        slot: u32,
        timeout: Duration,
        response: Completion<Result<serde_json::Value, String>>,
    ) -> Self::Output<'handler> {
        if !self.game_host.is_active(token) {
            response.send(Err("emulator is no longer active".to_owned()));
            return None;
        }
        match self.game_host.resume_state(slot, timeout) {
            Ok(callback) => Some(PendingEmulatorCallback::State(PendingStateAction {
                slot,
                save: false,
                result: response,
                response: callback,
            })),
            Err(error) => {
                response.send(Err(error));
                None
            }
        }
    }

    fn state_screenshot(
        &mut self,
        token: InstanceToken,
        record_id: String,
    ) -> Result<Option<Vec<u8>>, String> {
        if self.game_host.is_active(token) {
            self.game_host.save_state_screenshot(&record_id)
        } else {
            Err("emulator is no longer active".to_owned())
        }
    }
}

#[command_handler]
impl InputCommandHandler for ShellCoordinator {
    type Output<'handler>
        =
        std::pin::Pin<Box<dyn std::future::Future<Output = Option<PendingInputChange>> + 'handler>>
    where
        Self: 'handler;

    fn configure<'handler>(
        &'handler mut self,
        request_id: u32,
        transaction: serde_json::Value,
        completion: polymer_command::Completion<Result<(), std::convert::Infallible>>,
    ) -> Self::Output<'handler> {
        Box::pin(async move {
            match serde_json::from_value::<InputTransaction>(transaction) {
                Ok(transaction) => {
                    let transaction = match self.input.prepare_controller_transaction(transaction) {
                        Ok(transaction) => transaction,
                        Err(error) => {
                            self.send_input_configuration_result(u64::from(request_id), Err(error));
                            return None;
                        }
                    };
                    self.dispatch_controller_transaction(
                        transaction,
                        PendingInputChangeCompletion::Configuration {
                            request_id: u64::from(request_id),
                            _admission: completion,
                        },
                    )
                }
                Err(error) => {
                    self.send_input_configuration_result(
                        u64::from(request_id),
                        Err(format!("invalid input transaction: {error}")),
                    );
                    None
                }
            }
        })
    }

    fn controller_api_response<'handler>(
        &'handler mut self,
        command: serde_json::Value,
        response: Completion<Result<serde_json::Value, String>>,
    ) -> Self::Output<'handler> {
        Box::pin(async move {
            let command_result = self.input.controller_command(command).await;
            let command_result = match command_result {
                Ok(command_result) => command_result,
                Err(error) => {
                    response.send(Err(error));
                    return None;
                }
            };
            let Some(commit) = command_result.session_commit else {
                if command_result.reapply_topology {
                    let transaction = InputTransaction {
                        expected_revision: self.input.configuration_snapshot().revision,
                        operations: Vec::new(),
                    };
                    return match self.input.prepare_controller_transaction(transaction) {
                        Ok(transaction) => self.dispatch_controller_transaction(
                            transaction,
                            PendingInputChangeCompletion::ControllerApi {
                                value: command_result.response,
                                result: response,
                            },
                        ),
                        Err(error) => {
                            response.send(Err(error));
                            None
                        }
                    };
                }
                response.send(Ok(command_result.response));
                return None;
            };
            let transaction = match self.input.prepare_controller_session_transaction(&commit) {
                Ok(transaction) => transaction,
                Err(error) => {
                    let error = self.settle_controller_session_failure(&commit.session_id, error);
                    response.send(Err(error));
                    return None;
                }
            };
            self.dispatch_controller_transaction(
                transaction,
                PendingInputChangeCompletion::ConnectionSession {
                    session_id: commit.session_id,
                    result: response,
                },
            )
        })
    }
}

#[command_handler]
impl WebRtcCommandHandler for ShellCoordinator {
    type Output<'handler>
        = ()
    where
        Self: 'handler;

    fn start(
        &mut self,
        token: InstanceToken,
        bind: std::net::SocketAddr,
        targets: Vec<u32>,
    ) -> Result<(), String> {
        self.game_host.start_webrtc(token, bind, &targets)
    }

    fn stop(&mut self, token: InstanceToken) -> Result<(), String> {
        let stopped = self.game_host.stop_webrtc(token);
        if stopped {
            Ok(())
        } else {
            Err("the WebRTC publication is not active".to_owned())
        }
    }
}
