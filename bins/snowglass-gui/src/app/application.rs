use super::*;

pub(crate) struct ShellApplication {
    proxy: EventLoopProxy<UserEvent>,
    options: Option<ShellOptions>,
    statistics_recorder: Option<RecorderHandle>,
    shell_factory: Option<ShellSessionFactory>,
    shell_rasterizer: Option<ShellRasterizer>,
    pending_window: Option<WindowBootstrap>,
    state: Option<ShellCoordinator>,
    pending_input_commands: std::collections::VecDeque<InputCommand>,
    pending_input_change: Option<PendingInputChange>,
    pending_pause_resume: std::collections::VecDeque<PendingPauseResume>,
    pending_state_commands: std::collections::VecDeque<EmulatorCommand>,
    pending_state_action: Option<PendingStateAction>,
    pending_quit: Option<PendingQuit>,
    exit_requested: bool,
    error: Option<Box<dyn Error>>,
    next_runtime_poll: Instant,
}

impl ShellApplication {
    pub(crate) fn new(
        proxy: EventLoopProxy<UserEvent>,
        options: ShellOptions,
        statistics_recorder: RecorderHandle,
        shell_factory: ShellSessionFactory,
        shell_rasterizer: ShellRasterizer,
    ) -> Self {
        Self {
            proxy,
            options: Some(options),
            statistics_recorder: Some(statistics_recorder),
            shell_factory: Some(shell_factory),
            shell_rasterizer: Some(shell_rasterizer),
            pending_window: None,
            state: None,
            pending_input_commands: std::collections::VecDeque::new(),
            pending_input_change: None,
            pending_pause_resume: std::collections::VecDeque::new(),
            pending_state_commands: std::collections::VecDeque::new(),
            pending_state_action: None,
            pending_quit: None,
            exit_requested: false,
            error: None,
            next_runtime_poll: Instant::now(),
        }
    }

    pub(crate) async fn run_async_tasks(&mut self) {
        let timing = std::env::var_os("SNOWGLASS_GAME_LAYER_TIMING")
            .is_some()
            .then(std::time::Instant::now);
        if self.state.is_none()
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
            let shell_rasterizer = self
                .shell_rasterizer
                .take()
                .expect("the shell rasterizer is consumed exactly once");
            match ShellCoordinator::new(
                window,
                self.proxy.clone(),
                options,
                statistics_recorder,
                shell_factory,
                shell_rasterizer,
            )
            .await
            {
                Ok(mut state) => match shell_document {
                    Ok(document) => match state.activate_shell(Some(document), None).await {
                        Ok(()) => self.state = Some(state),
                        Err(error) => self.error = Some(error),
                    },
                    Err(error) => self.error = Some(error),
                },
                Err(error) => self.error = Some(error),
            }
        }
        let completed_change = self
            .pending_input_change
            .as_mut()
            .and_then(|pending| match pending.response.try_recv() {
                Ok(result) => Some(result),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Some(Err(
                    "controller plugin session closed during topology change".to_owned(),
                )),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
            });
        if let Some(result) = completed_change
            && let Some(pending) = self.pending_input_change.take()
            && let Some(state) = self.state.as_mut()
        {
            state.finish_input_change(pending, result);
        }
        loop {
            let completed = self
                .pending_pause_resume
                .front_mut()
                .and_then(|pending| match pending.response.try_recv() {
                    Ok(result) => Some(result),
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Some(Err(
                        "emulator plugin session closed during pause/resume".to_owned(),
                    )),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                });
            let Some(completed) = completed else {
                break;
            };
            let pending = self
                .pending_pause_resume
                .pop_front()
                .expect("a completed pause/resume callback remains queued");
            if let Some(state) = self.state.as_mut() {
                state.finish_pause_resume(pending, completed);
            }
        }
        let completed_state = self
            .pending_state_action
            .as_mut()
            .and_then(|pending| match pending.response.try_recv() {
                Ok(result) => Some(result),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Some(Err(
                    "emulator plugin session closed during save-state action".to_owned(),
                )),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
            });
        if let Some(result) = completed_state
            && let Some(pending) = self.pending_state_action.take()
            && let Some(state) = self.state.as_mut()
        {
            state.finish_state_action(pending, result);
        }
        let completed_quit =
            self.pending_quit
                .as_mut()
                .and_then(|pending| match pending.response.try_recv() {
                    Ok(result) => Some(result),
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Some(Err(
                        "emulator plugin session closed during onQuit".to_owned(),
                    )),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                });
        if let Some(result) = completed_quit
            && let Some(pending) = self.pending_quit.take()
            && let Some(state) = self.state.as_mut()
        {
            state.finish_quit(pending, result);
        }
        if self.pending_state_action.is_none()
            && let Some(command) = self.pending_state_commands.pop_front()
            && let Some(state) = self.state.as_mut()
            && let Some(pending) = state.apply_emulator_command(command)
        {
            match pending {
                PendingEmulatorCallback::PauseResume(pending) => {
                    self.pending_pause_resume.push_back(pending)
                }
                PendingEmulatorCallback::State(pending) => {
                    self.pending_state_action = Some(pending)
                }
                PendingEmulatorCallback::Quit(pending) => self.pending_quit = Some(pending),
            }
        }
        while self.pending_input_change.is_none()
            && let Some(command) = self.pending_input_commands.pop_front()
        {
            let Some(state) = self.state.as_mut() else {
                break;
            };
            self.pending_input_change = state.apply_input_command(command).await;
        }
        let now = Instant::now();
        let poll_runtime = now >= self.next_runtime_poll;
        if poll_runtime {
            self.next_runtime_poll = now + SHELL_RUNTIME_POLL_INTERVAL;
        }
        if poll_runtime
            && let Some(state) = self.state.as_mut()
            && state.shell_dom.poll_runtime().await
        {
            if let Some(started) = timing {
                let elapsed = started.elapsed();
                if elapsed >= Duration::from_millis(2) {
                    tracing::error!(
                        "snowglass: async runtime poll took {:.2}ms",
                        elapsed.as_secs_f64() * 1000.0,
                    );
                }
            }
            // Timers, promises, fetches, and WebSockets can mutate the
            // shell DOM without a Winit event. Polling applies those mutations
            // and marks the scene dirty; repaint it here so async
            // UI changes (including the launch fade) reach the compositor.
            if state.shell_dom.needs_paint() {
                state.refresh_shell();
            }
        }
    }

    pub(crate) async fn into_result(self) -> Result<(), Box<dyn Error>> {
        if let Some(state) = &self.state {
            state.input.shutdown().await;
        }
        self.error.map_or(Ok(()), Err)
    }

    pub(crate) fn wants_native_window(&self) -> bool {
        self.pending_window.is_none() && self.state.is_none() && self.error.is_none()
    }

    pub(crate) fn accept_native_window(&mut self, window: WindowBootstrap) {
        self.pending_window = Some(window);
    }

    pub(crate) fn fail(&mut self, error: impl Into<Box<dyn Error>>) {
        self.error = Some(error.into());
    }

    pub(crate) fn window_manager_mut(&mut self) -> Option<&mut WindowManager> {
        self.state.as_mut().map(|state| &mut state.window)
    }

    pub(crate) fn accept_inspector_window(
        &mut self,
        window: InspectorWindowBootstrap,
        address: std::net::SocketAddr,
    ) {
        if let Some(state) = self.state.as_mut() {
            state.accept_inspector_window(window, address);
        }
    }

    pub(crate) fn inspector_window_id(&self) -> Option<winit::window::WindowId> {
        self.state
            .as_ref()
            .and_then(|state| state.inspector.as_ref())
            .map(InspectorWindow::id)
    }

    pub(crate) fn handle_inspector_window_event(&self, event: &winit::event::WindowEvent) {
        if let Some(inspector) = self
            .state
            .as_ref()
            .and_then(|state| state.inspector.as_ref())
        {
            inspector.handle_window_event(event);
        }
    }
}

impl ShellApplication {
    pub(crate) fn handle_user_event(&mut self, event: UserEvent) {
        let event = match event {
            UserEvent::GameHost(GameHostCommand::Input(command)) => {
                if self.state.is_some() {
                    self.pending_input_commands.push_back(command);
                }
                return;
            }
            event => event,
        };
        let Some(state) = self.state.as_mut() else {
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
            UserEvent::Window(WindowCommand::RedrawShell) => state.refresh_shell(),
            UserEvent::Window(WindowCommand::SetCursor(icon)) => state.window.set_cursor(icon),
            UserEvent::Window(WindowCommand::SetTitle(title)) => state.window.set_title(&title),
            UserEvent::Window(WindowCommand::SetImeEnabled(enabled)) => {
                state.window.set_ime_enabled(enabled);
            }
            UserEvent::Window(WindowCommand::SetImeCursorArea {
                x,
                y,
                width,
                height,
            }) => state.window.set_ime_cursor_area(x, y, width, height),
            UserEvent::GameHost(GameHostCommand::Emulator(command)) => {
                if self.pending_state_action.is_some()
                    && matches!(
                        command,
                        EmulatorCommand::SaveState { .. } | EmulatorCommand::ResumeState { .. }
                    )
                {
                    self.pending_state_commands.push_back(command);
                    return;
                }
                if let Some(pending) = state.apply_emulator_command(command) {
                    match pending {
                        PendingEmulatorCallback::PauseResume(pending) => {
                            self.pending_pause_resume.push_back(pending)
                        }
                        PendingEmulatorCallback::State(pending) => {
                            self.pending_state_action = Some(pending)
                        }
                        PendingEmulatorCallback::Quit(pending) => self.pending_quit = Some(pending),
                    }
                }
            }
            UserEvent::GameHost(GameHostCommand::WebRtc(command)) => {
                state.apply_webrtc_command(command)
            }
            UserEvent::GameHost(GameHostCommand::Input(_)) => unreachable!(),
            UserEvent::InspectorWindowRequested { .. } => {}
            UserEvent::Inspector(command) => {
                command.handle(&state.shell_dom);
            }
        }
    }

    pub(crate) fn handle_device_event(&mut self, event: ShellDeviceEvent) {
        let Some(state) = self.state.as_mut() else {
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
            let Some(state) = self.state.as_mut() else {
                return true;
            };
            let Some(token) = state.game_host.active_token() else {
                return true;
            };
            if self.pending_quit.is_none()
                && let Some(pending) =
                    state.apply_emulator_command(EmulatorCommand::Close { token })
                && let PendingEmulatorCallback::Quit(pending) = pending
            {
                self.pending_quit = Some(pending);
            }
            return false;
        }
        let Some(state) = self.state.as_mut() else {
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
        let mut consumed = overlay_consumed && !keyboard_owned_by_game;
        let mut ui_prevented = false;
        if consumed {
            // egui took this event, so it is being interacted with and gets to
            // set its own repaint rate until the interaction settles.
            state.window.note_overlay_interaction(Instant::now());
        }
        if !consumed
            && let ShellWindowEvent::Keyboard {
                physical_key: Some(code),
                logical_key,
                pressed,
                repeat,
            } = &event
        {
            ui_prevented = state.shell_keyboard(
                if *pressed { "keydown" } else { "keyup" },
                logical_key.clone(),
                code.clone(),
                *repeat,
            );
            consumed = ui_prevented;
        }
        match event {
            ShellWindowEvent::CloseRequested => unreachable!(),
            ShellWindowEvent::DroppedFile(path) => {
                state.notify_snowglass(SnowglassNotification::DropActiveChanged(false));
                state.handle_dropped_file(path);
            }
            ShellWindowEvent::FileHover(active) => {
                state.notify_snowglass(SnowglassNotification::DropActiveChanged(active))
            }
            ShellWindowEvent::Moved => {}
            ShellWindowEvent::Resized { width, height } if width != 0 && height != 0 => {
                state.resize(width, height);
            }
            ShellWindowEvent::ScaleFactorChanged => {
                let size = state.window.size();
                if size.width != 0 && size.height != 0 {
                    state.resize(size.width, size.height);
                }
            }
            ShellWindowEvent::FocusLost => state.handle_focus_lost(),
            ShellWindowEvent::Keyboard {
                physical_key,
                pressed,
                ..
            } if state.input.keyboard_target(overlay_consumed, ui_prevented)
                == KeyboardTarget::Game =>
            {
                state.route_keyboard_to_game(physical_key, pressed);
            }
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
            ShellWindowEvent::CursorLeft if state.shell_dom.clear_pointer_hover() => {
                state.window.request_redraw();
            }
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
            ShellWindowEvent::RedrawRequested => {
                // Paint shell chrome at most once for the pointer samples Winit
                // coalesced into this redraw. This keeps expensive Blitz
                // resolve/paint work out of the mouse-message hot path.
                state.refresh_dirty_shell();
                state.draw_overlay();
            }
            _ => {}
        }
        false
    }

    pub(crate) fn about_to_wait(&mut self) -> LoopDirective {
        if self.error.is_some() {
            return LoopDirective::Exit;
        }
        if self.exit_requested
            && self.pending_quit.is_none()
            && self
                .state
                .as_ref()
                .is_none_or(|state| !state.game_host.has_active_game())
        {
            return LoopDirective::Exit;
        }
        let now = Instant::now();
        let mut deadline = now + Duration::from_millis(100);
        if let Some(state) = self.state.as_mut() {
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
            if state.has_pending_surfaces() && !state.window.is_pacing() {
                state.draw_pending_surfaces();
                deadline = deadline.min(now + PENDING_SURFACE_RETRY);
            }
            if state.shell_compositor.is_pending() {
                state.draw_pending_chrome();
                deadline = deadline.min(now + PENDING_SURFACE_RETRY);
            }
            let entered = state.profile.enter();
            state.publish_input_state();
            state.profile.record(TurnPhase::Input, entered);
            state.poll_target();
            if state.shell_dom.needs_paint() {
                state.refresh_shell();
                deadline = deadline.min(now + Duration::from_millis(16));
            }
            if state.shell_dom.is_active() {
                // Deno owns browser timers, fetch, WebSockets, promises, and
                // inspector traffic. Return to the Tokio loop regularly so it
                // can advance them even when the window is otherwise idle.
                deadline = deadline.min(now + Duration::from_millis(16));
            }
            if state.window.overlay_repaint_due(now) {
                state.window.request_redraw();
                // `next_overlay_repaint` remains in the past until Winit
                // delivers RedrawRequested and the overlay advances it. Do not
                // feed that expired instant back into ControlFlow::WaitUntil,
                // or the outer pump spins thousands of empty turns while the
                // redraw request is pending.
                deadline = deadline.min(now + Duration::from_millis(16));
            } else {
                deadline = deadline.min(state.window.next_overlay_repaint());
            }
            // The clock is useful only while a new captured frame is waiting
            // for a present permit. Keeping it active for an idle connected
            // stream makes a signalled DXGI handle continuously wake the main
            // thread without displaying anything.
            state.window.set_clock_active(state.has_pending_surfaces());
            if state.game_host.has_capture_work() {
                deadline = deadline.min(now + PENDING_SURFACE_RETRY);
            }
            state.profile.finish_turn(Instant::now());
            state.window.set_overlay_note(state.profile.report());
        }
        LoopDirective::WaitUntil(deadline)
    }
}
