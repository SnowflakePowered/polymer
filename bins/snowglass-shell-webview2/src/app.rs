use crate::cli;
use crate::cli::ShellOptions;
use crate::commands::ShellDocumentSource;
use crate::game_host::{GameHost, GameHostCaptureEvent, GameHostCommand, ShellRasterizer};
use crate::input::{InputPriority, InputRouter, InputTransport, KeyboardTarget};
use crate::instance_events;
use crate::launch::AttachedLaunchPlan;
use crate::metrics::{TurnPhase, TurnProfile};
use crate::shell_document::{DomKeyboardEvent, DomMouseEvent, DomWheelEvent, ShellDocumentFactory};
use crate::shell_dom::ShellDomCoordinator;
use crate::shell_runtime::{
    EmulatorCommand, EmulatorCommandSink, InstanceToken, WebRtcCommand, WebRtcCommandSink,
};
use crate::window_manager::{
    EventLoopProxy, LoopDirective, ShellDeviceEvent, ShellImeEvent, ShellWindowEvent,
    WindowBootstrap, WindowCommand, WindowManager,
};
use snowglass_input::{hub::InputTransaction, mouse::PointerPhase};
use snowglass_input_dinput::DirectInputForwarder;
use snowglass_renderer::{CompositingPlan, Extent, cli::ViewerApi};
use snowglass_statistics::RecorderHandle;
use std::{
    error::Error,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) use crate::commands::UserEvent;

// Capture can produce events much faster than the window message queue when
// several streams are active. Keep each dispatch small so native window input
// and egui repaint messages get a turn between capture batches.
const MAX_CAPTURE_EVENTS_PER_TURN: usize = 32;
/// Composition-tree key for the single visual every stream shares when the
/// shell composites them together. Not a real connection.
/// How many game views the layer document may show at once.
///
/// This fixes how many chrome bands are kept, which is one more than the view
/// count. Overshooting costs a command buffer each and nothing per frame, so it
/// is set well above what a console layout needs rather than derived from the
/// document, which can change under us.
const SHELL_SURFACE_CAPACITY: usize = 8;
const PENDING_SURFACE_RETRY: Duration = Duration::from_millis(1);

/// How long a compositor-clock tick waits for DXGI to return a present slot.
///
/// The clock ticks when DWM begins a composition pass; the slot for the frame
/// that pass retires is released a moment later. Polling at the tick therefore
/// misses it by a hair and defers the present a whole interval, which is the
/// judder this budget exists to close. Expected waits are a fraction of a
/// millisecond — the bound is only here so a stalled swapchain cannot hold the
/// event loop and Deno UI isolate that share this thread.
const PRESENT_PERMIT_WAIT: Duration = Duration::from_millis(3);
/// Native input and emulator events dispatch into JavaScript
/// synchronously. Poll the browser's asynchronous work once per UI frame
/// instead of once per cursor, capture, and compositor event.
const SHELL_RUNTIME_POLL_INTERVAL: Duration = Duration::from_millis(16);

struct ShellServices {
    session: String,
    proxy: EventLoopProxy<UserEvent>,
}

struct ShellCoordinator {
    shell_dom: ShellDomCoordinator,
    window: WindowManager,
    profile: TurnProfile,
    services: ShellServices,
    game_host: GameHost,
    input: InputRouter,
    startup_request: Option<serde_json::Value>,
    viewer: snowglass_renderer::cli::ViewerOptions,
}

impl ShellCoordinator {
    fn new(
        window: WindowBootstrap,
        proxy: EventLoopProxy<UserEvent>,
        options: ShellOptions,
        statistics_recorder: RecorderHandle,
        shell_factory: ShellDocumentFactory,
        shell_rasterizer: ShellRasterizer,
    ) -> Result<Self, Box<dyn Error>> {
        let ShellOptions {
            viewer: options,
            ui_url: _,
            plugins: _,
            game_launch,
        } = options;
        if options.api != ViewerApi::D3d12 {
            return Err(
                "snowglass-shell-webview2 requires `--api d3d12` for DirectComposition".into(),
            );
        }
        let startup_request = match game_launch {
            Some(request) => Some(serde_json::json!({
                "kind": "package",
                "path": request.game.to_string_lossy(),
                "emulator": request.emulator,
            })),
            None => cli::launch_from_viewer(&options)
                .map_err(std::io::Error::other)?
                .map(|options| serde_json::json!({ "kind": "launch", "options": options })),
        };

        let mut window = WindowManager::new(window, statistics_recorder, proxy.clone())?;
        let size = window.size();
        let render_window = window.surface(size.width.max(1), size.height.max(1));
        // Prepare the retained Vello renderer before the first shell frame.
        let game_host = GameHost::new(&render_window, shell_rasterizer)?;
        let merged_content = game_host.composition_content()?;
        drop(render_window);
        window.set_merged_content(&merged_content)?;
        window.commit()?;
        let session = unique_session();
        let shell_dom = ShellDomCoordinator::new(shell_factory);

        let input = InputTransport::runtime().map_err(std::io::Error::other)?;
        let control_sender = input.control_sender();
        // Merging composites every stream into one opaque window-covering
        // swapchain. The shell document supplies the chrome that it draws
        // around the streams, which is what lets it stay opaque and so stay
        // eligible for a hardware overlay plane.

        let state = Self {
            shell_dom,
            window,
            profile: TurnProfile::default(),
            services: ShellServices { session, proxy },
            game_host,
            input: InputRouter::new(input, DirectInputForwarder::new(control_sender)),
            startup_request,
            viewer: options,
        };
        Ok(state)
    }

    fn refresh_after_shell_script(&mut self) {
        if self.shell_dom.needs_paint() {
            self.refresh_shell();
        }
    }

    fn apply_emulator_command(&mut self, command: EmulatorCommand) {
        match command {
            EmulatorCommand::Launch {
                token,
                options,
                result,
            } => {
                let _ = result.send(self.launch_game(token, options));
            }
            EmulatorCommand::Close(token) => self.close_game(token),
            EmulatorCommand::SetVolume { token, volume } => self.set_game_volume(token, volume),
            EmulatorCommand::SetMuted { token, muted } => self.set_game_muted(token, muted),
            EmulatorCommand::Message { token, name, args } => self.game_message(token, &name, args),
            EmulatorCommand::Input { token, active } if self.is_active(token) => {
                self.set_input_priority(if active {
                    InputPriority::Game
                } else {
                    InputPriority::Ui
                });
            }
            EmulatorCommand::Input { .. } => {}
            EmulatorCommand::ConfigureInput {
                request_id,
                transaction,
            } => self.configure_input(u64::from(request_id), transaction),
        }
    }

    fn apply_webrtc_command(&mut self, command: WebRtcCommand) {
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

    fn is_active(&self, token: InstanceToken) -> bool {
        self.game_host.is_active(token)
    }

    fn configure_input(&mut self, request_id: u64, transaction: InputTransaction) {
        match self
            .input
            .apply_configuration_transaction(transaction, self.game_host.has_active_game())
        {
            Ok(configuration) => {
                self.publish_input_configuration();
                self.send_input_configuration_result(request_id, Ok(configuration));
            }
            Err(error) => {
                self.send_input_configuration_result(request_id, Err(error.to_string()));
            }
        }
    }

    fn publish_input_configuration(&mut self) {
        let publication = self.input.configuration_publication();
        self.shell_dom
            .set_input_configuration(&publication.configuration, &publication.layouts);
    }

    fn send_input_configuration_result(
        &mut self,
        request_id: u64,
        result: Result<snowglass_input::hub::InputConfiguration, String>,
    ) {
        let (result, _) = InputRouter::configuration_result(request_id, result);
        self.shell_dom
            .input_configuration_result(request_id, &result);
    }

    async fn activate_shell(
        &mut self,
        source: Option<ShellDocumentSource>,
        source_error: Option<String>,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(error) = source_error {
            tracing::error!("snowglass: loading the shell document failed: {error}");
        }
        if let Some(shell_source) = source {
            let size = self.window.size();
            let command_proxy = self.services.proxy.clone();
            let webrtc_proxy = self.services.proxy.clone();
            let html = shell_source.html.clone();
            let base_url = shell_source.base_url.clone();
            let viewer = self.viewer.clone();
            let startup_request = self.startup_request.take();
            let activated = match self
                .shell_dom
                .activate(|factory| async move {
                    let emulator_commands = EmulatorCommandSink::new(move |command| {
                        if command_proxy
                            .send_event(UserEvent::GameHost(GameHostCommand::Emulator(command)))
                            .is_err()
                        {
                            tracing::error!(
                                "snowglass: emulator command event loop is unavailable"
                            );
                        }
                    });
                    let webrtc_commands = WebRtcCommandSink::new(move |command| {
                        if webrtc_proxy
                            .send_event(UserEvent::GameHost(GameHostCommand::WebRtc(command)))
                            .is_err()
                        {
                            tracing::error!("snowglass: WebRTC command event loop is unavailable");
                        }
                    });
                    factory
                        .create(
                            &html,
                            base_url,
                            SHELL_SURFACE_CAPACITY,
                            size.width,
                            size.height,
                            emulator_commands,
                            webrtc_commands,
                            startup_request,
                            viewer,
                        )
                        .await
                })
                .await
            {
                Ok(activated) => activated,
                Err(error) => {
                    self.shell_dom.disable();
                    return Err(error);
                }
            };
            if !activated {
                tracing::error!(
                    "snowglass: ignoring a second shell document at {}",
                    shell_source.base_url
                );
                return Ok(());
            }
            self.refresh_shell();
            tracing::error!(
                "snowglass: using shell document at {}",
                shell_source.base_url
            );
        } else {
            // Failed/absent documents do not retain an inactive DOM slot.
            self.shell_dom.disable();
        }
        self.publish_input_configuration();
        Ok(())
    }

    fn handle_compositor_tick(&mut self) {
        self.window.clear_clock_tick();
        self.window.record_clock();
        self.refresh_pending_shell();
        self.present_pending_surfaces(PRESENT_PERMIT_WAIT);
        // A DXGI latency handle can remain signalled after the available frame
        // has been consumed. Stop waiting until capture publishes more work so
        // the clock thread cannot flood the UI loop with empty ticks.
        self.window.set_clock_active(self.has_pending_surfaces());
    }

    fn launch_game(
        &mut self,
        token: InstanceToken,
        options: AttachedLaunchPlan,
    ) -> Result<(), String> {
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
            .configure_launch(&options.machine, &options.gamepad_mapping)
        {
            self.emit_instance_event(token, "error", &instance_events::error(&error));
            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            return Err(error);
        }
        self.publish_input_configuration();
        let wake_proxy = self.services.proxy.clone();
        match self
            .game_host
            .launch(token, options, self.services.session.clone(), move || {
                wake_proxy
                    .send_event(UserEvent::Window(WindowCommand::WakeCapture))
                    .is_ok()
            }) {
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

    fn close_game(&mut self, token: InstanceToken) {
        if !self.game_host.request_close(token) {
            return;
        }
        self.input.detach();
        self.finish_active_game();
    }

    fn finish_active_game(&mut self) {
        self.input.clear_provider_state();
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
        self.emit_instance_event(finished.token, "closed", &serde_json::Value::Null);
    }

    fn game_message(&mut self, token: InstanceToken, name: &str, args: serde_json::Value) {
        if self.is_active(token) {
            self.emit_instance_event(token, "message", &instance_events::message(name, args));
        }
    }

    fn set_game_volume(&mut self, token: InstanceToken, volume: f64) {
        if self.game_host.set_volume(token, volume) {
            self.publish_audio_state(false);
        }
    }

    fn set_game_muted(&mut self, token: InstanceToken, muted: bool) {
        if self.game_host.set_muted(token, muted) {
            self.publish_audio_state(false);
        }
    }

    fn publish_audio_state(&mut self, force: bool) {
        let Some((token, audio)) = self.game_host.audio_change(force) else {
            return;
        };
        self.emit_instance_event(token, "audioChanged", &instance_events::audio(audio));
    }

    fn emit_instance_event(
        &mut self,
        token: InstanceToken,
        event_name: &str,
        payload: &serde_json::Value,
    ) {
        let event = instance_events::envelope(token, event_name, payload);
        let shell_changed = self.shell_dom.dispatch_emulator_event(&event);
        if shell_changed {
            self.refresh_shell();
        }
    }

    fn process_capture(&mut self) {
        let entered = self.profile.enter();
        self.process_capture_inner();
        self.profile.record(TurnPhase::Capture, entered);
    }

    fn process_capture_inner(&mut self) {
        // Leave the notification latched while draining. Producers can mark
        // `capture_dirty`, but cannot flood the event loop with one user event
        // per captured frame.
        self.game_host.begin_capture_drain();
        let mut changed = false;
        let mut processed = 0;
        while processed < MAX_CAPTURE_EVENTS_PER_TURN {
            let event = {
                let size = self.window.size();
                let render_window = self.window.surface(size.width.max(1), size.height.max(1));
                self.game_host.try_capture_event(&render_window)
            };
            let Some(event) = event else {
                break;
            };
            processed += 1;
            match self.game_host.take_composition_content_update() {
                Ok(Some(content)) => {
                    if let Err(error) = self.window.set_merged_content(&content) {
                        tracing::error!(
                            "snowglass: attaching replacement game compositor failed: {error}"
                        );
                    } else if let Err(error) = self.window.commit() {
                        tracing::error!(
                            "snowglass: committing replacement game compositor failed: {error}"
                        );
                    }
                    self.shell_dom.invalidate();
                }
                Ok(None) => {}
                Err(error) => tracing::error!(
                    "snowglass: obtaining replacement game compositor content failed: {error}"
                ),
            }
            match event {
                GameHostCaptureEvent::Connected(accepted) => {
                    let Some(surface) = accepted.surface else {
                        continue;
                    };
                    if let Some(previous) = surface.replaced_connection {
                        self.window.remove_target(previous);
                        self.cancel_pointer_connection(previous);
                    }
                    self.window.set_backend(surface.backend);
                    self.window.add_surface(
                        surface.connection,
                        surface.surface_id,
                        surface.target_index,
                    );
                    self.refresh_shell();
                    changed = true;
                }
                GameHostCaptureEvent::Rejected { connection, error } => {
                    tracing::error!("snowglass: rejected capture stream {connection}: {error}");
                }
                GameHostCaptureEvent::Frame {
                    connection,
                    frame,
                    dimensions_changed,
                } => {
                    self.window.observe_frame(connection, frame);
                    if dimensions_changed {
                        changed = true;
                    }
                }
                GameHostCaptureEvent::Removed {
                    connection,
                    layout_changed,
                } => {
                    self.window.remove_target(connection);
                    self.cancel_pointer_connection(connection);
                    if layout_changed {
                        changed = true;
                    }
                }
            }
        }
        if changed && let Some(token) = self.game_host.active_token() {
            self.publish_targets_for(token);
        }
        self.draw_unpaced_surfaces();
        // A producer that ran while the notification was latched could not
        // enqueue another WakeCapture. Carry that work into the next event-loop
        // cycle instead. Resetting the latch before reading `capture_dirty`
        // closes the race: a later producer will enqueue a fresh notification.
        self.game_host
            .finish_capture_drain(processed == MAX_CAPTURE_EVENTS_PER_TURN);
    }

    /// Presents pending content only while nothing else is pacing the shell.
    ///
    /// Once the compositor clock is running it owns every present, so that each
    /// one lands at the start of a composition interval rather than wherever
    /// the guest happened to finish a frame. Presenting on arrival puts it at
    /// an arbitrary phase against DWM's sampling deadline, and a guest and a
    /// display at nominally the same rate drift that phase slowly across the
    /// deadline — a stretch of doubled and dropped frames every time it
    /// crosses.
    fn draw_unpaced_surfaces(&mut self) {
        if !self.window.is_pacing() {
            self.draw_pending_surfaces();
        }
    }

    fn draw_pending_surfaces(&mut self) {
        self.present_pending_surfaces(Duration::ZERO);
    }

    /// Presents every surface holding new content, waiting up to `permit_wait`
    /// for DXGI to hand back a present slot.
    fn present_pending_surfaces(&mut self, permit_wait: Duration) {
        let entered = self.profile.enter();
        let timing = std::env::var_os("SNOWGLASS_GAME_LAYER_TIMING")
            .is_some()
            .then(std::time::Instant::now);
        let size = self.window.size();
        let presented = match self.game_host.draw(size.width, size.height, permit_wait) {
            Ok(Some(carried_new_frame)) => {
                self.window.record_present(carried_new_frame);
                true
            }
            Ok(None) => false,
            Err(error) => {
                tracing::error!("snowglass: rendering merged surfaces failed: {error}");
                false
            }
        };
        if let Some(started) = timing {
            let elapsed = started.elapsed();
            tracing::error!(
                "snowglass: compositor {} in {:.2}ms (permit wait {:.2}ms)",
                if presented { "presented" } else { "deferred" },
                elapsed.as_secs_f64() * 1000.0,
                permit_wait.as_secs_f64() * 1000.0,
            );
        }
        self.profile.record(TurnPhase::Present, entered);
    }

    fn has_pending_surfaces(&self) -> bool {
        // Re-presenting an unchanged shared buffer burns GPU time and can
        // occupy the one-frame DXGI queue when a real guest frame arrives.
        // DirectComposition retains the last buffer, so only new content or
        // chrome needs another present.
        self.game_host.has_pending_frame()
    }

    /// Re-lays out the shell document and moves the compositor to match.
    ///
    /// The chrome is uploaded band by band and the game views are placed
    /// between them, so a document whose bezel sits over a fullscreen view
    /// composites in the order the document painted it.
    ///
    /// Before a stream connects, chrome can be shown in temporary
    /// DirectComposition surfaces. Once the opaque merged swapchain exists,
    /// chrome goes inside it or it would be hidden.
    fn refresh_shell(&mut self) {
        let entered = self.profile.enter();
        self.refresh_shell_inner();
        self.profile.record(TurnPhase::Shell, entered);
    }

    fn refresh_shell_inner(&mut self) {
        self.game_host.clear_shell_refresh_request();
        let Some(frame) = self.shell_dom.produce_frame() else {
            return;
        };
        if let Err(error) = self.game_host.rasterize_shell(&frame) {
            tracing::error!("snowglass: rasterizing shell chrome failed: {error}");
            return;
        }
        let placements = self.shell_dom.tracked_surface_layout();
        self.game_host.apply_tracked_layout(&placements);
    }

    /// Dispatches a regular DOM mouse event to the Deno-backed shell.
    /// Script claims the pointer from the guest by cancelling the event.
    fn shell_mouse(
        &mut self,
        kind: &str,
        x: f32,
        y: f32,
        button: u16,
        pressed: Option<bool>,
    ) -> bool {
        let timing = std::env::var_os("SNOWGLASS_GAME_LAYER_TIMING")
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
            // Pointer-driven chrome must follow the input sample that produced
            // it. The DirectComposition clock can be throttled well below the
            // monitor refresh rate while the composition tree is otherwise
            // stable, so deferring Ark drag/resize mutations to that clock
            // turns inexpensive DOM updates into visibly stale 15 Hz motion.
            //
            // Raster now and make a zero-wait present attempt. If DXGI has no
            // permit, `chrome_dirty` remains set and the ordinary compositor
            // clock retries it without ever blocking the input handler.
            self.refresh_shell();
            self.draw_pending_surfaces();
        }
        self.refresh_after_shell_script();
        if kind == "mousemove"
            && let (Some(started), Some(hit_elapsed), Some(dispatch_elapsed)) =
                (timing, hit_elapsed, dispatch_elapsed)
        {
            tracing::error!(
                "snowglass: mousemove hit {:.2}ms dispatch {:.2}ms total {:.2}ms",
                hit_elapsed.as_secs_f64() * 1000.0,
                (dispatch_elapsed - hit_elapsed).as_secs_f64() * 1000.0,
                started.elapsed().as_secs_f64() * 1000.0,
            );
        }
        prevented
    }

    fn shell_keyboard(&mut self, kind: &str, key: String, code: String, repeat: bool) -> bool {
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
        self.refresh_after_shell_script();
        prevented
    }

    fn refresh_pending_shell(&mut self) {
        if self.game_host.take_shell_refresh_request() {
            self.refresh_shell();
        }
    }

    fn pointer_presentation(&self) -> (snowglass_renderer::Presentation, CompositingPlan) {
        let size = self.window.size();
        self.game_host
            .pointer_presentation(Extent::new(size.width, size.height))
    }

    fn route_pointer(&mut self, phase: PointerPhase, button: u32, horizontal: i32, vertical: i32) {
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

    fn set_input_priority(&mut self, priority: InputPriority) {
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
            .set_provider_enabled(priority == InputPriority::Game);
        self.input.replace_priority(priority);
        self.focus_game_input();
    }

    fn focus_game_input(&self) {
        self.window.focus_host();
    }

    fn cancel_pointer_connection(&mut self, connection: u64) {
        let (presentation, plan) = self.pointer_presentation();
        self.input.cancel_pointer_connection(
            connection,
            &presentation,
            &plan,
            self.game_host.pointer_sender(),
        );
    }

    fn handle_dropped_file(&mut self, path: std::path::PathBuf) {
        self.shell_dom.request_package(&serde_json::json!({
            "kind": "package",
            "path": path.to_string_lossy(),
        }));
        self.refresh_after_shell_script();
    }

    fn resize(&mut self, width: u32, height: u32) {
        if let Err(error) = self.window.resize_overlay() {
            tracing::error!("snowglass: resizing egui composition overlay failed: {error}");
        }
        if self.shell_dom.resize(width, height) {
            self.refresh_shell();
        }
        let render_window = self.window.surface(width, height);
        if let Err(error) = self.game_host.resize_compositor(&render_window) {
            tracing::error!("snowglass: resizing the shared swapchain failed: {error}");
        }
        self.window.request_redraw();
    }

    fn handle_focus_lost(&mut self) {
        if let Some(connection) = self.input.captured_connection() {
            self.cancel_pointer_connection(connection);
        }
        let cursor = self.input.cursor();
        for (button, _) in self.input.focus_lost_buttons() {
            self.shell_mouse(
                "mouseup",
                cursor.x as f32,
                cursor.y as f32,
                button,
                Some(false),
            );
        }
        self.input.finish_focus_lost();
    }

    fn route_keyboard_to_game(&mut self, key: Option<String>, pressed: bool, repeat: bool) {
        let Some(key) = key else { return };
        if !self.input.route_game_key(&key, pressed) {
            self.publish_keyboard_input(&key, pressed, repeat);
        }
    }

    fn handle_cursor_moved(&mut self, x: f64, y: f64, consumed: bool) {
        self.input.set_cursor(x, y);
        if consumed {
            return;
        }
        let Some(hit) = self.shell_dom.hit_test(x as f32, y as f32) else {
            return;
        };
        let prevented = self.shell_mouse("mousemove", x as f32, y as f32, 0, None);
        if prevented || !hit.over_tracked_surface {
            return;
        }
        self.route_pointer(PointerPhase::Move, 0, 0, 0);
    }

    fn handle_mouse_button(&mut self, button: Option<u16>, pressed: bool, consumed: bool) {
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
        let Some(hit) = self.shell_dom.hit_test(cursor.x as f32, cursor.y as f32) else {
            return;
        };
        let kind = if pressed { "mousedown" } else { "mouseup" };
        let prevented = self.shell_mouse(
            kind,
            cursor.x as f32,
            cursor.y as f32,
            dom_button,
            Some(pressed),
        );
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

    fn handle_mouse_wheel(&mut self, horizontal: i32, vertical: i32) {
        let cursor = self.input.cursor();
        let Some(hit) = self.shell_dom.hit_test(cursor.x as f32, cursor.y as f32) else {
            return;
        };
        let claimed = self.shell_dom.wheel_event(
            hit,
            DomWheelEvent {
                x: cursor.x as f32,
                y: cursor.y as f32,
                horizontal: f64::from(horizontal),
                vertical: f64::from(vertical),
                ctrl_key: self.input.dom_control_key(),
                shift_key: self.input.dom_shift_key(),
                alt_key: self.input.dom_alt_key(),
            },
        );
        self.refresh_after_shell_script();
        if !claimed && hit.over_tracked_surface {
            self.route_pointer(PointerPhase::Wheel, 0, horizontal, vertical);
        }
    }

    fn handle_ime(&mut self, event: ShellImeEvent) {
        let event = match event {
            ShellImeEvent::Enabled => blitz_traits::events::BlitzImeEvent::Enabled,
            ShellImeEvent::Preedit(value, range) => {
                blitz_traits::events::BlitzImeEvent::Preedit(value, range)
            }
            ShellImeEvent::Commit(value) => blitz_traits::events::BlitzImeEvent::Commit(value),
            ShellImeEvent::Disabled => blitz_traits::events::BlitzImeEvent::Disabled,
        };
        self.shell_dom.ime_event(event);
        self.refresh_after_shell_script();
    }

    fn draw_overlay(&mut self) {
        let entered = self.profile.enter();
        if let Err(error) = self.window.draw_overlay() {
            tracing::error!("snowglass: drawing egui composition overlay failed: {error}");
        }
        self.profile.record(TurnPhase::Overlay, entered);
    }

    fn publish_input_state(&mut self) {
        if !self.input.publication_due(Instant::now()) {
            return;
        }
        if self.input.guide_button_pressed() && self.input.priority() == InputPriority::Game {
            self.set_input_priority(InputPriority::Ui);
        }
        let publication = self.input.build_publication(
            self.game_host.gamepad_mapping(),
            self.game_host.has_active_game(),
        );
        self.window.set_input_diagnostics(publication.diagnostics);
        self.shell_dom
            .set_input_state(&publication.controllers, publication.priority);
        let Some(state_changed) = publication.state_changed else {
            return;
        };
        if !state_changed && publication.routed.is_empty() {
            return;
        }
        self.window.request_overlay_repaint();
        let _ = publication.routed;
    }

    fn publish_keyboard_input(&mut self, key: &str, pressed: bool, repeat: bool) {
        let _ = (key, pressed, repeat);
    }

    fn publish_targets_for(&mut self, token: InstanceToken) {
        let payload = instance_events::surfaces(self.game_host.surface_targets());
        self.emit_instance_event(token, "surfacesChanged", &payload);
    }

    fn poll_target(&mut self) {
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

pub(crate) struct ShellApplication {
    proxy: EventLoopProxy<UserEvent>,
    options: Option<ShellOptions>,
    statistics_recorder: Option<RecorderHandle>,
    shell_factory: Option<ShellDocumentFactory>,
    shell_rasterizer: Option<ShellRasterizer>,
    pending_window: Option<WindowBootstrap>,
    state: Option<ShellCoordinator>,
    error: Option<Box<dyn Error>>,
    next_runtime_poll: Instant,
}

impl ShellApplication {
    pub(crate) fn new(
        proxy: EventLoopProxy<UserEvent>,
        options: ShellOptions,
        statistics_recorder: RecorderHandle,
        shell_factory: ShellDocumentFactory,
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
            let options = self
                .options
                .take()
                .expect("shell options are consumed exactly once");
            let shell_document = load_shell_document(&options.ui_url).await;
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
            ) {
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
            state.refresh_after_shell_script();
        }
    }

    pub(crate) fn into_result(self) -> Result<(), Box<dyn Error>> {
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
}

impl ShellApplication {
    pub(crate) fn handle_user_event(&mut self, event: UserEvent) {
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
                state.apply_emulator_command(command)
            }
            UserEvent::GameHost(GameHostCommand::WebRtc(command)) => {
                state.apply_webrtc_command(command)
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
        let Some(state) = self.state.as_mut() else {
            return false;
        };
        // At game priority the virtual pad owns keyboard events. egui reports
        // them consumed while its diagnostic overlay is focused, which used
        // to starve keyboard-backed controllers even though the overlay has no
        // editable keyboard controls.
        let keyboard_owned_by_game = matches!(&event, ShellWindowEvent::Keyboard { .. })
            && state.input.priority() == InputPriority::Game;
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
            ShellWindowEvent::CloseRequested => return true,
            ShellWindowEvent::DroppedFile(path) => {
                state.shell_dom.set_drop_active(false);
                state.handle_dropped_file(path);
            }
            ShellWindowEvent::FileHover(active) => state.shell_dom.set_drop_active(active),
            ShellWindowEvent::Moved => {}
            ShellWindowEvent::Resized { width, height } if width != 0 && height != 0 => {
                state.resize(width, height);
            }
            ShellWindowEvent::FocusLost => state.handle_focus_lost(),
            ShellWindowEvent::Keyboard {
                physical_key,
                pressed,
                repeat,
                ..
            } if state.input.keyboard_target(overlay_consumed, ui_prevented)
                == KeyboardTarget::Game =>
            {
                state.route_keyboard_to_game(physical_key, pressed, repeat);
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
            ShellWindowEvent::CursorLeft => {
                if state.shell_dom.clear_pointer_hover() {
                    state.refresh_shell();
                }
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
            ShellWindowEvent::RedrawRequested => state.draw_overlay(),
            _ => {}
        }
        false
    }

    pub(crate) fn about_to_wait(&mut self) -> LoopDirective {
        if self.error.is_some() {
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

fn unique_session() -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("snowglass-{}-{nonce:x}", std::process::id())
}

async fn load_shell_document(url: &str) -> Result<ShellDocumentSource, Box<dyn Error>> {
    let specifier = deno_core::ModuleSpecifier::parse(url)?;
    let html = if specifier.scheme() == "file" {
        let path = specifier
            .to_file_path()
            .map_err(|_| format!("invalid shell file URL: {url}"))?;
        tokio::fs::read_to_string(path).await?
    } else {
        reqwest::get(specifier.clone())
            .await?
            .error_for_status()?
            .text()
            .await?
    };
    Ok(ShellDocumentSource {
        html,
        base_url: specifier.to_string(),
    })
}
