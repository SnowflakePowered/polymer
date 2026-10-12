use super::*;

impl GameHost {
    pub fn launch(
        &mut self,
        token: crate::shell::runtime::InstanceToken,
        options: AttachedLaunchPlan,
        filesystem: polymer_vfs::MountedFilesystem,
        controller_session: Option<polymer_pluginhost::controller::ControllerSession>,
        session: String,
        wake_window: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<(), String> {
        if self.session.has_active_session() {
            return Err("another emulator instance is active".to_owned());
        }
        let artifact_root = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            .ok_or_else(|| "shell executable has no parent directory".to_owned())?;
        #[cfg(windows)]
        let automation_desktop = options.isolated.then(|| session.clone());
        let config = polymer_runtime::EmulatorLaunchConfig {
            session,
            command: options.command.clone(),
            working_directory: options.working_directory.as_ref().map(Into::into),
            env: options.env.clone(),
            arguments: options.arguments.clone(),
            artifact_root,
            hooks: options.hooks.clone(),
            consumer: self.compositor.borrow().consumer_preference(),
            isolate: options.isolated,
            tracked_surfaces: options
                .surfaces
                .iter()
                .map(|surface| polymer_runtime::TrackedSurfaceConfig {
                    id: polymer_runtime::TrackedSurfaceId(surface.index),
                    render_target_index: surface.index,
                    window_pattern: surface.selector.clone(),
                    rotation: surface.rotation,
                })
                .collect(),
            tracking_enabled: !options.surfaces.is_empty(),
            skip_surfaces: 0,
        };
        let dirty = self.dirty.clone();
        let wake_pending = self.wake_pending.clone();
        self.session.launch_with_input(
            token.0,
            config,
            filesystem,
            move || {
                dirty.store(true, std::sync::atomic::Ordering::Release);
                if !wake_pending.swap(true, std::sync::atomic::Ordering::AcqRel) && !wake_window() {
                    wake_pending.store(false, std::sync::atomic::Ordering::Release);
                }
            },
            move |_listener| Ok(()),
        )?;
        if let Some(controller_session) = &controller_session {
            let endpoint = self
                .session
                .instance()
                .expect("a successful session launch owns an emulator instance")
                .action_endpoint();
            let attached = futures::executor::block_on(controller_session.send(AttachActions(
                std::sync::Arc::new(move |hotkey| match hotkey {
                    polymer_pluginhost::actions::ParsedHotkey::Keyboard { code, modifiers } => {
                        endpoint.activate_keyboard(code, modifiers)
                    }
                    polymer_pluginhost::actions::ParsedHotkey::Binding { player, keys } => {
                        endpoint.activate_bindings(*player, keys)
                    }
                }),
            )))
            .map_err(|error| error.to_string())
            .and_then(|result| result);
            if let Err(error) = attached {
                let _ = self
                    .session
                    .finish(polymer_vfs::FinalizationReason::LaunchFailed);
                return Err(error);
            }
        }
        let automation = if let Some(controller_session) = &controller_session {
            let process_id = self
                .session
                .instance()
                .expect("a successful session launch owns an emulator instance")
                .process_id();
            #[cfg(windows)]
            let driver = polymer_platform_windows::WindowsAutomationDriver::new(
                process_id,
                automation_desktop,
            );
            #[cfg(target_os = "macos")]
            let driver = polymer_platform_macos::MacAutomationDriver::new(
                process_id,
                self.session.instance().unwrap().input(),
            );
            let attached = driver
                .and_then(polymer_automation::AutomationSession::spawn)
                .map_err(|error| format!("could not start emulator UI automation: {error}"))
                .and_then(|automation| {
                    futures::executor::block_on(
                        controller_session.send(AttachAutomation(automation.client())),
                    )
                    .map_err(|error| error.to_string())??;
                    Ok(automation)
                });
            match attached {
                Ok(automation) => Some(automation),
                Err(error) => {
                    let _ = self
                        .session
                        .finish(polymer_vfs::FinalizationReason::LaunchFailed);
                    return Err(error);
                }
            }
        } else {
            None
        };
        self.capture.attach(
            self.session
                .instance()
                .expect("a successful session launch owns an emulator instance"),
        );
        let save_states = options
            .save_states
            .as_ref()
            .map(|states| polymer_runtime::SaveStateManager::new(states.max));
        self.active = Some(ActivePresentation {
            token,
            current_disc_index: options.initial_disc_index,
            options,
            controller_session,
            automation,
            save_states,
            closing: false,
        });
        Ok(())
    }

    pub fn input_endpoint(&self) -> Option<polymer_ipc::input::host::InputIpcListener> {
        self.session.instance().map(|instance| instance.input())
    }

    pub fn request_close(&mut self, token: crate::shell::runtime::InstanceToken) -> bool {
        if !self.session.close(token.0) {
            return false;
        }
        self.capture.detach();
        true
    }

    pub fn set_volume(&self, token: crate::shell::runtime::InstanceToken, volume: f64) -> bool {
        self.session.set_volume(token.0, volume)
    }

    pub fn set_muted(&self, token: crate::shell::runtime::InstanceToken, muted: bool) -> bool {
        self.session.set_muted(token.0, muted)
    }

    pub fn audio_change(
        &mut self,
        force: bool,
    ) -> Option<(
        crate::shell::runtime::InstanceToken,
        polymer_runtime::AudioState,
    )> {
        self.session
            .audio_change(force)
            .map(|(token, audio)| (crate::shell::runtime::InstanceToken(token), audio))
    }

    pub fn has_exited(&self) -> bool {
        self.session.has_exited()
    }
}
