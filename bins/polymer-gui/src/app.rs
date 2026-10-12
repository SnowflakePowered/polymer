use crate::cli::ShellOptions;
use crate::commands::ShellDocumentSource;
use crate::game_host::{GameHost, GameHostCaptureEvent};
use crate::input::{InputPriority, InputRouter};
use crate::inspector::InspectorWindow;
use crate::instance_events;
use crate::launch::AttachedLaunchPlan;
use crate::metrics::{TurnPhase, TurnProfile};
use crate::shell::protocol::{
    InputOutcome, LaunchSurfaceGroups, PointerSample, ShellCommand, ShellEvent, ShellInput,
};
use crate::shell::{ShellSession, ShellSessionFactory, ShellSource};
use crate::shell::runtime::{InstanceToken, PolymerNotification, PolymerNotificationSender};
use crate::window_manager::{
    EventLoopProxy, LoopDirective, ShellDeviceEvent, ShellImeEvent, ShellWindowEvent, WindowCommand,
};
use polymer_composition::{CompositionScene, CompositorFactory, CompositorHandle};
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
mod held_input;

pub(crate) use application::ShellApplication;

pub(crate) use crate::commands::UserEvent;
use held_input::{HeldInput, InputHold};

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
/// event loop.
const PRESENT_PERMIT_WAIT: Duration = Duration::from_millis(3);

struct ShellCoordinator {
    shell: ShellSession,
    compositor: CompositorHandle,
    pending_layouts:
        std::collections::BTreeMap<u64, Vec<crate::shell::protocol::TrackedSurfaceLayout>>,
    next_scene_revision: u64,
    profile: TurnProfile,
    session: String,
    proxy: EventLoopProxy<UserEvent>,
    game_host: GameHost,
    input: InputRouter,
    held: InputHold,
    inspector: Option<InspectorWindow>,
    notifications: Option<PolymerNotificationSender>,
    theme: crate::theme::SelectedTheme,
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
        compositor_factory: CompositorFactory,
    ) -> Result<Self, Box<dyn Error>> {
        let ShellOptions {
            debug,
            theme,
            plugins: _,
            directories: _,
            local_storage: _,
            library: _,
            jobs: _,
        } = options;

        let compositor_proxy = proxy.clone();
        let compositor = compositor_factory(
            window,
            statistics_recorder,
            std::sync::Arc::new(move |command| {
                compositor_proxy
                    .send_event(UserEvent::Window(command))
                    .is_ok()
            }),
            debug,
        )?;
        let game_host = GameHost::new(compositor.clone())?;
        let session = unique_session();
        let input_sources = shell_factory.input_sources();
        let controller_api = shell_factory.controller_api();
        let shell = ShellSession::new(shell_factory);

        let input = InputRouter::runtime(input_sources, controller_api)
            .await
            .map_err(std::io::Error::other)?;

        let state = Self {
            shell,
            compositor,
            pending_layouts: Default::default(),
            next_scene_revision: 0,
            profile: TurnProfile::default(),
            session,
            proxy,
            game_host,
            input,
            held: InputHold::default(),
            inspector: None,
            notifications: None,
            theme,
            quitting: false,
        };
        Ok(state)
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
            let size = self.compositor.borrow_mut().size();
            let scale_factor = self.compositor.borrow_mut().scale_factor();
            let source = ShellSource {
                html: shell_source.html.clone(),
                base_url: shell_source.base_url.clone(),
                theme: shell_source.theme.clone(),
                width: size.width,
                height: size.height,
                scale_factor,
            };
            let notifications = match self.shell.activate(source, commands).await {
                Ok(notifications) => notifications,
                Err(error) => {
                    self.shell.disable();
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
            tracing::error!("polymer: using shell document at {}", shell_source.base_url);
        } else {
            // Failed/absent documents do not retain an inactive DOM slot.
            self.shell.disable();
        }
        self.publish_input_configuration();
        Ok(())
    }

    async fn reload_shell(&mut self, commands: FrontendCommands) {
        if !self.shell.take_reload() {
            return;
        }
        self.game_host.clear_shaders();
        self.inspector = None;
        let (source, error) = match self.theme.reload_document().await {
            Ok(source) => (Some(source), None),
            Err(error) => (None, Some(error.to_string())),
        };
        if let Err(error) = self.activate_shell(source, error, commands).await {
            tracing::error!("polymer: reloading the shell document failed: {error}");
        }
    }
}

fn unique_session() -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("polymer-{}-{nonce:x}", std::process::id())
}
