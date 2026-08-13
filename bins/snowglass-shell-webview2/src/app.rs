use crate::background_document::{BackgroundDocumentFactory, DomKeyboardEvent, DomMouseEvent};
use crate::background_dom::BackgroundDomCoordinator;
use crate::background_runtime::{
    BackgroundLaunchCoordinator, EmulatorCommand, EmulatorCommandSink, InstanceToken,
    LaunchCoordinatorEvent,
};
use crate::cli::{GameLaunchRequest, ShellOptions};
use crate::foreground_dom::ForegroundDom;
use crate::game_host::{BackgroundRasterizer, GameHost, GameHostCaptureEvent, GameHostCommand};
use crate::input::{
    DomPointerKind, InputCommand, InputPriority, InputRouter, InputTransport, KeyboardTarget,
    PointerTarget,
};
use crate::launch::AttachedLaunchPlan;
use crate::shared_bus::{SharedBusBroker, SharedBusRequest};
use crate::window_manager::{
    EventLoopProxy, LoopDirective, ShellDeviceEvent, ShellWindowEvent, WindowBootstrap,
    WindowCommand, WindowManager,
};
use crate::{cli, launcher};
use snowglass_egui::{InputAxisDiagnostics, InputDiagnostics, InputSlotDiagnostics};
use snowglass_input::{
    hub::InputTransaction, layout::SdlButton, mouse::PointerPhase, provider::sdl3::Sdl3Provider,
};
use snowglass_input_dinput::DirectInputForwarder;
use snowglass_renderer::{CompositingPlan, Extent, cli::ViewerApi};
use snowglass_statistics::RecorderHandle;
use std::{
    collections::HashMap,
    error::Error,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub(crate) enum UserEvent {
    Window(WindowCommand),
    Runtime(RuntimeCommand),
    GameHost(GameHostCommand),
    Input(InputCommand),
}

#[derive(Debug)]
pub(crate) enum RuntimeCommand {
    FrontendReady {
        background: Option<BackgroundDocumentSource>,
        background_error: Option<String>,
    },
    ForegroundNavigation,
    SharedBusDoorbell,
    SharedBusSettlement {
        generation: u32,
        request_id: u64,
        response_lane: usize,
        result: Result<Vec<u8>, snowglass_pluginhost::PluginInvocationError>,
    },
    LauncherOpen(GameLaunchRequest),
    LauncherStart(launcher::LauncherStartRequest),
    LauncherCancel,
    GameDetected {
        generation: u64,
        result: Result<launcher::DetectedGame, String>,
    },
}

#[derive(Debug)]
pub(crate) struct BackgroundDocumentSource {
    html: String,
    base_url: String,
}

fn decode_foreground_command(message: serde_json::Value) -> Result<UserEvent, String> {
    let kind = message
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "foreground shell command is missing its type".to_owned())?;
    match kind {
        "frontendReady" => {
            let background = message.get("background").and_then(|background| {
                Some(BackgroundDocumentSource {
                    html: background.get("html")?.as_str()?.to_owned(),
                    base_url: background.get("url")?.as_str()?.to_owned(),
                })
            });
            let background_error = message
                .get("backgroundError")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            Ok(UserEvent::Runtime(RuntimeCommand::FrontendReady {
                background,
                background_error,
            }))
        }
        "foregroundHitRegions" => {
            let regions = message
                .get("regions")
                .cloned()
                .ok_or_else(|| "foreground hit-region command is missing regions".to_owned())?;
            serde_json::from_value(regions)
                .map(|regions| UserEvent::Input(InputCommand::ForegroundHitRegions(regions)))
                .map_err(|error| format!("invalid WebView2 foreground hit regions: {error}"))
        }
        "inputConfigure" => {
            let request_id = message
                .get("id")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| "input configuration command has an invalid ID".to_owned())?;
            let transaction = message.get("transaction").cloned().ok_or_else(|| {
                "input configuration command is missing its transaction".to_owned()
            })?;
            serde_json::from_value(transaction)
                .map(|transaction| {
                    UserEvent::Input(InputCommand::Configure {
                        request_id,
                        transaction,
                    })
                })
                .map_err(|error| format!("invalid input configuration transaction: {error}"))
        }
        "launcherOpen" => {
            let game = message
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "launcher-open command is missing its path".to_owned())?
                .into();
            let emulator = message
                .get("emulator")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            Ok(UserEvent::Runtime(RuntimeCommand::LauncherOpen(
                GameLaunchRequest { game, emulator },
            )))
        }
        "launcherStart" => {
            let request = message
                .get("request")
                .cloned()
                .ok_or_else(|| "launcher-start command is missing its request".to_owned())?;
            serde_json::from_value(request)
                .map(|request| UserEvent::Runtime(RuntimeCommand::LauncherStart(request)))
                .map_err(|error| format!("invalid frontend launch request: {error}"))
        }
        "launcherCancel" => Ok(UserEvent::Runtime(RuntimeCommand::LauncherCancel)),
        other => Err(format!("unsupported foreground shell command '{other}'")),
    }
}

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

/// Where an event-loop turn's wall time goes.
///
/// The shell runs WebView2, Vello, capture intake, and every present on one
/// thread. A turn that runs long does not surface as an obviously slow
/// function: it surfaces as coalesced compositor ticks and a present rate below
/// the display's, which reads identically to a pacing bug. Attributing the time
/// is what separates the two.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TurnPhase {
    Capture,
    Present,
    Background,
    Input,
    Overlay,
}

impl TurnPhase {
    const ALL: [Self; 5] = [
        Self::Capture,
        Self::Present,
        Self::Background,
        Self::Input,
        Self::Overlay,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Present => "present",
            Self::Background => "background",
            Self::Input => "input",
            Self::Overlay => "overlay",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// An open phase measurement, holding the enclosing phase's nested total so it
/// can be restored when this one closes.
#[derive(Clone, Copy, Debug)]
struct PhaseEntry {
    started: Instant,
    outer_nested: Duration,
}

#[derive(Debug, Default)]
struct TurnProfile {
    spans: [Duration; TurnPhase::ALL.len()],
    /// Time already attributed to phases nested inside the one currently open.
    nested: Duration,
    turns: u64,
    started: Option<Instant>,
    report: String,
}

impl TurnProfile {
    /// Opens a phase. Phases nest — draining capture presents, and presenting
    /// repaints chrome — so each reports the time it spent on its own work
    /// rather than the time its callees spent. Otherwise the enclosing phase
    /// always looks like the expensive one and names the wrong thing to cut.
    fn enter(&mut self) -> PhaseEntry {
        PhaseEntry {
            started: Instant::now(),
            outer_nested: std::mem::take(&mut self.nested),
        }
    }

    fn record(&mut self, phase: TurnPhase, entry: PhaseEntry) {
        let total = entry.started.elapsed();
        self.spans[phase.index()] += total.saturating_sub(self.nested);
        self.nested = entry.outer_nested + total;
    }

    /// Closes a turn, refreshing the report once a sampling window elapses.
    ///
    /// Costs are reported as milliseconds spent per second of wall clock, so a
    /// phase reading 400 is eating 40% of the loop and a turn rate below the
    /// refresh rate cannot be fixed by scheduling presents differently.
    fn finish_turn(&mut self, now: Instant) {
        self.turns += 1;
        self.nested = Duration::ZERO;
        let started = *self.started.get_or_insert(now);
        let elapsed = now.saturating_duration_since(started);
        if elapsed < snowglass_statistics::SAMPLE_INTERVAL {
            return;
        }
        let seconds = elapsed.as_secs_f64();
        let mut phases = TurnPhase::ALL;
        // Costliest first: the point of the line is to name what to cut.
        phases.sort_by(|left, right| self.spans[right.index()].cmp(&self.spans[left.index()]));
        let costs = phases
            .iter()
            .map(|phase| {
                format!(
                    "{} {:.0}",
                    phase.label(),
                    self.spans[phase.index()].as_secs_f64() * 1000.0 / seconds
                )
            })
            .collect::<Vec<_>>()
            .join(" · ");
        self.report = format!(
            "Loop {:.1} turns/s · {costs} ms/s",
            self.turns as f64 / seconds
        );
        self.spans = <[Duration; TurnPhase::ALL.len()]>::default();
        self.turns = 0;
        self.started = Some(now);
    }
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct WebRenderTarget {
    connection: String,
    index: u32,
    width: u32,
    height: u32,
}

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
        let Ok(state) = serde_json::to_value(self.launch.state()) else {
            return;
        };
        self.send_foreground_command(serde_json::json!({
            "type": "launcherState",
            "state": state,
        }));
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
        let Ok(options) = serde_json::to_value(&options) else {
            return;
        };
        if !self.background_dom.post_message(&serde_json::json!({
            "type": "snowglass-cli-launch",
            "options": options,
        })) {
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

    fn input_provider(&self) -> Option<&Sdl3Provider> {
        self.input.provider()
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
        let configuration = self.input.configuration_snapshot();
        let layouts = self.input.layouts();
        let configuration_value =
            serde_json::to_value(&configuration).unwrap_or(serde_json::Value::Null);
        let layouts_value = serde_json::to_value(&layouts).unwrap_or(serde_json::Value::Null);
        self.background_dom
            .set_input_configuration(&configuration_value, &layouts_value);
        self.send_foreground_command(serde_json::json!({
            "type": "inputConfiguration",
            "configuration": configuration_value,
            "layouts": layouts_value,
        }));
    }

    fn send_input_configuration_result(
        &mut self,
        request_id: u64,
        result: Result<snowglass_input::hub::InputConfiguration, String>,
        background: bool,
    ) {
        let result = match result {
            Ok(configuration) => serde_json::json!({
                "ok": true,
                "configuration": configuration,
            }),
            Err(error) => serde_json::json!({
                "ok": false,
                "error": error,
            }),
        };
        if background {
            self.background_dom
                .input_configuration_result(request_id, &result);
            return;
        }
        self.send_foreground_command(serde_json::json!({
            "type": "inputConfigurationResult",
            "id": request_id,
            "result": result,
        }));
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
            self.send_foreground_command(serde_json::json!({ "type": "gameLayerReady" }));
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

    fn launch_game(&mut self, token: InstanceToken, options: AttachedLaunchPlan) {
        if self.game_host.has_active_game() {
            self.emit_instance_event(
                token,
                "error",
                &serde_json::json!({ "message": "another emulator instance is active" }),
            );
            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            return;
        }
        let invalid_background_attachment = self.background_dom.invalid_attachment(&options);
        if let Some(index) = invalid_background_attachment {
            self.emit_instance_event(
                token,
                "error",
                &serde_json::json!({
                    "message": format!(
                        "surface {index} does not attach to its background tracked-surface"
                    )
                }),
            );
            self.emit_instance_event(token, "closed", &serde_json::Value::Null);
            return;
        }
        self.emit_instance_event(token, "start", &serde_json::Value::Null);
        if let Err(error) = self
            .input
            .configure_launch(&options.machine, &options.gamepad_mapping)
        {
            self.emit_instance_event(token, "error", &serde_json::json!({ "message": error }));
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
                self.emit_instance_event(
                    token,
                    "error",
                    &serde_json::json!({ "message": error.to_string() }),
                );
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
            self.emit_instance_event(
                token,
                "message",
                &serde_json::json!({ "name": name, "args": args }),
            );
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
        self.emit_instance_event(
            token,
            "audioChanged",
            &serde_json::json!({
                "available": audio.available,
                "volume": audio.volume.get(),
                "muted": audio.muted,
            }),
        );
    }

    fn emit_instance_event(
        &mut self,
        token: InstanceToken,
        event_name: &str,
        payload: &serde_json::Value,
    ) {
        let event = serde_json::json!({
            "instanceId": token,
            "eventName": event_name,
            "payload": payload,
        });
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
        self.present_pending_surfaces_inner(permit_wait);
        self.profile.record(TurnPhase::Present, entered);
    }

    fn present_pending_surfaces_inner(&mut self, permit_wait: Duration) {
        self.draw_merged(permit_wait);
    }

    fn has_pending_surfaces(&self) -> bool {
        // Re-presenting an unchanged shared buffer burns GPU time and can
        // occupy the one-frame DXGI queue when a real guest frame arrives.
        // DirectComposition retains the last buffer, so only new content or
        // chrome needs another present.
        self.game_host.has_pending_frame()
    }

    /// Draws every stream and the game layer's chrome into the shared
    /// swapchain when either has changed.
    ///
    /// Every stream keeps its newest frame independently. A draw samples all of
    /// those newest frames, so one stream delivering does not make the others
    /// wait for another producer frame, while an unchanged composition remains
    /// resident in DirectComposition without redundant presents.
    fn draw_merged(&mut self, permit_wait: Duration) {
        let size = self.window.size();
        match self.game_host.draw(size.width, size.height, permit_wait) {
            Ok(Some(carried_new_frame)) => self.window.record_present(carried_new_frame),
            Ok(None) => {}
            Err(error) => tracing::error!("snowglass: rendering merged surfaces failed: {error}"),
        }
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
        self.send_foreground_command(serde_json::json!({
            "type": "inputPriority",
            "priority": match priority {
                InputPriority::Game => "game",
                InputPriority::Foreground => "foreground",
            },
        }));
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

    fn publish_input_state(&mut self) {
        if !self.input.publication_due(Instant::now()) {
            return;
        }
        let states = self
            .input_provider()
            .map(Sdl3Provider::states)
            .unwrap_or_default();
        let physical_devices = self.input.devices();
        let guide_pressed = physical_devices
            .iter()
            .any(|device| device.buttons.contains(&(SdlButton::Guide, true)));
        if self.input.observe_guide_button(guide_pressed)
            && self.input.priority() == InputPriority::Game
        {
            self.set_input_priority(InputPriority::Foreground);
        }
        self.window.set_input_diagnostics(InputDiagnostics {
            active: self.input.priority() == InputPriority::Game
                && self.game_host.has_active_game(),
            slots: self
                .game_host
                .gamepad_mapping()
                .iter()
                .map(|config| {
                    let Some(state) = states.get(config.slot).and_then(Option::as_ref) else {
                        return InputSlotDiagnostics {
                            slot: config.slot,
                            layout: config.layout.clone(),
                            ..InputSlotDiagnostics::default()
                        };
                    };
                    InputSlotDiagnostics {
                        slot: config.slot,
                        layout: state.layout_id.clone(),
                        connected: true,
                        name: state.name.clone(),
                        guid: state.guid.clone(),
                        pressed: state
                            .buttons
                            .iter()
                            .filter(|(_, pressed)| *pressed)
                            .map(|(button, _)| format!("{button:?}"))
                            .collect(),
                        axes: state
                            .axes
                            .iter()
                            .map(|(axis, value)| InputAxisDiagnostics {
                                name: format!("{axis:?}"),
                                value: *value,
                            })
                            .collect(),
                        mapped_buttons: state.mapped.buttons,
                        left_stick: state.mapped.left_stick,
                        right_stick: state.mapped.right_stick,
                        left_trigger: state.mapped.left_trigger,
                        right_trigger: state.mapped.right_trigger,
                        pointer: state.mapped.pointer,
                        accelerometer: state.mapped.accelerometer,
                        gyroscope: state.mapped.gyroscope,
                    }
                })
                .collect(),
        });
        let mut devices = Vec::new();
        let mut next_controls = HashMap::new();
        let foreground_priority = self.input.priority() == InputPriority::Foreground;
        for device in physical_devices {
            let assigned = states
                .iter()
                .enumerate()
                .filter_map(|(slot, state)| {
                    state
                        .as_ref()
                        .is_some_and(|state| state.device_id == device.device_id)
                        .then_some(slot)
                })
                .collect::<Vec<_>>();
            let mapped_states = assigned
                .iter()
                .filter_map(|&slot| states.get(slot).and_then(Option::as_ref))
                .collect::<Vec<_>>();
            let primary = mapped_states.first().copied();
            let buttons = device
                .buttons
                .iter()
                .map(|&(button, pressed)| {
                    let name = format!("{button:?}");
                    let global = button == SdlButton::Guide;
                    let consumed = global
                        || mapped_states
                            .iter()
                            .any(|state| state.mapped_buttons.contains(&button));
                    if foreground_priority || !consumed {
                        if !global {
                            next_controls.insert(
                                format!("{}:button:{name}", device.device_id),
                                i32::from(pressed),
                            );
                        }
                    }
                    serde_json::json!({
                        "button": name,
                        "pressed": pressed,
                        "mapped": consumed,
                    })
                })
                .collect::<Vec<_>>();
            let axes = device
                .axes
                .iter()
                .map(|&(axis, value)| {
                    let name = format!("{axis:?}");
                    let consumed = mapped_states
                        .iter()
                        .any(|state| state.mapped_axes.contains(&axis));
                    if foreground_priority || !consumed {
                        next_controls.insert(
                            format!("{}:axis:{name}", device.device_id),
                            i32::from(value),
                        );
                    }
                    serde_json::json!({
                        "axis": name,
                        "value": value,
                        "mapped": consumed,
                    })
                })
                .collect::<Vec<_>>();
            devices.push(serde_json::json!({
                "index": device.enumeration_index,
                "slot": device.enumeration_index,
                "deviceId": device.device_id,
                "name": device.name,
                "guid": device.guid,
                "vendor": device.vendor,
                "product": device.product,
                "assignedPorts": assigned,
                "layout": primary.map(|state| state.layout_id.as_str()),
                "buttons": buttons,
                "axes": axes,
                "mapped": {
                    "buttons": primary.map_or(0, |state| state.mapped.buttons),
                    "leftStick": primary.map_or([0; 2], |state| state.mapped.left_stick),
                    "rightStick": primary.map_or([0; 2], |state| state.mapped.right_stick),
                    "leftTrigger": primary.map_or(0, |state| state.mapped.left_trigger),
                    "rightTrigger": primary.map_or(0, |state| state.mapped.right_trigger),
                    "pointer": primary.map_or([0; 2], |state| state.mapped.pointer),
                    "accelerometer": primary.map_or([0.0; 3], |state| state.mapped.accelerometer),
                    "gyroscope": primary.map_or([0.0; 3], |state| state.mapped.gyroscope),
                }
            }));
        }
        for (slot, state) in states.iter().enumerate().filter_map(|(slot, state)| {
            state
                .as_ref()
                .filter(|state| state.device_id == "keyboard-mouse")
                .map(|state| (slot, state))
        }) {
            devices.push(serde_json::json!({
                "index": devices.len(),
                "slot": devices.len(),
                "deviceId": state.device_id,
                "name": state.name,
                "guid": state.guid,
                "vendor": state.vendor,
                "product": state.product,
                "assignedPorts": [slot],
                "layout": state.layout_id,
                "buttons": state.buttons.iter().map(|(button, pressed)| serde_json::json!({
                    "button": format!("{button:?}"),
                    "pressed": pressed,
                    "mapped": true,
                })).collect::<Vec<_>>(),
                "axes": state.axes.iter().map(|(axis, value)| serde_json::json!({
                    "axis": format!("{axis:?}"),
                    "value": value,
                    "mapped": true,
                })).collect::<Vec<_>>(),
                "mapped": {
                    "buttons": state.mapped.buttons,
                    "leftStick": state.mapped.left_stick,
                    "rightStick": state.mapped.right_stick,
                    "leftTrigger": state.mapped.left_trigger,
                    "rightTrigger": state.mapped.right_trigger,
                    "pointer": state.mapped.pointer,
                    "accelerometer": state.mapped.accelerometer,
                    "gyroscope": state.mapped.gyroscope,
                }
            }));
        }
        let routed = self
            .input
            .route_ui_controls(next_controls)
            .into_iter()
            .map(|(key, value)| control_event(&key, value))
            .collect::<Vec<_>>();
        let controllers = serde_json::Value::Array(devices.clone());
        self.background_dom.set_input_state(
            &controllers,
            if foreground_priority {
                "foreground"
            } else {
                "game"
            },
        );
        let Ok(json) = serde_json::to_string(&devices) else {
            return;
        };
        let state_changed = self.input.observe_published_state(&json);
        if !state_changed && routed.is_empty() {
            return;
        }
        self.window.request_overlay_repaint();
        self.send_foreground_command(serde_json::json!({
            "type": "inputState",
            "controllers": devices,
            "routed": routed,
        }));
    }

    fn publish_keyboard_input(&mut self, key: &str, pressed: bool, repeat: bool) {
        let event = serde_json::json!({
            "device": "keyboard",
            "controlType": "key",
            "control": key,
            "value": i32::from(pressed),
            "pressed": pressed,
            "repeat": repeat,
        });
        self.send_foreground_command(serde_json::json!({
            "type": "keyboardInput",
            "event": event,
        }));
    }

    fn publish_targets_for(&mut self, token: InstanceToken) {
        let mut targets = self
            .game_host
            .surface_targets()
            .into_iter()
            .map(|surface| WebRenderTarget {
                connection: surface.connection.to_string(),
                index: surface.index,
                width: surface.width,
                height: surface.height,
            })
            .collect::<Vec<_>>();
        targets.sort_by_key(|target| target.index);
        let Ok(payload) = serde_json::to_value(&targets) else {
            return;
        };
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
            UserEvent::Window(WindowCommand::CompositorTick) => {
                state.window.clear_clock_tick();
                state.window.record_clock();
                // Mouse input can run several times faster than the display.
                // Paint its latest position once for this scanout rather than
                // blocking on a Vello repaint for every intermediate event.
                state.refresh_pending_background();
                // Present here, in phase with DWM, rather than when a guest
                // frame happened to arrive. The chrome refreshed just above
                // rides the same present.
                state.present_pending_surfaces(PRESENT_PERMIT_WAIT);
            }
            UserEvent::Runtime(RuntimeCommand::FrontendReady {
                background,
                background_error,
            }) => {
                self.pending_frontend_ready = Some((background, background_error));
            }
            UserEvent::Runtime(RuntimeCommand::ForegroundNavigation) => {
                state.launch.plugin_runtime_handle().cancel_all();
                state.bus_broker.reset();
                state.foreground_dom.next_bus_generation();
                state.foreground_bus_attached = false;
                state.input.clear_foreground_hit_regions();
                state.background_dom.reset();
                state.game_host.clear_background();
            }
            UserEvent::Input(InputCommand::ForegroundHitRegions(regions)) => {
                state.input.replace_foreground_hit_regions(regions);
            }
            UserEvent::Runtime(RuntimeCommand::SharedBusDoorbell) => {
                if !state.foreground_bus_attached {
                    match state.foreground_dom.attach_shared_bus() {
                        Ok(()) => state.foreground_bus_attached = true,
                        Err(error) => {
                            tracing::error!(
                                "snowglass: attaching the foreground shared bus failed: {error}"
                            );
                            return;
                        }
                    }
                }
                state.flush_shared_bus_responses();
                for request in state.drain_shared_bus_requests() {
                    match request {
                        SharedBusRequest::Plugin(request) => {
                            let runtime = state.launch.plugin_runtime_handle();
                            let proxy = state.services.proxy.clone();
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
                                    let _ = state.services.proxy.send_event(event);
                                }
                                Err(error) => {
                                    tracing::error!("snowglass: {error}");
                                }
                            }
                        }
                    }
                }
            }
            UserEvent::Runtime(RuntimeCommand::SharedBusSettlement {
                generation,
                request_id,
                response_lane,
                result,
            }) => {
                let foreground = state.bus_broker.settle_plugin(
                    state.foreground_dom.shared_bus(),
                    generation,
                    request_id,
                    response_lane,
                    result,
                );
                state.flush_shared_bus_responses();
                if foreground && let Err(error) = state.foreground_dom.ring_bus_doorbell() {
                    tracing::error!(
                        "snowglass: ringing the foreground shared-bus doorbell failed: {error}"
                    );
                }
            }
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
            ShellWindowEvent::DroppedFile(path) => {
                if state.game_host.has_active_game() {
                    state
                        .launch
                        .set_error("Close the active game before launching another one.");
                    state.publish_launcher_state();
                } else {
                    state.queue_game(GameLaunchRequest {
                        game: path,
                        emulator: None,
                    });
                }
            }
            ShellWindowEvent::Moved => {
                let size = state.window.size();
                if let Err(error) = state.foreground_dom.synchronize(size.width, size.height) {
                    tracing::error!("snowglass: synchronizing foreground WebView2 failed: {error}");
                }
            }
            ShellWindowEvent::Resized { width, height } if width != 0 && height != 0 => {
                if let Err(error) = state.foreground_dom.synchronize(width, height) {
                    tracing::error!("snowglass: resizing foreground WebView2 failed: {error}");
                }
                if let Err(error) = state.window.resize_overlay() {
                    tracing::error!("snowglass: resizing egui composition overlay failed: {error}");
                }
                if state.background_dom.resize(width, height) {
                    state.refresh_background();
                }
                // The shared swapchain and the background both cover the whole
                // window, so the window is what resizes them, not a DOM box.
                let render_window = state.window.surface(width, height);
                if let Err(error) = state.game_host.resize_compositor(&render_window) {
                    tracing::error!("snowglass: resizing the shared swapchain failed: {error}");
                }
                state.window.request_redraw();
            }
            ShellWindowEvent::FocusLost => {
                if let Some(connection) = state.input.captured_connection() {
                    state.cancel_pointer_connection(connection);
                }
                let cursor = state.input.cursor();
                for (button, _) in state.input.focus_lost_buttons() {
                    state.background_mouse(
                        "mouseup",
                        cursor.x as f32,
                        cursor.y as f32,
                        button,
                        Some(false),
                    );
                }
                state.input.finish_focus_lost();
            }
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
                if let Some(key) = physical_key {
                    if !state.input.route_game_key(&key, pressed) {
                        state.publish_keyboard_input(&key, pressed, repeat);
                    }
                }
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
                state.input.set_cursor(x, y);
                let background_hit = state.background_dom.hit_test(x as f32, y as f32).is_some();
                match state.input.pointer_target(consumed, true, background_hit) {
                    PointerTarget::Overlay => return false,
                    PointerTarget::Foreground => {
                        state.route_foreground_mouse(DomPointerKind::Move);
                        return false;
                    }
                    PointerTarget::Background => {
                        // WebView2 still needs the move when the pointer leaves
                        // a foreground region so CSS hover state can clear.
                        state.route_foreground_mouse(DomPointerKind::Move);
                        if state.background_mouse("mousemove", x as f32, y as f32, 0, None) {
                            return false;
                        }
                    }
                    PointerTarget::Game => {
                        state.route_foreground_mouse(DomPointerKind::Move);
                    }
                }
                state.route_pointer(PointerPhase::Move, 0, 0, 0);
            }
            ShellWindowEvent::MouseInput { button, pressed }
                if !consumed || state.input.has_active_pointer_sequence() =>
            {
                if let Some(button) = button {
                    let dom_button = match button {
                        1 => 0,
                        2 => 2,
                        3 => 1,
                        value => value.saturating_sub(1),
                    };
                    let foreground_hit = state.input.foreground_region_claims(consumed, true);
                    let background_hit = state
                        .background_dom
                        .hit_test(state.input.cursor().x as f32, state.input.cursor().y as f32)
                        .is_some();
                    match state.input.pointer_target(consumed, true, background_hit) {
                        PointerTarget::Overlay => return false,
                        PointerTarget::Foreground => {
                            if pressed && foreground_hit {
                                state.input.set_foreground_mouse_capture(true);
                            }
                            state
                                .route_foreground_mouse(DomPointerKind::Button { button, pressed });
                            if !pressed && !state.input.foreground_buttons_active() {
                                state.input.set_foreground_mouse_capture(false);
                            }
                            return false;
                        }
                        PointerTarget::Background => {
                            let background_kind = if pressed { "mousedown" } else { "mouseup" };
                            if state.background_mouse(
                                background_kind,
                                state.input.cursor().x as f32,
                                state.input.cursor().y as f32,
                                dom_button,
                                Some(pressed),
                            ) {
                                return false;
                            }
                        }
                        PointerTarget::Game => {}
                    }
                    state.route_pointer(
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
            }
            ShellWindowEvent::MouseWheel {
                horizontal,
                vertical,
            } if !consumed => {
                match state.input.pointer_target(false, false, false) {
                    PointerTarget::Foreground => {
                        if horizontal != 0 {
                            state.route_foreground_mouse(DomPointerKind::Wheel {
                                horizontal: true,
                                delta: horizontal,
                            });
                        }
                        if vertical != 0 {
                            state.route_foreground_mouse(DomPointerKind::Wheel {
                                horizontal: false,
                                delta: vertical,
                            });
                        }
                        return false;
                    }
                    PointerTarget::Overlay => return false,
                    PointerTarget::Background | PointerTarget::Game => {}
                }
                state.route_pointer(PointerPhase::Wheel, 0, horizontal, vertical);
            }
            ShellWindowEvent::RedrawRequested => {
                let entered = state.profile.enter();
                if let Err(error) = state.window.draw_overlay() {
                    tracing::error!("snowglass: drawing egui composition overlay failed: {error}");
                }
                state.profile.record(TurnPhase::Overlay, entered);
            }
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
            let report = std::mem::take(&mut state.profile.report);
            state.window.set_overlay_note(&report);
            state.profile.report = report;
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

fn control_event(key: &str, value: i32) -> serde_json::Value {
    let mut fields = key.splitn(3, ':');
    let slot = fields
        .next()
        .and_then(|slot| slot.parse::<usize>().ok())
        .unwrap_or_default();
    let control_type = fields.next().unwrap_or_default();
    let control = fields.next().unwrap_or_default();
    serde_json::json!({
        "slot": slot,
        "controlType": control_type,
        "control": control,
        "value": value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::background_document::GameViewStyle;
    use crate::game_host::fit_surface_content;
    use snowglass_input::mouse::layout::Rect;
    use snowglass_renderer::SurfacePlacement;
    use style::{
        properties::generated::longhands::object_fit::computed_value::T as ObjectFit,
        values::computed::{LengthPercentage, Percentage, position::Position},
    };

    fn game_view_style(object_fit: ObjectFit, x: f32, y: f32) -> GameViewStyle {
        GameViewStyle {
            object_fit,
            object_position: Position::new(
                LengthPercentage::new_percent(Percentage(x)),
                LengthPercentage::new_percent(Percentage(y)),
            ),
        }
    }

    #[test]
    fn launcher_start_accepts_safe_integers_after_shared_bus_round_trip() {
        let command = serde_json::json!({
            "type": "launcherStart",
            "request": {
                "generation": 1,
                "emulator": "cemu",
                "groups": [{
                    "name": "controller",
                    "ports": [{ "index": 0, "layout": "WIIU_PRO_CONTROLLER", "device": "K" }]
                }],
                "settings": { "content-graphic.override_gamma_value": 2.2 }
            }
        });
        let command = snowglass_pluginhost::bus::StructuredValue::from_json(&command)
            .unwrap()
            .to_json()
            .unwrap();

        let event = decode_foreground_command(command).unwrap();
        let UserEvent::Runtime(RuntimeCommand::LauncherStart(request)) = event else {
            panic!("launcher command decoded to the wrong event");
        };
        assert_eq!(request.generation, 1);
        assert_eq!(request.groups[0].ports[0].index, 0);
        assert_eq!(
            request.settings["content-graphic.override_gamma_value"].as_f64(),
            Some(2.2)
        );
    }

    #[test]
    fn cover_crops_the_source_without_changing_the_layout_box() {
        let rectangle = Rect::new(10, 20, 200, 200);
        let style = game_view_style(ObjectFit::Cover, 0.5, 0.5);
        let content = fit_surface_content(rectangle, Extent::new(400, 200), Some(&style), 0.35);

        assert_eq!(content.destination, [10.0, 20.0, 210.0, 220.0]);
        assert_eq!(content.source, [0.25, 0.0, 0.75, 1.0]);
        assert!((content.opacity - 0.35).abs() < f32::EPSILON);
        let placement = SurfacePlacement {
            connection: 1,
            surface_id: 2,
            rectangle,
            content,
            corner_radius: 0.0,
        };
        assert_eq!(
            CompositingPlan::new([0.0; 4], vec![placement]).rectangle(1, 2),
            Some(rectangle)
        );
    }

    #[test]
    fn contain_and_object_position_move_only_the_draw_quad() {
        let style = game_view_style(ObjectFit::Contain, 1.0, 1.0);
        let content = fit_surface_content(
            Rect::new(10, 20, 200, 200),
            Extent::new(400, 200),
            Some(&style),
            1.0,
        );

        assert_eq!(content.destination, [10.0, 120.0, 210.0, 220.0]);
        assert_eq!(content.source, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn none_is_clipped_at_the_object_position() {
        let style = game_view_style(ObjectFit::None, 1.0, 1.0);
        let content = fit_surface_content(
            Rect::new(0, 0, 200, 100),
            Extent::new(400, 200),
            Some(&style),
            1.0,
        );

        assert_eq!(content.destination, [0.0, 0.0, 200.0, 100.0]);
        assert_eq!(content.source, [0.5, 0.5, 1.0, 1.0]);
    }
}
