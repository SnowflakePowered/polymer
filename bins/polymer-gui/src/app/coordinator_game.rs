use super::*;

impl ShellCoordinator {
    pub(super) fn launch_game(
        &mut self,
        token: InstanceToken,
        options: AttachedLaunchPlan,
        prepared_filesystem: Option<polymer_vfs::PreparedFilesystemLaunch>,
        controller_session: Option<polymer_pluginhost::controller::ControllerSession>,
    ) -> Result<(), String> {
        let host = polymer_vfs_winfsp::WinFspFilesystemHost::default();
        let mounted_filesystem = match prepared_filesystem
            .ok_or_else(|| "guest launch requires a prepared virtual filesystem".to_owned())
            .and_then(|prepared| {
                polymer_runtime::mount_prepared_filesystem(&host, prepared, &options.command)
            }) {
            Ok(filesystem) => filesystem,
            Err(error) => {
                self.emit_instance_event(token, "error", &instance_events::error(&error));
                self.emit_instance_event(token, "closed", &serde_json::Value::Null);
                return Err(error);
            }
        };
        if self.game_host.has_active_game() {
            let error = "another emulator instance is active".to_owned();
            self.emit_instance_event(token, "error", &instance_events::error(&error));
            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            return Err(error);
        }
        self.shell_dom.flush_script_mutations();
        let invalid_shell_attachment = self.shell_dom.invalid_attachment(&options);
        if let Some(index) = invalid_shell_attachment {
            let error = format!("surface {index} does not attach to its shell tracked-surface");
            self.emit_instance_event(token, "error", &instance_events::error(&error));
            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            return Err(error);
        }
        self.emit_instance_event(token, "start", &serde_json::Value::Null);
        if let Err(error) = self
            .input
            .configure_launch(&options.machine, &options.controller_topology)
        {
            self.emit_instance_event(token, "error", &instance_events::error(&error));
            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            return Err(error);
        }
        if let Err(error) = self
            .input
            .configure_input_hooks(&options.hooks.input, &self.session)
        {
            self.emit_instance_event(token, "error", &instance_events::error(&error));
            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            return Err(error);
        }
        self.publish_input_configuration();
        let wake_proxy = self.proxy.clone();
        match self.game_host.launch(
            token,
            options,
            mounted_filesystem,
            controller_session,
            self.session.clone(),
            move || {
                wake_proxy
                    .send_event(UserEvent::Window(WindowCommand::WakeCapture))
                    .is_ok()
            },
        ) {
            Ok(()) => {
                self.input.attach(
                    self.game_host
                        .input_endpoint()
                        .expect("the launched game host exposes input capabilities"),
                );
                self.set_input_priority(InputPriority::Game);
                self.emit_instance_event(token, "ready", &serde_json::Value::Null);
                self.publish_audio_state(true);
                Ok(())
            }
            Err(error) => {
                self.emit_instance_event(token, "error", &instance_events::error(&error));
                self.emit_instance_event(token, "closed", &serde_json::Value::Null);
                Err(error)
            }
        }
    }

    pub(super) fn close_game(&mut self, token: InstanceToken) {
        if !self.game_host.request_close(token) {
            return;
        }
        self.input.detach();
        self.finish_active_game();
    }

    pub(super) fn finish_active_game(&mut self) {
        self.quitting = false;
        self.input.clear_input_hooks();
        self.input.clear_guest_input_state();
        let Some(finished) = self.game_host.finish_active_game() else {
            return;
        };
        for connection in finished.connections {
            self.cancel_pointer_connection(connection);
            self.window.remove_target(connection);
        }
        let _ = self.window.commit();
        self.set_input_priority(InputPriority::Ui);
        self.publish_targets_for(finished.token);
        if let Some(error) = finished.finalization_error {
            self.emit_instance_event(
                finished.token,
                "error",
                &instance_events::error(format!("filesystem finalization failed: {error}")),
            );
        }
        self.emit_instance_event(finished.token, "closed", &serde_json::Value::Null);
    }

    pub(super) fn publish_audio_state(&mut self, force: bool) {
        let Some((token, audio)) = self.game_host.audio_change(force) else {
            return;
        };
        self.emit_instance_event(token, "audioChanged", &instance_events::audio(audio));
    }

    pub(super) fn emit_instance_event(
        &mut self,
        token: InstanceToken,
        event_name: &str,
        payload: &serde_json::Value,
    ) {
        let event = instance_events::envelope(token, event_name, payload);
        self.notify_polymer(PolymerNotification::EmulatorInstance(event));
    }
}
