use crate::cli::ShellOptions;
use crate::commands::ShellDocumentSource;
use crate::game_host::{GameHost, GameHostCaptureEvent};
use crate::input::{InputPriority, InputRouter, KeyboardTarget};
use crate::inspector::InspectorWindow;
use crate::instance_events;
use crate::launch::AttachedLaunchPlan;
use crate::metrics::{TurnPhase, TurnProfile};
use crate::shell_compositor::{ShellCompositor, ShellRasterizer};
use crate::shell_document::{DomKeyboardEvent, DomMouseEvent, DomWheelEvent};
use crate::shell_dom::{ShellSession, ShellSessionFactory};
use crate::shell_runtime::{InstanceToken, PolymerNotification, PolymerNotificationSender};
use crate::window_manager::{
    EventLoopProxy, LoopDirective, ShellDeviceEvent, ShellImeEvent, ShellWindowEvent,
    WindowCommand, WindowManager,
};
use polymer_config::input::InputTransaction;
use polymer_pluginhost::controller::{ControllerChanged, PendingReply, Quit};
use polymer_protocol::input::PointerPhase;
use polymer_renderer::{CompositingPlan, Extent};
use polymer_runtime::FrontendCommands;
use polymer_statistics::RecorderHandle;
use std::{
    error::Error,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use winit::window::Window;

mod actor;
mod application;
mod coordinator_commands;
mod coordinator_game;
mod coordinator_input;
mod coordinator_presentation;

pub(crate) use application::ShellApplication;

pub(crate) use crate::commands::UserEvent;

// Capture can produce events much faster than the window message queue when
// several streams are active. Keep each dispatch small so native window input
// and egui repaint messages get a turn between capture batches.
const MAX_CAPTURE_EVENTS_PER_TURN: usize = 32;
/// Composition-tree key for the single visual every stream shares when the
/// shell composites them together. Not a real connection.
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
/// Native notifications enter JavaScript through the Polymer inbox. Poll
/// the browser's asynchronous work once per UI frame instead of once per
/// cursor, capture, and compositor event.
const SHELL_RUNTIME_POLL_INTERVAL: Duration = Duration::from_millis(16);

struct ShellCoordinator {
    shell_dom: ShellSession,
    window: WindowManager,
    profile: TurnProfile,
    session: String,
    proxy: EventLoopProxy<UserEvent>,
    game_host: GameHost,
    shell_compositor: ShellCompositor,
    input: InputRouter,
    inspector: Option<InspectorWindow>,
    notifications: Option<PolymerNotificationSender>,
    _theme: crate::theme::SelectedTheme,
    quitting: bool,
}

enum InputChangeCompletion {
    Configuration { request_id: u64 },
    ConnectionSession { session_id: String },
    ControllerApi { value: serde_json::Value },
}

enum ControllerChange {
    Ready(serde_json::Value),
    Pending {
        transaction: Box<polymer_runtime::ControllerTopologyTransaction>,
        completion: InputChangeCompletion,
        response: PendingReply<ControllerChanged>,
    },
}

struct PendingQuit {
    token: InstanceToken,
    cache: crate::launch::QuickResumeCache,
    screenshot: Option<Vec<u8>>,
}

impl ShellCoordinator {
    async fn new(
        window: Window,
        proxy: EventLoopProxy<UserEvent>,
        options: ShellOptions,
        statistics_recorder: RecorderHandle,
        shell_factory: ShellSessionFactory,
        shell_rasterizer: ShellRasterizer,
    ) -> Result<Self, Box<dyn Error>> {
        let ShellOptions {
            debug,
            theme,
            plugins: _,
            directories: _,
            local_storage: _,
            library: _,
        } = options;

        let window = WindowManager::new(window, statistics_recorder, proxy.clone(), debug)?;
        let compositor = window.compositor();
        let (game_host, shell_compositor) = {
            // Prepare the retained Vello renderer before the first shell frame.
            let raster_proxy = proxy.clone();
            let mut shell_compositor =
                ShellCompositor::new(&compositor, shell_rasterizer, move || {
                    let _ =
                        raster_proxy.send_event(UserEvent::Window(WindowCommand::ShellRasterReady));
                })?;
            let game_host = GameHost::new(shell_compositor.device_context())?;
            shell_compositor.reconcile(&game_host)?;
            (game_host, shell_compositor)
        };
        window.set_shell_root(shell_compositor.root_visual())?;
        window.commit()?;
        let session = unique_session();
        let input_sources = shell_factory.input_sources();
        let controller_api = shell_factory.controller_api();
        let shell_dom = ShellSession::new(shell_factory);

        let input = InputRouter::runtime(input_sources, controller_api)
            .await
            .map_err(std::io::Error::other)?;

        let state = Self {
            shell_dom,
            window,
            profile: TurnProfile::default(),
            session,
            proxy,
            game_host,
            shell_compositor,
            input,
            inspector: None,
            notifications: None,
            _theme: theme,
            quitting: false,
        };
        Ok(state)
    }

    fn refresh_dirty_shell(&mut self) {
        if self.shell_dom.needs_paint() {
            self.refresh_shell();
        }
    }

    async fn activate_shell(
        &mut self,
        source: Option<ShellDocumentSource>,
        source_error: Option<String>,
        commands: FrontendCommands,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(error) = source_error {
            tracing::error!("polymer: loading the shell document failed: {error}");
        }
        if let Some(shell_source) = source {
            let size = self.window.size();
            let scale_factor = self.window.scale_factor();
            let html = shell_source.html.clone();
            let base_url = shell_source.base_url.clone();
            let notifications = match self
                .shell_dom
                .activate(|factory| async move {
                    factory
                        .create(
                            &html,
                            base_url,
                            size.width,
                            size.height,
                            scale_factor,
                            commands,
                        )
                        .await
                })
                .await
            {
                Ok(notifications) => notifications,
                Err(error) => {
                    self.shell_dom.disable();
                    return Err(error);
                }
            };
            let Some(notifications) = notifications else {
                tracing::error!(
                    "polymer: ignoring a second shell document at {}",
                    shell_source.base_url
                );
                return Ok(());
            };
            self.notifications = Some(notifications);
            self.refresh_shell();
            tracing::error!("polymer: using shell document at {}", shell_source.base_url);
        } else {
            // Failed/absent documents do not retain an inactive DOM slot.
            self.shell_dom.disable();
        }
        self.publish_input_configuration();
        Ok(())
    }
}

fn unique_session() -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("polymer-{}-{nonce:x}", std::process::id())
}
