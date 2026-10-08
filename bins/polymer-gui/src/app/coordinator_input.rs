use super::*;

impl ShellCoordinator {
    pub(super) fn open_inspector(&mut self) {
        if let Some(inspector) = &self.inspector {
            inspector.show();
            return;
        }
        let Some(address) = self.shell.inspector_address() else {
            tracing::warn!("polymer: inspector requested before the shell runtime was ready");
            return;
        };
        if let Err(error) = self
            .proxy
            .send_event(UserEvent::InspectorWindowRequested { address })
        {
            tracing::error!(%error, "polymer: requesting an inspector window failed");
        }
    }

    pub(super) fn accept_inspector_window(
        &mut self,
        window: Window,
        address: std::net::SocketAddr,
    ) {
        let Some(shell) = self.shell.channel() else {
            return;
        };
        let result = InspectorWindow::open(window, address, shell);
        match result {
            Ok(inspector) => self.inspector = Some(inspector),
            Err(error) => tracing::error!(%error, "polymer: opening the Deno inspector failed"),
        }
    }

    /// Sends a mouse event to the shell thread and holds it. Script may
    /// claim it; otherwise it reaches the game if it landed on a game surface.
    fn shell_mouse(
        &mut self,
        kind: &'static str,
        button: u16,
        pressed: Option<bool>,
        phase: PointerPhase,
        game_button: u32,
    ) {
        if !self.shell.is_active() {
            return;
        }
        let flag = match button {
            0 => 1,
            1 => 4,
            2 => 2,
            3 => 8,
            4 => 16,
            _ => 0,
        };
        let flag_was_set = self.input.mouse_buttons() & flag != 0;
        if let Some(pressed) = pressed {
            self.input.set_mouse_button(flag, pressed);
        }
        let sequence = self.held.hold(HeldInput::Pointer {
            phase,
            button: game_button,
            flag: if pressed.is_some() { flag } else { 0 },
            flag_was_set,
        });
        let input = self.pointer_input(kind, button);
        self.shell.input(Some(sequence), input);
    }

    fn pointer_input(&self, kind: &'static str, button: u16) -> ShellInput {
        let (x, y) = self.input.cursor();
        ShellInput::Pointer {
            kind,
            sample: PointerSample {
                x,
                y,
                at: Instant::now(),
            },
            button: button.min(u16::from(u8::MAX)) as u8,
            buttons: self.input.mouse_buttons(),
            ctrl_key: self.input.dom_control_key(),
            shift_key: self.input.dom_shift_key(),
        }
    }

    /// Sends a key to the shell thread. While the game owns the keyboard the
    /// key is held, and reaches the game unless script cancels it.
    pub(super) fn shell_keyboard(&mut self, pressed: bool, key: String, code: String, repeat: bool) {
        let game_owned = self.input.priority() == InputPriority::Game;
        if !self.shell.is_active() {
            if game_owned {
                self.route_keyboard_to_game(Some(code), pressed);
            }
            return;
        }
        let sequence = game_owned.then(|| {
            self.held.hold(HeldInput::Key {
                physical_key: Some(code.clone()),
                pressed,
            })
        });
        let input = ShellInput::Key {
            pressed,
            key,
            code,
            repeat,
            alt_key: self.input.dom_alt_key(),
            ctrl_key: self.input.dom_control_key(),
            shift_key: self.input.dom_shift_key(),
        };
        self.shell.input(sequence, input);
    }

    /// Routes held input the shell has settled, or that waited too long, to
    /// the game in arrival order.
    pub(super) fn release_held_input(&mut self, now: Instant) {
        for (input, outcome) in self.held.release(now) {
            match input {
                HeldInput::Key {
                    physical_key,
                    pressed,
                } if !outcome.claimed && self.input.priority() == InputPriority::Game => {
                    self.route_keyboard_to_game(physical_key, pressed);
                }
                HeldInput::Pointer {
                    phase,
                    button,
                    flag,
                    flag_was_set,
                } if !outcome.claimed => {
                    self.input.set_mouse_button(flag, flag_was_set);
                    if outcome.over_tracked_surface {
                        self.route_pointer(phase, button, 0, 0);
                    }
                }
                HeldInput::Wheel {
                    horizontal,
                    vertical,
                } if !outcome.claimed && outcome.over_tracked_surface => {
                    self.route_pointer(PointerPhase::Wheel, 0, horizontal, vertical);
                }
                _ => {}
            }
        }
    }

    pub(super) fn forward_shell_refresh_request(&mut self) {
        if self.game_host.take_shell_refresh_request() {
            self.shell.send(ShellCommand::Invalidate);
        }
    }

    pub(super) fn pointer_presentation(&self) -> (polymer_renderer::Presentation, CompositingPlan) {
        let size = self.compositor.borrow_mut().size();
        self.game_host
            .pointer_presentation(Extent::new(size.width, size.height))
    }

    pub(super) fn route_pointer(
        &mut self,
        phase: PointerPhase,
        button: u32,
        horizontal: i32,
        vertical: i32,
    ) {
        let (presentation, plan) = self.pointer_presentation();
        self.input.route_game_pointer(
            phase,
            button,
            horizontal,
            vertical,
            &presentation,
            &plan,
            self.game_host.pointer_sender(),
        );
    }

    pub(super) fn set_input_priority(&mut self, priority: InputPriority) {
        let priority = if priority == InputPriority::Game && !self.game_host.has_active_game() {
            InputPriority::Ui
        } else {
            priority
        };
        if self.input.priority() == priority {
            // A UI pointer interaction changes DOM focus without changing the
            // logical priority. Re-activating game input
            // must still return keyboard focus to the native shell window.
            if priority == InputPriority::Game {
                self.focus_game_input();
            }
            return;
        }
        if let Some(connection) = self.input.captured_connection() {
            self.cancel_pointer_connection(connection);
        }
        self.input.prepare_priority_change();
        self.input
            .set_game_input_enabled(priority == InputPriority::Game);
        self.input.replace_priority(priority);
        self.focus_game_input();
    }

    pub(super) fn focus_game_input(&self) {
        self.compositor.borrow_mut().focus_host();
    }

    pub(super) fn cancel_pointer_connection(&mut self, connection: u64) {
        let (presentation, plan) = self.pointer_presentation();
        self.input.cancel_pointer_connection(
            connection,
            &presentation,
            &plan,
            self.game_host.pointer_sender(),
        );
    }

    pub(super) fn handle_dropped_file(&mut self, path: std::path::PathBuf) {
        self.notify_polymer(PolymerNotification::OpenRequested(serde_json::json!({
            "kind": "scrape",
            "path": path.to_string_lossy(),
        })));
    }

    pub(super) fn resize(&mut self, width: u32, height: u32) {
        if let Err(error) = self.compositor.borrow_mut().resize_overlay() {
            tracing::error!("polymer: resizing egui composition overlay failed: {error}");
        }
        let scale_factor = self.compositor.borrow_mut().scale_factor();
        self.shell.send(ShellCommand::Resize {
            width,
            height,
            scale_factor,
        });
        self.compositor.borrow_mut().request_redraw();
    }

    pub(super) fn handle_focus_lost(&mut self) {
        if let Some(connection) = self.input.captured_connection() {
            self.cancel_pointer_connection(connection);
        }
        for (button, _) in self.input.focus_lost_buttons() {
            let input = self.pointer_input("mouseup", button);
            self.shell.input(None, input);
        }
        self.input.finish_focus_lost();
    }

    pub(super) fn route_keyboard_to_game(&mut self, key: Option<String>, pressed: bool) {
        let Some(key) = key else { return };
        self.input.route_game_key(&key, pressed);
    }

    pub(super) fn handle_cursor_moved(&mut self, x: f64, y: f64, consumed: bool) {
        self.input.set_cursor(x, y);
        if consumed {
            return;
        }
        self.shell_mouse("mousemove", 0, None, PointerPhase::Move, 0);
    }

    pub(super) fn handle_mouse_button(
        &mut self,
        button: Option<u16>,
        pressed: bool,
        consumed: bool,
    ) {
        let Some(button) = button else { return };
        if consumed {
            return;
        }
        let dom_button = match button {
            1 => 0,
            2 => 2,
            3 => 1,
            value => value.saturating_sub(1),
        };
        let (kind, phase) = if pressed {
            ("mousedown", PointerPhase::ButtonDown)
        } else {
            ("mouseup", PointerPhase::ButtonUp)
        };
        self.shell_mouse(kind, dom_button, Some(pressed), phase, u32::from(button));
    }

    pub(super) fn handle_mouse_wheel(&mut self, horizontal: i32, vertical: i32) {
        if !self.shell.is_active() {
            return;
        }
        let (x, y) = self.input.cursor();
        let sequence = self.held.hold(HeldInput::Wheel {
            horizontal,
            vertical,
        });
        let input = ShellInput::Wheel {
            x: x as f32,
            y: y as f32,
            horizontal,
            vertical,
            ctrl_key: self.input.dom_control_key(),
            shift_key: self.input.dom_shift_key(),
            alt_key: self.input.dom_alt_key(),
        };
        self.shell.input(Some(sequence), input);
    }

    pub(super) fn handle_ime(&mut self, event: ShellImeEvent) {
        let event = match event {
            ShellImeEvent::Enabled => blitz_traits::events::BlitzImeEvent::Enabled,
            ShellImeEvent::Preedit(value, range) => {
                blitz_traits::events::BlitzImeEvent::Preedit(value, range)
            }
            ShellImeEvent::Commit(value) => blitz_traits::events::BlitzImeEvent::Commit(value),
            ShellImeEvent::Disabled => blitz_traits::events::BlitzImeEvent::Disabled,
        };
        self.shell.input(None, ShellInput::Ime(event));
    }

    pub(super) fn draw_overlay(&mut self) {
        let entered = self.profile.enter();
        if self.compositor.borrow().scene_pending() {
            self.draw_pending_chrome();
        }
        if let Err(error) = self.compositor.borrow_mut().draw_overlay() {
            tracing::error!("polymer: drawing egui composition overlay failed: {error}");
        }
        self.profile.record(TurnPhase::Overlay, entered);
    }

    pub(super) fn publish_input_state(&mut self) {
        if !self.input.publication_due(Instant::now()) {
            return;
        }
        let (presentation, plan) = self.pointer_presentation();
        self.input
            .route_device_pointers(&presentation, &plan, self.game_host.pointer_sender());
        let applied_topology = self.input.applied_controller_topology().unwrap_or_default();
        let publication = self
            .input
            .build_publication(&applied_topology.mappings, self.game_host.has_active_game());
        self.compositor
            .borrow_mut()
            .set_input_diagnostics(publication.diagnostics);
        if let Some(state) = publication.api_state {
            self.notify_polymer(PolymerNotification::ControllerApiStateChanged(state));
        }
    }

    pub(super) fn publish_targets_for(&mut self, token: InstanceToken) {
        let payload = instance_events::surfaces(self.game_host.surface_targets());
        self.emit_instance_event(token, "surfacesChanged", &payload);
    }

    pub(super) fn poll_target(&mut self) {
        if !self.game_host.has_active_game() {
            return;
        }
        if self.game_host.has_exited() {
            self.game_host.detach_capture();
            self.input.detach();
            self.finish_active_game();
        } else {
            self.publish_audio_state(false);
        }
    }
}
