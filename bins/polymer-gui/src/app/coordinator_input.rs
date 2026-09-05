use super::*;

impl ShellCoordinator {
    pub(super) fn open_inspector(&mut self) {
        if let Some(inspector) = &self.inspector {
            inspector.show();
            return;
        }
        let Some(address) = self.shell_dom.inspector_address() else {
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
        let result = InspectorWindow::open(window, address, self.proxy.clone());
        match result {
            Ok(inspector) => self.inspector = Some(inspector),
            Err(error) => tracing::error!(%error, "polymer: opening the Deno inspector failed"),
        }
    }

    /// Dispatches a regular DOM mouse event to the Deno-backed shell.
    /// Script claims the pointer from the guest by cancelling the event.
    pub(super) fn shell_mouse(
        &mut self,
        kind: &str,
        x: f32,
        y: f32,
        button: u16,
        pressed: Option<bool>,
    ) -> bool {
        let timing = std::env::var_os("POLYMER_GAME_LAYER_TIMING")
            .is_some()
            .then(std::time::Instant::now);
        let flag = match button {
            0 => 1,
            1 => 4,
            2 => 2,
            3 => 8,
            4 => 16,
            _ => 0,
        };
        let previous_buttons = pressed
            .map(|pressed| self.input.set_mouse_button(flag, pressed))
            .unwrap_or(self.input.mouse_buttons());
        let buttons = self.input.mouse_buttons();
        let Some(hit) = self.shell_dom.hit_test(x, y) else {
            self.input.restore_mouse_buttons(previous_buttons);
            return false;
        };
        let hit_elapsed = timing.map(|started| started.elapsed());
        let prevented = self.shell_dom.mouse_event(
            hit,
            DomMouseEvent {
                kind,
                x,
                y,
                button: button.min(u16::from(u8::MAX)) as u8,
                buttons,
                ctrl_key: self.input.dom_control_key(),
                shift_key: self.input.dom_shift_key(),
            },
        );
        let dispatch_elapsed = timing.map(|started| started.elapsed());
        let needs_paint = self.shell_dom.needs_paint();
        if !prevented {
            self.input.restore_mouse_buttons(previous_buttons);
        }
        if needs_paint {
            // Winit coalesces redraw requests, which gives capture wakeups a
            // chance to drain between pointer samples. Resolving and encoding
            // the complete Blitz scene here used to hold the producer's export
            // buffers for the duration of every mouse move and back-pressure
            // the guest itself. The redraw path still applies DOM motion once
            // per display frame, while guest capture/presentation remains
            // independent of chrome work.
            self.window.request_redraw();
        }
        if kind == "mousemove"
            && let (Some(started), Some(hit_elapsed), Some(dispatch_elapsed)) =
                (timing, hit_elapsed, dispatch_elapsed)
        {
            tracing::error!(
                "polymer: mousemove hit {:.2}ms dispatch {:.2}ms total {:.2}ms",
                hit_elapsed.as_secs_f64() * 1000.0,
                (dispatch_elapsed - hit_elapsed).as_secs_f64() * 1000.0,
                started.elapsed().as_secs_f64() * 1000.0,
            );
        }
        prevented
    }

    pub(super) fn shell_keyboard(
        &mut self,
        kind: &str,
        key: String,
        code: String,
        repeat: bool,
    ) -> bool {
        let prevented = self.shell_dom.keyboard_event(DomKeyboardEvent {
            kind,
            key,
            code,
            repeat,
            alt_key: self.input.dom_alt_key(),
            ctrl_key: self.input.dom_control_key(),
            shift_key: self.input.dom_shift_key(),
        });
        if self.shell_dom.needs_paint() {
            self.refresh_shell();
        }
        prevented
    }

    pub(super) fn refresh_pending_shell(&mut self) {
        if self.game_host.take_shell_refresh_request() {
            self.refresh_shell();
        }
    }

    pub(super) fn pointer_presentation(&self) -> (polymer_renderer::Presentation, CompositingPlan) {
        let size = self.window.size();
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
        self.window.focus_host();
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
        if let Err(error) = self.window.resize_overlay() {
            tracing::error!("polymer: resizing egui composition overlay failed: {error}");
        }
        if self
            .shell_dom
            .resize(width, height, self.window.scale_factor())
        {
            self.refresh_shell();
        }
        self.window.request_redraw();
    }

    pub(super) fn handle_focus_lost(&mut self) {
        if let Some(connection) = self.input.captured_connection() {
            self.cancel_pointer_connection(connection);
        }
        let cursor = self.input.cursor();
        let (css_x, css_y) = self
            .shell_dom
            .physical_to_css(cursor.x as f32, cursor.y as f32);
        for (button, _) in self.input.focus_lost_buttons() {
            self.shell_mouse("mouseup", css_x, css_y, button, Some(false));
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
        let (css_x, css_y) = self.shell_dom.physical_to_css(x as f32, y as f32);
        let Some(hit) = self.shell_dom.hit_test(css_x, css_y) else {
            return;
        };
        let prevented = self.shell_mouse("mousemove", css_x, css_y, 0, None);
        if prevented || !hit.over_tracked_surface {
            return;
        }
        self.route_pointer(PointerPhase::Move, 0, 0, 0);
    }

    pub(super) fn handle_mouse_button(
        &mut self,
        button: Option<u16>,
        pressed: bool,
        consumed: bool,
    ) {
        let Some(button) = button else { return };
        let dom_button = match button {
            1 => 0,
            2 => 2,
            3 => 1,
            value => value.saturating_sub(1),
        };
        let cursor = self.input.cursor();
        if consumed {
            return;
        }
        let (css_x, css_y) = self
            .shell_dom
            .physical_to_css(cursor.x as f32, cursor.y as f32);
        let Some(hit) = self.shell_dom.hit_test(css_x, css_y) else {
            return;
        };
        let kind = if pressed { "mousedown" } else { "mouseup" };
        let prevented = self.shell_mouse(kind, css_x, css_y, dom_button, Some(pressed));
        if prevented || !hit.over_tracked_surface {
            return;
        }
        self.route_pointer(
            if pressed {
                PointerPhase::ButtonDown
            } else {
                PointerPhase::ButtonUp
            },
            u32::from(button),
            0,
            0,
        );
    }

    pub(super) fn handle_mouse_wheel(&mut self, horizontal: i32, vertical: i32) {
        let cursor = self.input.cursor();
        let (css_x, css_y) = self
            .shell_dom
            .physical_to_css(cursor.x as f32, cursor.y as f32);
        let Some(hit) = self.shell_dom.hit_test(css_x, css_y) else {
            return;
        };
        let scale_factor = f64::from(self.window.scale_factor());
        let claimed = self.shell_dom.wheel_event(
            hit,
            DomWheelEvent {
                x: css_x,
                y: css_y,
                horizontal: f64::from(horizontal) / scale_factor,
                vertical: f64::from(vertical) / scale_factor,
                ctrl_key: self.input.dom_control_key(),
                shift_key: self.input.dom_shift_key(),
                alt_key: self.input.dom_alt_key(),
            },
        );
        self.refresh_dirty_shell();
        if !claimed && hit.over_tracked_surface {
            self.route_pointer(PointerPhase::Wheel, 0, horizontal, vertical);
        }
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
        self.shell_dom.ime_event(event);
        self.refresh_dirty_shell();
    }

    pub(super) fn draw_overlay(&mut self) {
        let entered = self.profile.enter();
        if self.shell_compositor.is_pending() {
            self.draw_pending_chrome();
        }
        if let Err(error) = self.window.draw_overlay() {
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
        self.window.set_input_diagnostics(publication.diagnostics);
        if let Some(state) = publication.api_state {
            self.notify_polymer(PolymerNotification::ControllerApiStateChanged(state));
        }
        self.window.request_overlay_repaint();
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
