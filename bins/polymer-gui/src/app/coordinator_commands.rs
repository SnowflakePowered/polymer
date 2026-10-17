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

    pub(super) fn launch(
        &mut self,
        token: InstanceToken,
        options: serde_json::Value,
        prepared_filesystem: Option<polymer_vfs::PreparedFilesystemLaunch>,
        controller_session: Option<polymer_pluginhost::controller::ControllerSession>,
        surface_groups: Result<LaunchSurfaceGroups, u32>,
    ) -> Result<(), String> {
        let options = serde_json::from_value::<AttachedLaunchPlan>(options)
            .map_err(|error| format!("invalid emulator launch options: {error}"))
            .and_then(|options| {
                options.validate()?;
                Ok(options)
            });
        options.and_then(|options| {
            self.launch_game(
                token,
                options,
                prepared_filesystem,
                controller_session,
                surface_groups,
            )
        })
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
                    pending.cache.disc_index,
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
        self.quitting = false;
        self.close_game(pending.token);
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

    pub(super) fn begin_quit(
        &mut self,
        token: InstanceToken,
    ) -> Option<(PendingQuit, PendingReply<Quit>)> {
        if self.quitting || !self.game_host.is_active(token) {
            return None;
        }
        match self.game_host.begin_quit(token, Duration::from_secs(30)) {
            Ok(Some((cache, screenshot, response))) => {
                self.quitting = true;
                Some((
                    PendingQuit {
                        token,
                        cache,
                        screenshot,
                    },
                    response,
                ))
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

    pub(super) fn configure(
        &mut self,
        request_id: u32,
        transaction: serde_json::Value,
    ) -> Result<ControllerChange, String> {
        let transaction = serde_json::from_value::<InputTransaction>(transaction)
            .map_err(|error| format!("invalid input transaction: {error}"))
            .and_then(|transaction| self.input.controllers.transaction(transaction));
        match transaction {
            Ok(transaction) => self.dispatch_controller_transaction(
                transaction,
                InputChangeCompletion::Configuration {
                    request_id: u64::from(request_id),
                },
            ),
            Err(error) => {
                self.send_input_configuration_result(u64::from(request_id), Err(error.clone()));
                Err(error)
            }
        }
    }

    pub(super) fn controller_api_response(
        &mut self,
        outcome: polymer_runtime::ControllerCommandOutcome,
    ) -> Result<ControllerChange, String> {
        let Some(commit) = outcome.session_commit else {
            if !outcome.reapply_topology {
                return Ok(ControllerChange::Ready(outcome.response));
            }
            let transaction = InputTransaction {
                expected_revision: self.input.configuration_snapshot().revision,
                operations: Vec::new(),
            };
            let transaction = self.input.controllers.transaction(transaction)?;
            return self.dispatch_controller_transaction(
                transaction,
                InputChangeCompletion::ControllerApi {
                    value: outcome.response,
                },
            );
        };
        let transaction = self
            .input
            .controllers
            .connection_transaction(&commit)
            .map_err(|error| self.settle_controller_session_failure(&commit.session_id, error))?;
        self.dispatch_controller_transaction(
            transaction,
            InputChangeCompletion::ConnectionSession {
                session_id: commit.session_id,
            },
        )
    }

    pub(super) fn adopt_controller_assignments(&mut self) -> Result<ControllerChange, String> {
        let transaction = self.input.controllers.adoption_transaction()?;
        self.dispatch_controller_transaction(
            transaction,
            InputChangeCompletion::ControllerApi {
                value: serde_json::Value::Null,
            },
        )
    }

    fn dispatch_controller_transaction(
        &mut self,
        mut transaction: polymer_runtime::ControllerTopologyTransaction,
        completion: InputChangeCompletion,
    ) -> Result<ControllerChange, String> {
        if transaction.adopt_rebinding() {
            let applied = transaction.requested_topology().clone();
            let result = self
                .input
                .stage_controller_transaction(&transaction)
                .and_then(|()| {
                    self.input
                        .finish_controller_transaction(transaction, Ok(applied))
                });
            return self
                .complete_controller_transaction(completion, result)
                .map(ControllerChange::Ready);
        }
        if let Err(error) = self.input.stage_controller_transaction(&transaction) {
            return self
                .complete_controller_transaction(completion, Err(error))
                .map(ControllerChange::Ready);
        }
        let requested = transaction.requested_topology().clone();
        match self.game_host.controller_change(transaction.change()) {
            Ok(Some(response)) => Ok(ControllerChange::Pending {
                transaction: Box::new(transaction),
                completion,
                response,
            }),
            response => {
                let result = self
                    .input
                    .finish_controller_transaction(transaction, response.map(|_| requested));
                self.complete_controller_transaction(completion, result)
                    .map(ControllerChange::Ready)
            }
        }
    }

    pub(super) fn complete_controller_transaction(
        &mut self,
        completion: InputChangeCompletion,
        result: Result<polymer_config::input::InputConfiguration, String>,
    ) -> Result<serde_json::Value, String> {
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
            InputChangeCompletion::Configuration { request_id } => {
                let configuration = result.inspect(|_| self.publish_input_configuration());
                let result = configuration
                    .as_ref()
                    .map(|_| serde_json::Value::Null)
                    .map_err(Clone::clone);
                self.send_input_configuration_result(request_id, configuration);
                result
            }
            InputChangeCompletion::ConnectionSession { session_id } => match result {
                Ok(_) => self
                    .input
                    .settle_controller_session(&session_id, true)
                    .inspect(|_| self.publish_input_configuration()),
                Err(error) => Err(self.settle_controller_session_failure(&session_id, error)),
            },
            InputChangeCompletion::ControllerApi { value } => result.map(|_| {
                self.publish_input_configuration();
                value
            }),
        }
    }

    fn settle_controller_session_failure(&mut self, session_id: &str, error: String) -> String {
        match self.input.settle_controller_session(session_id, false) {
            Ok(_) => error,
            Err(settle) => format!("{error}; restoring controller session failed: {settle}"),
        }
    }
}
