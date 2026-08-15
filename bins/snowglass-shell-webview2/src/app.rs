use crate::background_document::{BackgroundDocumentFactory, DomKeyboardEvent, DomMouseEvent};
use crate::background_dom::BackgroundDomCoordinator;
use crate::background_runtime::{
    BackgroundLaunchCoordinator, EmulatorCommand, EmulatorCommandSink, InstanceToken,
    LaunchCoordinatorEvent,
};
use crate::cli::{GameLaunchRequest, ShellOptions};
use crate::commands::{BackgroundDocumentSource, RuntimeCommand, decode_foreground_command};
use crate::foreground_dom::ForegroundDom;
use crate::game_host::{BackgroundRasterizer, GameHost, GameHostCaptureEvent, GameHostCommand};
use crate::input::{
    DomPointerKind, InputCommand, InputPriority, InputRouter, InputTransport, KeyboardTarget,
    PointerTarget,
};
use crate::instance_events;
use crate::launch::AttachedLaunchPlan;
use crate::metrics::{TurnPhase, TurnProfile};
use crate::shared_bus::{SharedBusBroker, SharedBusRequest};
use crate::window_manager::{
    EventLoopProxy, LoopDirective, ShellDeviceEvent, ShellWindowEvent, WindowBootstrap,
    WindowCommand, WindowManager,
};
use crate::{cli, launcher};
use snowglass_input::{hub::InputTransaction, mouse::PointerPhase};
use snowglass_input_dinput::DirectInputForwarder;
use snowglass_renderer::{CompositingPlan, Extent, cli::ViewerApi};
use snowglass_statistics::RecorderHandle;
use std::{
    error::Error,
    sync::Arc,
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
const BACKGROUND_SURFACE_CAPACITY: usize = 8;
const PENDING_SURFACE_RETRY: Duration = Duration::from_millis(1);

/// How long a compositor-clock tick waits for DXGI to return a present slot.
///
/// The clock ticks when DWM begins a composition pass; the slot for the frame
/// that pass retires is released a moment later. Polling at the tick therefore
/// misses it by a hair and defers the present a whole interval, which is the
/// judder this budget exists to close. Expected waits are a fraction of a
/// millisecond — the bound is only here so a stalled swapchain cannot hold the
/// event loop, and the WebView2 host that shares this thread with it.
const PRESENT_PERMIT_WAIT: Duration = Duration::from_millis(3);

struct ShellServices {
    session: String,
    proxy: EventLoopProxy<UserEvent>,
}

struct ShellCoordinator {
    // Drop WebView2 and its COM host before tearing down their visual tree.
    foreground_dom: ForegroundDom,
    background_dom: BackgroundDomCoordinator,
    bus_broker: SharedBusBroker,
    foreground_bus_attached: bool,
    window: WindowManager,
    profile: TurnProfile,
    services: ShellServices,
    launch: BackgroundLaunchCoordinator,
    game_host: GameHost,
    input: InputRouter,
}

impl ShellCoordinator {
    fn new(
        window: WindowBootstrap,
        proxy: EventLoopProxy<UserEvent>,
        options: ShellOptions,
        statistics_recorder: RecorderHandle,
        background_factory: BackgroundDocumentFactory,
        background_rasterizer: BackgroundRasterizer,
    ) -> Result<Self, Box<dyn Error>> {
        let ShellOptions {
            viewer: options,
            foreground_url,
            plugins,
            game_launch,
        } = options;
        if options.api != ViewerApi::D3d12 {
            return Err(
                "snowglass-shell-webview2 requires `--api d3d12` for DirectComposition".into(),
            );
        }
        let pending_cli_launch = if game_launch.is_none() {
            cli::launch_from_viewer(&options).map_err(std::io::Error::other)?
        } else {
            None
        };

        let mut window = WindowManager::new(window, statistics_recorder, proxy.clone())?;
        let size = window.size();
        let render_window = window.surface(size.width.max(1), size.height.max(1));
        // Prepare Vello before building WebView2. Initializing its D3D12 state
        // after Chromium has started WARP faults inside d3d10warp.dll on this
        // system. The factory chooses the initial adapter, and the merged
        // compositor must use the same device so it can import Vello's shared
        // game-layer textures before a captured stream supplies an adapter.
        let game_host = GameHost::new(&render_window, background_rasterizer)?;
        let merged_content = game_host.composition_content()?;
        drop(render_window);
        window.set_merged_content(&merged_content)?;
        window.commit()?;
        let session = unique_session();
        // The foreground page opts into a composed game layer with
        // `<link rel="background">`. Its initialization bridge discovers and
        // fetches that document before announcing that the frontend is ready;
        // document discovery itself remains deferred to the page.
        let plugin_runtime = background_factory.plugin_runtime();
        let launch_proxy = proxy.clone();
        let launch =
            BackgroundLaunchCoordinator::new(
                pending_cli_launch,
                plugins,
                Arc::clone(&plugin_runtime),
                options.clone(),
                move |event| match event {
                    LaunchCoordinatorEvent::GameDetected { generation, result } => {
                        let _ = launch_proxy.send_event(UserEvent::Runtime(
                            RuntimeCommand::GameDetected { generation, result },
                        ));
                    }
                },
            );
        let mut background_dom = BackgroundDomCoordinator::new(background_factory);
        let foreground_visual = window.foreground_visual()?;
        let foreground_proxy = proxy.clone();
        let navigation_proxy = proxy.clone();
        let bridge_script = include_str!(concat!(env!("OUT_DIR"), "/bridge.js"));
        let render_window = window.surface(size.width.max(1), size.height.max(1));
        let foreground_dom = ForegroundDom::new(
            &render_window,
            size.width,
            size.height,
            foreground_visual,
            bridge_script,
            &foreground_url,
            move |request| {
                if request.body().is_empty() {
                    let _ = foreground_proxy
                        .send_event(UserEvent::Runtime(RuntimeCommand::SharedBusDoorbell));
                } else {
                    tracing::error!("snowglass: WebView2 IPC carried a forbidden payload");
                }
            },
            move |_| {
                let _ = navigation_proxy
                    .send_event(UserEvent::Runtime(RuntimeCommand::ForegroundNavigation));
            },
        )?;
        // Assigning WebView2's RootVisualTarget mutates this DirectComposition
        // device after the tree's initial commit. Publish that mutation before
        // entering the event loop or the controller renders into a detached
        // visual while the game and overlay swapchains remain visible.
        window.commit()?;
        let background_proxy = proxy.clone();
        background_dom.attach_shared_bus(foreground_dom.background_bus_endpoint(move || {
            let _ =
                background_proxy.send_event(UserEvent::Runtime(RuntimeCommand::SharedBusDoorbell));
        }));

        let input = InputTransport::runtime().map_err(std::io::Error::other)?;
        let control_sender = input.control_sender();
        // Merging composites every stream into one opaque window-covering
        // swapchain. A game layer supplies the chrome that swapchain draws
        // around the streams, which is what lets it stay opaque and so stay
        // eligible for a hardware overlay plane.

        let mut state = Self {
            foreground_dom,
            background_dom,
            bus_broker: SharedBusBroker::default(),
            foreground_bus_attached: false,
            window,
            profile: TurnProfile::default(),
            services: ShellServices { session, proxy },
            launch,
            game_host,
            input: InputRouter::new(input, DirectInputForwarder::new(control_sender)),
        };
        if let Some(request) = game_launch {
            state.queue_game(request);
        }
        Ok(state)
    }

    fn queue_game(&mut self, request: GameLaunchRequest) {
        self.launch.queue_game(request);
        self.publish_launcher_state();
    }

    fn game_detected(&mut self, generation: u64, result: Result<launcher::DetectedGame, String>) {
        self.launch.game_detected(generation, result);
        self.publish_launcher_state();
    }

    fn start_prepared_game(&mut self, request: launcher::LauncherStartRequest) {
        self.launch
            .start_prepared_game(request, self.game_host.has_active_game());
        self.publish_launcher_state();
    }

    fn cancel_launcher(&mut self) {
        self.launch.cancel();
        self.publish_launcher_state();
    }

    fn send_foreground_command(&mut self, command: serde_json::Value) {
        if let Err(error) = self.bus_broker.queue_foreground_command(&command) {
            tracing::error!("snowglass: encoding foreground command failed: {error}");
            return;
        }
        self.flush_shared_bus_responses();
        if self.foreground_bus_attached
            && let Err(error) = self.foreground_dom.ring_bus_doorbell()
        {
            tracing::error!("snowglass: ringing the foreground command doorbell failed: {error}");
        }
    }

    fn publish_launcher_state(&mut self) {
        let Some(command) = crate::commands::launcher_state(self.launch.state()) else {
            return;
        };
        self.send_foreground_command(command);
    }

    async fn deliver_cli_launch(&mut self) {
        let Some(result) = self.launch.resolve_pending_launch().await else {
            return;
        };
        let options = match result {
            Ok(options) => options,
            Err(error) => {
                tracing::error!("snowglass: emulator plugin launch failed: {error}");
                self.launch.set_error(error);
                self.publish_launcher_state();
                return;
            }
        };
        let Some(message) = crate::commands::cli_launch(&options) else {
            return;
        };
        if !self.background_dom.post_message(&message) {
            let message = "CLI launch requires a linked game layer".to_owned();
            tracing::error!("snowglass: {message}");
            self.launch.set_error(message);
            self.publish_launcher_state();
            return;
        }
        self.refresh_after_background_script();
    }

    fn refresh_after_background_script(&mut self) {
        if self.background_dom.needs_paint() {
            self.refresh_background();
        }
    }

    fn apply_emulator_command(&mut self, command: EmulatorCommand) {
        match command {
            EmulatorCommand::Launch { token, options } => self.launch_game(token, options),
            EmulatorCommand::Close(token) => self.close_game(token),
            EmulatorCommand::SetVolume { token, volume } => self.set_game_volume(token, volume),
            EmulatorCommand::SetMuted { token, muted } => self.set_game_muted(token, muted),
            EmulatorCommand::Message { token, name, args } => self.game_message(token, &name, args),
            EmulatorCommand::Input { token, active } if self.is_active(token) => {
                self.set_input_priority(if active {
                    InputPriority::Game
                } else {
                    InputPriority::Foreground
                });
            }
            EmulatorCommand::Input { .. } => {}
            EmulatorCommand::ConfigureInput {
                request_id,
                transaction,
            } => self.configure_input(u64::from(request_id), transaction, true),
        }
    }

    fn is_active(&self, token: InstanceToken) -> bool {
        self.game_host.is_active(token)
    }

    fn configure_input(
        &mut self,
        request_id: u64,
        transaction: InputTransaction,
        background: bool,
    ) {
        match self
            .input
            .apply_configuration_transaction(transaction, self.game_host.has_active_game())
        {
            Ok(configuration) => {
                self.publish_input_configuration();
                self.send_input_configuration_result(request_id, Ok(configuration), background);
            }
            Err(error) => {
                self.send_input_configuration_result(
                    request_id,
                    Err(error.to_string()),
                    background,
                );
            }
        }
    }

    fn publish_input_configuration(&mut self) {
        let publication = self.input.configuration_publication();
        self.background_dom
            .set_input_configuration(&publication.configuration, &publication.layouts);
        self.send_foreground_command(publication.command);
    }

    fn send_input_configuration_result(
        &mut self,
        request_id: u64,
        result: Result<snowglass_input::hub::InputConfiguration, String>,
        background: bool,
    ) {
        let (result, command) = InputRouter::configuration_result(request_id, result);
        if background {
            self.background_dom
                .input_configuration_result(request_id, &result);
            return;
        }
        self.send_foreground_command(command);
    }

    async fn frontend_ready(
        &mut self,
        background: Option<BackgroundDocumentSource>,
        background_error: Option<String>,
    ) {
        if let Some(error) = background_error {
            tracing::error!("snowglass: loading the linked game layer failed: {error}");
        }
        if let Some(background) = background {
            let size = self.window.size();
            let command_proxy = self.services.proxy.clone();
            let html = background.html.clone();
            let base_url = background.base_url.clone();
            let activated = self
                .background_dom
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
                    factory
                        .create(
                            &html,
                            base_url,
                            BACKGROUND_SURFACE_CAPACITY,
                            size.width,
                            size.height,
                            emulator_commands,
                        )
                        .await
                })
                .await;
            if !activated {
                tracing::error!(
                    "snowglass: ignoring a second game layer discovered at {}",
                    background.base_url
                );
                self.deliver_cli_launch().await;
                return;
            }
            self.refresh_background();
            self.send_foreground_command(crate::commands::game_layer_ready());
            tracing::error!(
                "snowglass: using game layer discovered at {}",
                background.base_url
            );
        } else {
            // A page without the link does not use composed chrome and should
            // not retain the renderer that was prepared solely for it.
            self.background_dom.disable();
        }
        self.publish_input_configuration();
        self.publish_launcher_state();
        self.deliver_cli_launch().await;
    }

    fn drain_shared_bus_requests(&mut self) -> Vec<SharedBusRequest> {
        let runtime = self.launch.plugin_runtime_handle();
        let requests = self
            .bus_broker
            .drain_requests(self.foreground_dom.shared_bus_mut(), &runtime);
        let _ = self.foreground_dom.ring_bus_doorbell();
        requests
    }

    fn flush_shared_bus_responses(&mut self) {
        self.bus_broker.flush(self.foreground_dom.shared_bus_mut());
    }

    fn handle_compositor_tick(&mut self) {
        self.window.clear_clock_tick();
        self.window.record_clock();
        self.refresh_pending_background();
        self.present_pending_surfaces(PRESENT_PERMIT_WAIT);
    }

    fn handle_foreground_navigation(&mut self) {
        self.launch.plugin_runtime_handle().cancel_all();
        self.bus_broker.reset();
        self.foreground_dom.next_bus_generation();
        self.foreground_bus_attached = false;
        self.input.clear_foreground_hit_regions();
        self.background_dom.reset();
        self.game_host.clear_background();
    }

    fn handle_shared_bus_doorbell(&mut self) {
        if !self.foreground_bus_attached {
            match self.foreground_dom.attach_shared_bus() {
                Ok(()) => self.foreground_bus_attached = true,
                Err(error) => {
                    tracing::error!(
                        "snowglass: attaching the foreground shared bus failed: {error}"
                    );
                    return;
                }
            }
        }
        self.flush_shared_bus_responses();
        for request in self.drain_shared_bus_requests() {
            match request {
                SharedBusRequest::Plugin(request) => {
                    let runtime = self.launch.plugin_runtime_handle();
                    let proxy = self.services.proxy.clone();
                    tokio::task::spawn_local(async move {
                        let result = runtime
                            .invoke_encoded_with_cancellation(
                                &request.plugin,
                                &request.path,
                                request.encoded_arguments,
                                request.cancellation,
                            )
                            .await;
                        let _ = proxy.send_event(UserEvent::Runtime(
                            RuntimeCommand::SharedBusSettlement {
                                generation: request.generation,
                                request_id: request.request_id,
                                response_lane: request.response_lane,
                                result,
                            },
                        ));
                    });
                }
                SharedBusRequest::ShellCommand(command) => {
                    match decode_foreground_command(command) {
                        Ok(event) => {
                            let _ = self.services.proxy.send_event(event);
                        }
                        Err(error) => tracing::error!("snowglass: {error}"),
                    }
                }
            }
        }
    }

    fn settle_shared_bus(
        &mut self,
        generation: u32,
        request_id: u64,
        response_lane: usize,
        result: Result<Vec<u8>, snowglass_pluginhost::PluginInvocationError>,
    ) {
        let foreground = self.bus_broker.settle_plugin(
            self.foreground_dom.shared_bus(),
            generation,
            request_id,
            response_lane,
            result,
        );
        self.flush_shared_bus_responses();
        if foreground && let Err(error) = self.foreground_dom.ring_bus_doorbell() {
            tracing::error!(
                "snowglass: ringing the foreground shared-bus doorbell failed: {error}"
            );
        }
    }

    fn launch_game(&mut self, token: InstanceToken, options: AttachedLaunchPlan) {
        if self.game_host.has_active_game() {
            self.emit_instance_event(
                token,
                "error",
                &instance_events::error("another emulator instance is active"),
            );
            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            return;
        }
        let invalid_background_attachment = self.background_dom.invalid_attachment(&options);
        if let Some(index) = invalid_background_attachment {
            self.emit_instance_event(
                token,
                "error",
                &instance_events::error(format!(
                    "surface {index} does not attach to its background tracked-surface"
                )),
            );
            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            return;
        }
        self.emit_instance_event(token, "start", &serde_json::Value::Null);
        if let Err(error) = self
            .input
            .configure_launch(&options.machine, &options.gamepad_mapping)
        {
            self.emit_instance_event(token, "error", &instance_events::error(error));
            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            return;
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
            }
            Err(error) => {
                self.emit_instance_event(token, "error", &instance_events::error(error));
                self.emit_instance_event(token, "closed", &serde_json::Value::Null);
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
        self.set_input_priority(InputPriority::Foreground);
        self.input.clear_foreground_hit_regions();
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
        let background_changed = self.background_dom.dispatch_emulator_event(&event);
        if background_changed {
            self.refresh_background();
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
                    self.background_dom.invalidate();
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
                    self.refresh_background();
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
        let size = self.window.size();
        match self.game_host.draw(size.width, size.height, permit_wait) {
            Ok(Some(carried_new_frame)) => self.window.record_present(carried_new_frame),
            Ok(None) => {}
            Err(error) => tracing::error!("snowglass: rendering merged surfaces failed: {error}"),
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

    /// Re-lays out the game layer and moves the compositor to match.
    ///
    /// The chrome is uploaded band by band and the game views are placed
    /// between them, so a document whose bezel sits over a fullscreen view
    /// composites in the order the document painted it.
    ///
    /// Before a stream connects, chrome can be shown in temporary
    /// DirectComposition surfaces. Once the opaque merged swapchain exists,
    /// chrome goes inside it or it would be hidden.
    fn refresh_background(&mut self) {
        let entered = self.profile.enter();
        self.refresh_background_inner();
        self.profile.record(TurnPhase::Background, entered);
    }

    fn refresh_background_inner(&mut self) {
        self.game_host.clear_background_refresh_request();
        let Some(frame) = self.background_dom.produce_frame() else {
            return;
        };
        if let Err(error) = self.game_host.rasterize_background(&frame) {
            tracing::error!("snowglass: rasterizing background chrome failed: {error}");
            return;
        }
        let placements = self.background_dom.tracked_surface_layout();
        self.game_host.apply_tracked_layout(&placements);
    }

    /// Dispatches a regular DOM mouse event to the Deno-backed game layer.
    /// Script claims the pointer from the guest by cancelling the event.
    fn background_mouse(
        &mut self,
        kind: &str,
        x: f32,
        y: f32,
        button: u16,
        pressed: Option<bool>,
    ) -> bool {
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
        let Some(hit) = self.background_dom.hit_test(x, y) else {
            self.input.restore_mouse_buttons(previous_buttons);
            return false;
        };
        let prevented = self.background_dom.mouse_event(
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
        let needs_paint = self.background_dom.needs_paint();
        if !prevented {
            self.input.restore_mouse_buttons(previous_buttons);
        }
        if needs_paint {
            if self.window.is_pacing() {
                self.game_host.request_background_refresh();
            } else {
                self.refresh_background();
            }
        }
        self.refresh_after_background_script();
        prevented
    }

    fn background_keyboard(&mut self, kind: &str, key: String, code: String, repeat: bool) -> bool {
        let prevented = self.background_dom.keyboard_event(DomKeyboardEvent {
            kind,
            key,
            code,
            repeat,
            alt_key: self.input.dom_alt_key(),
            ctrl_key: self.input.dom_control_key(),
            shift_key: self.input.dom_shift_key(),
        });
        if self.background_dom.needs_paint() {
            self.refresh_background();
        }
        self.refresh_after_background_script();
        prevented
    }

    fn refresh_pending_background(&mut self) {
        if self.game_host.take_background_refresh_request() {
            self.refresh_background();
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
            InputPriority::Foreground
        } else {
            priority
        };
        if self.input.priority() == priority {
            // A foreground pointer interaction moves focus into WebView2
            // without changing the logical priority. Re-activating game input
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
        match priority {
            InputPriority::Foreground => {
                let _ = self.foreground_dom.focus();
            }
            InputPriority::Game => self.focus_game_input(),
        }
        self.send_foreground_command(crate::input::priority_command(priority));
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

    fn route_foreground_mouse(&mut self, kind: DomPointerKind) {
        if matches!(kind, DomPointerKind::Button { pressed: true, .. }) {
            let _ = self.foreground_dom.focus();
        }
        let event = self.input.foreground_pointer_event(kind);
        let _ = self.foreground_dom.send_pointer_event(event);
    }

    fn handle_dropped_file(&mut self, path: std::path::PathBuf) {
        if self.game_host.has_active_game() {
            self.launch
                .set_error("Close the active game before launching another one.");
            self.publish_launcher_state();
        } else {
            self.queue_game(GameLaunchRequest {
                game: path,
                emulator: None,
            });
        }
    }

    fn synchronize_foreground(&mut self) {
        let size = self.window.size();
        if let Err(error) = self.foreground_dom.synchronize(size.width, size.height) {
            tracing::error!("snowglass: synchronizing foreground WebView2 failed: {error}");
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        if let Err(error) = self.foreground_dom.synchronize(width, height) {
            tracing::error!("snowglass: resizing foreground WebView2 failed: {error}");
        }
        if let Err(error) = self.window.resize_overlay() {
            tracing::error!("snowglass: resizing egui composition overlay failed: {error}");
        }
        if self.background_dom.resize(width, height) {
            self.refresh_background();
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
            self.background_mouse(
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
        let background_hit = self.background_dom.hit_test(x as f32, y as f32).is_some();
        match self.input.pointer_target(consumed, true, background_hit) {
            PointerTarget::Overlay => return,
            PointerTarget::Foreground => {
                self.route_foreground_mouse(DomPointerKind::Move);
                return;
            }
            PointerTarget::Background => {
                self.route_foreground_mouse(DomPointerKind::Move);
                if self.background_mouse("mousemove", x as f32, y as f32, 0, None) {
                    return;
                }
            }
            PointerTarget::Game => self.route_foreground_mouse(DomPointerKind::Move),
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
        let foreground_hit = self.input.foreground_region_claims(consumed, true);
        let cursor = self.input.cursor();
        let background_hit = self
            .background_dom
            .hit_test(cursor.x as f32, cursor.y as f32)
            .is_some();
        match self.input.pointer_target(consumed, true, background_hit) {
            PointerTarget::Overlay => return,
            PointerTarget::Foreground => {
                if pressed && foreground_hit {
                    self.input.set_foreground_mouse_capture(true);
                }
                self.route_foreground_mouse(DomPointerKind::Button { button, pressed });
                if !pressed && !self.input.foreground_buttons_active() {
                    self.input.set_foreground_mouse_capture(false);
                }
                return;
            }
            PointerTarget::Background => {
                let kind = if pressed { "mousedown" } else { "mouseup" };
                if self.background_mouse(
                    kind,
                    cursor.x as f32,
                    cursor.y as f32,
                    dom_button,
                    Some(pressed),
                ) {
                    return;
                }
            }
            PointerTarget::Game => {}
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
        match self.input.pointer_target(false, false, false) {
            PointerTarget::Foreground => {
                if horizontal != 0 {
                    self.route_foreground_mouse(DomPointerKind::Wheel {
                        horizontal: true,
                        delta: horizontal,
                    });
                }
                if vertical != 0 {
                    self.route_foreground_mouse(DomPointerKind::Wheel {
                        horizontal: false,
                        delta: vertical,
                    });
                }
            }
            PointerTarget::Overlay => {}
            PointerTarget::Background | PointerTarget::Game => {
                self.route_pointer(PointerPhase::Wheel, 0, horizontal, vertical);
            }
        }
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
            self.set_input_priority(InputPriority::Foreground);
        }
        let publication = self.input.build_publication(
            self.game_host.gamepad_mapping(),
            self.game_host.has_active_game(),
        );
        self.window.set_input_diagnostics(publication.diagnostics);
        self.background_dom
            .set_input_state(&publication.controllers, publication.priority);
        let Some(state_changed) = publication.state_changed else {
            return;
        };
        if !state_changed && publication.routed.is_empty() {
            return;
        }
        self.window.request_overlay_repaint();
        self.send_foreground_command(crate::input::state_command(
            publication.controllers,
            publication.routed,
        ));
    }

    fn publish_keyboard_input(&mut self, key: &str, pressed: bool, repeat: bool) {
        self.send_foreground_command(crate::input::keyboard_command(key, pressed, repeat));
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
    background_factory: Option<BackgroundDocumentFactory>,
    background_rasterizer: Option<BackgroundRasterizer>,
    pending_frontend_ready: Option<(Option<BackgroundDocumentSource>, Option<String>)>,
    pending_window: Option<WindowBootstrap>,
    state: Option<ShellCoordinator>,
    error: Option<Box<dyn Error>>,
}

impl ShellApplication {
    pub(crate) fn new(
        proxy: EventLoopProxy<UserEvent>,
        options: ShellOptions,
        statistics_recorder: RecorderHandle,
        background_factory: BackgroundDocumentFactory,
        background_rasterizer: BackgroundRasterizer,
    ) -> Self {
        Self {
            proxy,
            options: Some(options),
            statistics_recorder: Some(statistics_recorder),
            background_factory: Some(background_factory),
            background_rasterizer: Some(background_rasterizer),
            pending_frontend_ready: None,
            pending_window: None,
            state: None,
            error: None,
        }
    }

    pub(crate) async fn run_async_tasks(&mut self) {
        if self.state.is_none()
            && self.error.is_none()
            && let Some(window) = self.pending_window.take()
        {
            let options = self
                .options
                .take()
                .expect("shell options are consumed exactly once");
            let statistics_recorder = self
                .statistics_recorder
                .take()
                .expect("statistics recorder is consumed exactly once");
            let background_factory = self
                .background_factory
                .take()
                .expect("the game-layer renderer is consumed exactly once");
            let background_rasterizer = self
                .background_rasterizer
                .take()
                .expect("the background rasterizer is consumed exactly once");
            match ShellCoordinator::new(
                window,
                self.proxy.clone(),
                options,
                statistics_recorder,
                background_factory,
                background_rasterizer,
            ) {
                Ok(state) => self.state = Some(state),
                Err(error) => self.error = Some(error),
            }
        }
        if let Some((background, background_error)) = self.pending_frontend_ready.take()
            && let Some(state) = self.state.as_mut()
        {
            state.frontend_ready(background, background_error).await;
        }
        if let Some(state) = self.state.as_mut()
            && state.background_dom.poll_runtime().await
        {
            // Timers, promises, fetches, and WebSockets can mutate the
            // background DOM without a Winit event. Polling applies those
            // mutations and marks the layer dirty; repaint it here so async
            // UI changes (including the launch fade) reach the compositor.
            if state.background_dom.needs_paint() {
                state.refresh_background();
            }
            state.refresh_after_background_script();
        }
        if let Some(state) = self.state.as_mut()
            && state.launch.has_pending_launch()
            && state.background_dom.is_active()
        {
            state.deliver_cli_launch().await;
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
            UserEvent::Runtime(RuntimeCommand::FrontendReady {
                background,
                background_error,
            }) => {
                self.pending_frontend_ready = Some((background, background_error));
            }
            UserEvent::Runtime(RuntimeCommand::ForegroundNavigation) => {
                state.handle_foreground_navigation()
            }
            UserEvent::Input(InputCommand::ForegroundHitRegions(regions)) => {
                state.input.replace_foreground_hit_regions(regions);
            }
            UserEvent::Runtime(RuntimeCommand::SharedBusDoorbell) => {
                state.handle_shared_bus_doorbell()
            }
            UserEvent::Runtime(RuntimeCommand::SharedBusSettlement {
                generation,
                request_id,
                response_lane,
                result,
            }) => state.settle_shared_bus(generation, request_id, response_lane, result),
            UserEvent::Input(InputCommand::Configure {
                request_id,
                transaction,
            }) => state.configure_input(request_id, transaction, false),
            UserEvent::GameHost(GameHostCommand::Emulator(command)) => {
                state.apply_emulator_command(command)
            }
            UserEvent::Runtime(RuntimeCommand::LauncherOpen(request)) => state.queue_game(request),
            UserEvent::Runtime(RuntimeCommand::LauncherStart(request)) => {
                state.start_prepared_game(request)
            }
            UserEvent::Runtime(RuntimeCommand::LauncherCancel) => state.cancel_launcher(),
            UserEvent::Runtime(RuntimeCommand::GameDetected { generation, result }) => {
                state.game_detected(generation, result)
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
        let mut background_prevented = false;
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
            background_prevented = state.background_keyboard(
                if *pressed { "keydown" } else { "keyup" },
                logical_key.clone(),
                code.clone(),
                *repeat,
            );
            consumed = background_prevented;
        }
        match event {
            ShellWindowEvent::CloseRequested => return true,
            ShellWindowEvent::DroppedFile(path) => state.handle_dropped_file(path),
            ShellWindowEvent::Moved => state.synchronize_foreground(),
            ShellWindowEvent::Resized { width, height } if width != 0 && height != 0 => {
                state.resize(width, height);
            }
            ShellWindowEvent::FocusLost => state.handle_focus_lost(),
            ShellWindowEvent::Keyboard {
                physical_key,
                pressed,
                repeat,
                ..
            } if state
                .input
                .keyboard_target(overlay_consumed, background_prevented)
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
            if state.background_dom.is_active() {
                // Deno owns browser timers, fetch, WebSockets, promises, and
                // inspector traffic. Return to the Tokio loop regularly so it
                // can advance them even when the window is otherwise idle.
                deadline = deadline.min(now + Duration::from_millis(16));
            }
            if state.window.overlay_repaint_due(now) {
                state.window.request_redraw();
            }
            deadline = deadline.min(state.window.next_overlay_repaint());
            // Cheap enough to reconcile every turn, which keeps pacing correct
            // no matter which path added or dropped a stream.
            state
                .window
                .set_clock_active(state.game_host.has_connected_surfaces());
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
