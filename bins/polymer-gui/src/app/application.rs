use super::actor::GuiActors;
use super::*;
use std::{cell::RefCell, rc::Rc};

pub(crate) struct ShellApplication {
    proxy: EventLoopProxy<UserEvent>,
    options: Option<ShellOptions>,
    statistics_recorder: Option<RecorderHandle>,
    shell_factory: Option<ShellSessionFactory>,
    compositor_factory: Option<CompositorFactory>,
    pending_window: Option<Window>,
    state: Rc<RefCell<Option<ShellCoordinator>>>,
    command_actors: GuiActors,
    commands: FrontendCommands,
    exit_requested: bool,
    error: Option<Box<dyn Error>>,
}

impl ShellApplication {
    pub(crate) fn new(
        proxy: EventLoopProxy<UserEvent>,
        options: ShellOptions,
        statistics_recorder: RecorderHandle,
        shell_factory: ShellSessionFactory,
        compositor_factory: CompositorFactory,
    ) -> Self {
        let state = Rc::new(RefCell::new(None));
        let command_actors = GuiActors::start_for(&state);
        let commands = command_actors.commands(proxy.clone());
        Self {
            proxy,
            options: Some(options),
            statistics_recorder: Some(statistics_recorder),
            shell_factory: Some(shell_factory),
            compositor_factory: Some(compositor_factory),
            pending_window: None,
            state,
            command_actors,
            commands,
            exit_requested: false,
            error: None,
        }
    }

    pub(crate) async fn run_async_tasks(&mut self) {
        let timing = std::env::var_os("POLYMER_GAME_LAYER_TIMING")
            .is_some()
            .then(std::time::Instant::now);
        let needs_state = self.state.borrow().is_none();
        if needs_state
            && self.error.is_none()
            && let Some(window) = self.pending_window.take()
        {
            let mut options = self
                .options
                .take()
                .expect("shell options are consumed exactly once");
            let shell_document = options.theme.load_document().await;
            let statistics_recorder = self
                .statistics_recorder
                .take()
                .expect("statistics recorder is consumed exactly once");
            let shell_factory = self
                .shell_factory
                .take()
                .expect("the shell document factory is consumed exactly once");
            let compositor_factory = self
                .compositor_factory
                .take()
                .expect("the shell rasterizer is consumed exactly once");
            match ShellCoordinator::new(
                window,
                self.proxy.clone(),
                options,
                statistics_recorder,
                shell_factory,
                compositor_factory,
            )
            .await
            {
                Ok(mut state) => match shell_document {
                    Ok(document) => match state
                        .activate_shell(Some(document), None, self.commands.clone())
                        .await
                    {
                        Ok(()) => *self.state.borrow_mut() = Some(state),
                        Err(error) => self.error = Some(error),
                    },
                    Err(error) => self.error = Some(error),
                },
                Err(error) => self.error = Some(error),
            }
        }
        let reload = self
            .state
            .borrow()
            .as_ref()
            .is_some_and(|state| state.shell.reload_pending());
        if reload {
            // Move the coordinator out so no RefCell borrow crosses the
            // reload's await, then return it before the next actor turn.
            let state = self.state.borrow_mut().take();
            if let Some(mut state) = state {
                state.reload_shell(self.commands.clone()).await;
                *self.state.borrow_mut() = Some(state);
            }
        }
        if let Some(started) = timing {
            let elapsed = started.elapsed();
            if elapsed >= Duration::from_millis(2) {
                tracing::debug!(
                    elapsed_ms = elapsed.as_secs_f64() * 1000.0,
                    "polymer: async runtime poll"
                );
            }
        }
    }

    pub(crate) async fn into_result(mut self) -> Result<(), Box<dyn Error>> {
        self.command_actors.shutdown();
        let state = self.state.borrow_mut().take();
        if let Some(state) = state {
            state.input.shutdown().await;
        }
        self.error.take().map_or(Ok(()), Err)
    }

    pub(crate) fn wants_native_window(&self) -> bool {
        self.pending_window.is_none() && self.state.borrow().is_none() && self.error.is_none()
    }

    pub(crate) fn titlebar(&self) -> bool {
        self.options
            .as_ref()
            .is_none_or(|options| options.theme.titlebar())
    }

    pub(crate) fn accept_native_window(&mut self, window: Window) {
        self.pending_window = Some(window);
    }

    pub(crate) fn fail(&mut self, error: impl Into<Box<dyn Error>>) {
        self.error = Some(error.into());
    }

    pub(crate) fn compositor_handle(&mut self) -> Option<CompositorHandle> {
        self.state
            .borrow()
            .as_ref()
            .map(|state| state.compositor.clone())
    }

    pub(crate) fn accept_inspector_window(
        &mut self,
        window: Window,
        address: std::net::SocketAddr,
    ) {
        if let Some(state) = self.state.borrow_mut().as_mut() {
            state.accept_inspector_window(window, address);
        }
    }

    pub(crate) fn inspector_window_id(&self) -> Option<winit::window::WindowId> {
        self.state
            .borrow()
            .as_ref()
            .and_then(|state| state.inspector.as_ref())
            .map(InspectorWindow::id)
    }

    pub(crate) fn handle_inspector_window_event(&self, event: &winit::event::WindowEvent) {
        if let Some(inspector) = self
            .state
            .borrow()
            .as_ref()
            .and_then(|state| state.inspector.as_ref())
        {
            inspector.handle_window_event(event);
        }
    }
}

impl ShellApplication {
    pub(crate) fn handle_user_event(&mut self, event: UserEvent) {
        let mut state_slot = self.state.borrow_mut();
        let Some(state) = state_slot.as_mut() else {
            return;
        };
        match event {
            // Drain promptly rather than waiting for `about_to_wait`. A busy
            // window can keep delivering compositor, WebView, and redraw
            // events for most of a frame; postponing the drain holds the
            // producer's three export slots and eventually makes it skip
            // captures. `process_capture` is bounded, and carries any overflow
            // into `about_to_wait`.
            UserEvent::Window(WindowCommand::WakeCapture) => state.process_capture(),
            UserEvent::Window(WindowCommand::CompositorTick) => state.handle_compositor_tick(),
            UserEvent::Window(WindowCommand::ShellRasterReady) => state.draw_pending_chrome(),
            UserEvent::Window(WindowCommand::ShellUpdated) => state.drain_shell(),
            UserEvent::Window(WindowCommand::SetCursor(icon)) => {
                state.compositor.borrow_mut().set_cursor(icon)
            }
            UserEvent::Window(WindowCommand::SetTitle(title)) => {
                state.compositor.borrow_mut().set_title(&title)
            }
            UserEvent::Window(WindowCommand::SetImeEnabled(enabled)) => {
                state.compositor.borrow_mut().set_ime_enabled(enabled);
            }
            UserEvent::Window(WindowCommand::SetImeCursorArea {
                x,
                y,
                width,
                height,
            }) => state
                .compositor
                .borrow_mut()
                .set_ime_cursor_area(x, y, width, height),
            UserEvent::InspectorWindowRequested { .. } => {}
            UserEvent::WakeActors => {}
        }
    }

    pub(crate) fn handle_device_event(&mut self, event: ShellDeviceEvent) {
        let mut state_slot = self.state.borrow_mut();
        let Some(state) = state_slot.as_mut() else {
            return;
        };
        if let ShellDeviceEvent::MouseMotion { delta_x, delta_y } = event {
            state.input.add_mouse_delta(delta_x, delta_y);
        }
    }

    pub(crate) fn handle_window_event(
        &mut self,
        event: ShellWindowEvent,
        overlay_consumed: bool,
    ) -> bool {
        if matches!(event, ShellWindowEvent::CloseRequested) {
            self.exit_requested = true;
            let mut state_slot = self.state.borrow_mut();
            let Some(state) = state_slot.as_mut() else {
                return true;
            };
            let Some(token) = state.game_host.active_token() else {
                return true;
            };
            self.command_actors.close(token);
            return false;
        }
        let mut state_slot = self.state.borrow_mut();
        let Some(state) = state_slot.as_mut() else {
            return false;
        };
        // At game priority the virtual pad owns keyboard events. egui reports
        // them consumed while its diagnostic overlay is focused, which used
        // to starve keyboard-backed controllers even though the overlay has no
        // editable keyboard controls.
        let keyboard_owned_by_game = matches!(&event, ShellWindowEvent::Keyboard { .. })
            && state.input.priority() == InputPriority::Game;
        if let ShellWindowEvent::Keyboard {
            physical_key: Some(code),
            pressed: true,
            repeat: false,
            ..
        } = &event
            && code == "KeyI"
            && state.input.dom_control_key()
        {
            state.open_inspector();
            return false;
        }
        let consumed = overlay_consumed && !keyboard_owned_by_game;
        if consumed {
            // egui took this event, so it is being interacted with and gets to
            // set its own repaint rate until the interaction settles.
            state
                .compositor
                .borrow_mut()
                .note_overlay_interaction(Instant::now());
        }
        if !consumed
            && let ShellWindowEvent::Keyboard {
                physical_key: Some(code),
                logical_key,
                pressed,
                repeat,
            } = &event
        {
            state.shell_keyboard(*pressed, logical_key.clone(), code.clone(), *repeat);
        }
        match event {
            ShellWindowEvent::CloseRequested => unreachable!(),
            ShellWindowEvent::DroppedFile(path) => {
                state.notify_polymer(PolymerNotification::DropActiveChanged(false));
                state.handle_dropped_file(path);
            }
            ShellWindowEvent::FileHover(active) => {
                state.notify_polymer(PolymerNotification::DropActiveChanged(active))
            }
            ShellWindowEvent::Moved => {}
            ShellWindowEvent::Resized { width, height } if width != 0 && height != 0 => {
                state.resize(width, height);
            }
            ShellWindowEvent::ScaleFactorChanged => {
                let size = state.compositor.borrow_mut().size();
                if size.width != 0 && size.height != 0 {
                    state.resize(size.width, size.height);
                }
            }
            ShellWindowEvent::FocusLost => state.handle_focus_lost(),
            ShellWindowEvent::Modifiers {
                alt,
                shift,
                control,
            } => {
                state.input.set_modifiers(alt, shift, control);
            }
            ShellWindowEvent::CursorMoved { x, y }
                if !consumed || state.input.has_active_pointer_sequence() =>
            {
                state.handle_cursor_moved(x, y, consumed);
            }
            ShellWindowEvent::CursorLeft => state.shell.input(None, ShellInput::PointerLeft),
            ShellWindowEvent::MouseInput { button, pressed }
                if !consumed || state.input.has_active_pointer_sequence() =>
            {
                state.handle_mouse_button(button, pressed, consumed);
            }
            ShellWindowEvent::MouseWheel {
                horizontal,
                vertical,
            } if !consumed => {
                state.handle_mouse_wheel(horizontal, vertical);
            }
            ShellWindowEvent::Ime(event) if !consumed => state.handle_ime(event),
            ShellWindowEvent::RedrawRequested => state.draw_overlay(),
            _ => {}
        }
        false
    }

    pub(crate) fn about_to_wait(&mut self) -> LoopDirective {
        if self.error.is_some() {
            return LoopDirective::Exit;
        }
        if self.exit_requested
            && self
                .state
                .borrow()
                .as_ref()
                .is_none_or(|state| !state.game_host.has_active_game())
        {
            return LoopDirective::Exit;
        }
        let now = Instant::now();
        let mut deadline = now + Duration::from_millis(100);
        if let Some(state) = self.state.borrow_mut().as_mut() {
            if state.game_host.has_capture_work() {
                state.process_capture();
            }
            // A frame the compositor deferred keeps its `latest` slot, so it
            // has to be retried from here. `process_capture` only runs when a
            // producer posts new work, which would hold an already-finished
            // frame back until the guest renders its next one.
            //
            // While the compositor clock is pacing, its tick is that retry, and
            // it lands in phase with DWM instead of at an arbitrary point in
            // the interval. A clock that cannot be waited on at all reports
            // itself unhealthy, which is what turns this fallback back on.
            if state.has_pending_surfaces() && !state.compositor.borrow_mut().is_pacing() {
                state.draw_pending_surfaces();
                deadline = deadline.min(now + PENDING_SURFACE_RETRY);
            }
            if state.compositor.borrow().scene_pending() {
                state.draw_pending_chrome();
                deadline = deadline.min(now + PENDING_SURFACE_RETRY);
            }
            let entered = state.profile.enter();
            state.publish_input_state();
            if state.input.controllers.begin_adoption() {
                self.command_actors.adopt_assignments();
            }
            state.profile.record(TurnPhase::Input, entered);
            deadline = deadline.min(state.input.next_publication());
            state.poll_target();
            state.release_held_input(now);
            if let Some(held) = state.held.deadline() {
                deadline = deadline.min(held);
            }
            if state.compositor.borrow_mut().overlay_repaint_due(now) {
                state.compositor.borrow_mut().request_redraw();
                // `next_overlay_repaint` remains in the past until Winit
                // delivers RedrawRequested and the overlay advances it. Do not
                // feed that expired instant back into ControlFlow::WaitUntil,
                // or the outer pump spins thousands of empty turns while the
                // redraw request is pending.
                deadline = deadline.min(now + Duration::from_millis(16));
            } else if let Some(repaint) = state.compositor.borrow_mut().next_overlay_repaint() {
                deadline = deadline.min(repaint);
            }
            // The clock is useful only while a new captured frame is waiting
            // for a present permit. Keeping it active for an idle connected
            // stream makes a signalled DXGI handle continuously wake the main
            // thread without displaying anything.
            let pending = state.has_pending_surfaces();
            state.compositor.borrow().set_clock_active(pending);
            if state.game_host.has_capture_work() {
                deadline = deadline.min(now + PENDING_SURFACE_RETRY);
            }
            state.profile.finish_turn(Instant::now());
            state
                .compositor
                .borrow_mut()
                .set_overlay_note(state.profile.report());
        }
        LoopDirective::WaitUntil(deadline)
    }
}

impl Drop for ShellApplication {
    fn drop(&mut self) {
        self.command_actors.shutdown();
        // Close recipients' access before any remaining actor futures can run.
        self.state.borrow_mut().take();
    }
}
