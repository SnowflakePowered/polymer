//! Native emulator/capture state owned by the game-host subsystem.
//!
//! Validated launch-plan adaptation, process lifecycle, and captured-surface
//! models are contained here; the shell coordinator only forwards commands.

use crate::{
    launch::AttachedLaunchPlan, shell_document::GameViewStyle, shell_dom::TrackedSurfaceLayout,
};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use snowglass_input::mouse::layout::Rect;
use snowglass_platform_windows::performance_counter;
use snowglass_protocol::{AdapterLuid, PresentationMappingKind, PresentationTransform};
use snowglass_renderer::{
    CompositingPlan, D3d12Compositor, Extent, Renderer, SurfaceCapture, SurfaceCaptureEvent,
    SurfaceContent, SurfacePlacement, SurfacePointerSender,
};
use snowglass_runtime::{SurfaceFrameReceiver, SurfaceFrameUpdate};
use std::error::Error;
use std::time::Duration;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};
use windows::{
    Win32::Graphics::Direct3D12::ID3D12Device,
    core::{IUnknown, Interface},
};

fn hook_backend(hook: snowglass_pluginhost::GraphicsHook) -> snowglass_runtime::HookBackend {
    match hook {
        snowglass_pluginhost::GraphicsHook::Opengl => snowglass_runtime::HookBackend::OpenGl,
        snowglass_pluginhost::GraphicsHook::Vulkan => snowglass_runtime::HookBackend::Vulkan,
        snowglass_pluginhost::GraphicsHook::D3d11 => snowglass_runtime::HookBackend::D3d11,
        snowglass_pluginhost::GraphicsHook::D3d12 => snowglass_runtime::HookBackend::D3d12,
    }
}

/// Capture renderer state local to one D3D12 adapter. Shell rasterization and
/// WUC composition are owned independently by `ShellCompositor`.
struct RenderDevice {
    adapter: AdapterLuid,
    device: IUnknown,
    surface_compositors: HashMap<u64, SurfaceCompositor>,
}

struct SurfaceCompositor {
    compositor: Option<D3d12Compositor>,
    content: IUnknown,
    #[expect(
        dead_code,
        reason = "retains and joins the surface presentation thread"
    )]
    worker: Option<SurfacePresentationWorker>,
    latest_frame: Option<snowglass_protocol::FrameReady>,
    surface_id: u64,
    target_window: i64,
    quarter_turns: u8,
    flip_y: bool,
    width: u32,
    height: u32,
}

struct SurfaceWorkerFrame {
    connection: u64,
    update: SurfaceFrameUpdate,
    observed_qpc: u64,
}

/// Owns one captured stream and its opaque WUC swapchain on a presentation
/// thread. The compositor copies a producer slot into its private cache and
/// signals that slot's release without waiting for Blitz/Deno to finish a UI
/// turn.
struct SurfacePresentationWorker {
    stopping: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl SurfacePresentationWorker {
    #[expect(clippy::too_many_arguments)]
    fn spawn(
        mut compositor: D3d12Compositor,
        mut frames: SurfaceFrameReceiver,
        pointer: SurfacePointerSender,
        events: mpsc::Sender<SurfaceWorkerFrame>,
        connection: u64,
        surface_id: u64,
        width: u32,
        height: u32,
    ) -> Self {
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let worker = thread::Builder::new()
            .name(format!("snowglass-surface-{connection}"))
            .spawn(move || {
                let rectangle = Rect::new(0, 0, width.max(1), height.max(1));
                let mut placement =
                    SurfacePlacement::new(connection, surface_id, rectangle, 0.0);
                placement.content = SurfaceContent::fill(rectangle);
                let plan =
                    CompositingPlan::new([0.0, 0.0, 0.0, 1.0], vec![placement]);

                while !worker_stopping.load(Ordering::Acquire) {
                    let timeout = if Renderer::has_pending_frame(&compositor) {
                        Duration::from_millis(1)
                    } else {
                        Duration::from_millis(100)
                    };
                    match frames.next_frame_timeout(timeout) {
                        Ok(Some(update)) => {
                            pointer.remember_frame(connection, update);
                            Renderer::set_latest(&mut compositor, connection, update.frame);
                            let _ = events.send(SurfaceWorkerFrame {
                                connection,
                                update,
                                observed_qpc: performance_counter()
                                    .unwrap_or(update.frame.timestamp_qpc),
                            });
                        }
                        Ok(None) => {}
                        Err(_) => break,
                    }

                    if Renderer::has_pending_frame(&compositor)
                        && compositor.begin_frame_within(Duration::from_millis(2))
                        && let Err(error) = compositor.present_with_webrtc(&plan, None)
                    {
                        tracing::error!(
                            "snowglass: presenting surface {connection} on its worker failed: {error}"
                        );
                        break;
                    }
                }
            })
            .expect("failed to create a captured-surface presentation worker");
        Self {
            stopping,
            worker: Some(worker),
        }
    }
}

impl Drop for SurfacePresentationWorker {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Delegates native handles while overriding only the composition swapchain
/// extent. Tracked surfaces render in local coordinates and WUC owns their
/// screen-space offset and size.
struct LocalRenderWindow<'a, W> {
    window: &'a W,
    extent: Extent,
}

impl<W: HasWindowHandle> HasWindowHandle for LocalRenderWindow<'_, W> {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        self.window.window_handle()
    }
}

impl<W: HasDisplayHandle> HasDisplayHandle for LocalRenderWindow<'_, W> {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        self.window.display_handle()
    }
}

impl<W: snowglass_renderer::RenderWindow> snowglass_renderer::RenderWindow
    for LocalRenderWindow<'_, W>
{
    fn render_extent(&self) -> Extent {
        self.extent
    }
}

impl RenderDevice {
    fn new((adapter, device): (AdapterLuid, IUnknown)) -> Self {
        Self {
            adapter,
            device,
            surface_compositors: HashMap::new(),
        }
    }

    fn adapter_luid(&self) -> AdapterLuid {
        self.adapter
    }

    fn d3d12_device(&self) -> Result<ID3D12Device, windows::core::Error> {
        self.device.cast()
    }

    fn create_surface_compositor(
        &self,
        window: &impl snowglass_renderer::RenderWindow,
        connection: u64,
        width: u32,
        height: u32,
    ) -> Result<D3d12Compositor, Box<dyn Error>> {
        let local = LocalRenderWindow {
            window,
            extent: Extent::new(width.max(1), height.max(1)),
        };
        D3d12Compositor::new_for_stream_composition(&local, self.adapter, connection)
    }
}

#[derive(Debug)]
pub(crate) enum GameHostCommand {
    Emulator(crate::shell_runtime::EmulatorCommand),
    Input(crate::shell_runtime::InputCommand),
    WebRtc(crate::shell_runtime::WebRtcCommand),
}

#[derive(Clone)]
struct CapturedSurface {
    index: u32,
    preferred_width: u32,
    preferred_height: u32,
    viewport_width: u32,
    viewport_height: u32,
    viewport_x: i32,
    viewport_y: i32,
    quarter_turns: u8,
    pointer_events: bool,
    z_index: i32,
    group: Option<u64>,
    corner_radius: f32,
    game_view_style: Option<GameViewStyle>,
    opacity: f32,
}

struct ActivePresentation {
    token: crate::shell_runtime::InstanceToken,
    options: AttachedLaunchPlan,
    webrtc: Option<snowglass_renderer::webrtc::Publication>,
    controller_session: Option<snowglass_pluginhost::ControllerSession>,
    automation: Option<snowglass_automation::AutomationSession>,
}

pub(crate) struct ConnectedSurface {
    pub connection: u64,
    pub surface_id: u64,
    pub target_index: u32,
    pub replaced_connection: Option<u64>,
    pub backend: String,
}

pub(crate) struct AcceptedSurface {
    pub surface: Option<ConnectedSurface>,
}

pub(crate) enum GameHostCaptureEvent {
    AdapterRequired(AdapterLuid),
    Connected(AcceptedSurface),
    Frame {
        connection: u64,
        frame: snowglass_protocol::FrameReady,
        observed_qpc: Option<u64>,
        dimensions_changed: bool,
    },
    Removed {
        connection: u64,
        layout_changed: bool,
    },
    Rejected {
        connection: u64,
        error: String,
    },
}

pub(crate) struct FinishedGame {
    pub token: crate::shell_runtime::InstanceToken,
    pub connections: Vec<u64>,
    pub finalization_error: Option<String>,
}

pub(crate) struct GameSurfaceTarget {
    pub connection: u64,
    pub index: u32,
    pub width: u32,
    pub height: u32,
}

pub(crate) struct CapturedSurfaceSnapshot {
    pub connection: u64,
    pub parent: Option<u64>,
    pub z_order: i32,
    pub content: IUnknown,
    pub placement: SurfaceContent,
}

pub(crate) struct GameHost {
    active: Option<ActivePresentation>,
    session: snowglass_runtime::SessionController,
    capture: SurfaceCapture,
    pointer: SurfacePointerSender,
    wake_pending: Arc<AtomicBool>,
    dirty: Arc<AtomicBool>,
    work_pending: bool,
    surface_worker_events: mpsc::Receiver<SurfaceWorkerFrame>,
    surface_worker_sender: mpsc::Sender<SurfaceWorkerFrame>,
    surfaces: HashMap<u64, CapturedSurface>,
    pending_offer: Option<snowglass_runtime::SurfaceOffer>,
    /// Adapter-local capture device and imported captured streams.
    render_device: RenderDevice,
    composition_revision: u64,
    shell_refresh_pending: bool,
}

impl GameHost {
    pub fn new(context: (AdapterLuid, IUnknown)) -> Result<Self, Box<dyn Error>> {
        let capture = SurfaceCapture::default();
        let pointer = capture.pointer_sender();
        let render_device = RenderDevice::new(context);
        let (surface_worker_sender, surface_worker_events) = mpsc::channel();
        Ok(Self {
            active: None,
            session: snowglass_runtime::SessionController::default(),
            capture,
            pointer,
            wake_pending: Arc::new(AtomicBool::new(false)),
            dirty: Arc::new(AtomicBool::new(false)),
            work_pending: false,
            surface_worker_events,
            surface_worker_sender,
            surfaces: HashMap::new(),
            pending_offer: None,
            render_device,
            composition_revision: 0,
            shell_refresh_pending: false,
        })
    }

    pub(crate) fn captured_surface_snapshots(&self) -> Vec<CapturedSurfaceSnapshot> {
        let mut snapshots = Vec::new();
        for (&connection, surface) in &self.surfaces {
            let Some(output) = self.render_device.surface_compositors.get(&connection) else {
                continue;
            };
            let rectangle = Rect::new(
                surface.viewport_x,
                surface.viewport_y,
                surface.viewport_width,
                surface.viewport_height,
            );
            let source = Extent::new(
                surface.preferred_width.max(1),
                surface.preferred_height.max(1),
            );
            let fitted = fit_surface_content(
                rectangle,
                source,
                surface.game_view_style.as_ref(),
                surface.opacity,
            );
            snapshots.push(CapturedSurfaceSnapshot {
                connection,
                parent: surface.group,
                z_order: surface.z_index,
                content: output.content.clone(),
                placement: fitted,
            });
        }
        snapshots.sort_by_key(|surface| surface.z_order);
        snapshots
    }

    pub(crate) const fn composition_revision(&self) -> u64 {
        self.composition_revision
    }

    fn mark_composition_dirty(&mut self) {
        self.composition_revision = self.composition_revision.wrapping_add(1);
    }

    pub fn is_active(&self, token: crate::shell_runtime::InstanceToken) -> bool {
        self.session.is_active(token.0)
    }

    pub fn active_token(&self) -> Option<crate::shell_runtime::InstanceToken> {
        self.session
            .active_token()
            .map(crate::shell_runtime::InstanceToken)
    }

    pub fn has_active_game(&self) -> bool {
        self.session.has_active_session()
    }

    pub fn controller_topology_locked(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.controller_session.is_none())
    }

    pub fn controller_mappings(&self) -> &[snowglass_input::topology::ControllerMapping] {
        self.active
            .as_ref()
            .map(|active| active.options.controller_topology.mappings.as_slice())
            .unwrap_or_default()
    }

    pub fn controller_change(
        &self,
        configuration: &snowglass_input::hub::InputConfiguration,
        state: &serde_json::Value,
    ) -> Result<Option<tokio::sync::oneshot::Receiver<Result<(), String>>>, String> {
        let Some(active) = &self.active else {
            return Ok(None);
        };
        let Some(session) = &active.controller_session else {
            return Ok(None);
        };
        let payload = Self::controller_change_payload(active, configuration, state)?;
        session.change(payload).map(Some)
    }

    fn controller_change_payload(
        active: &ActivePresentation,
        configuration: &snowglass_input::hub::InputConfiguration,
        state: &serde_json::Value,
    ) -> Result<snowglass_input::topology::ControllerTopology, String> {
        let players = state["controllers"]["players"]
            .as_array()
            .or_else(|| state["players"].as_array());
        let mappings = configuration
            .ports
            .iter()
            .map(|port| {
                let slot = port.slot.ok_or_else(|| {
                    format!(
                        "runtime controller topology requires a virtual gamepad for {}",
                        port.port
                    )
                })?;
                let baseline = active
                    .options
                    .controller_topology
                    .mappings
                    .iter()
                    .find(|mapping| mapping.gamepad == slot);
                let source = configuration
                    .assignments
                    .iter()
                    .find(|assignment| assignment.index == slot)
                    .map(|assignment| &assignment.source);
                let controller = players
                    .into_iter()
                    .flatten()
                    .find(|player| player["index"].as_u64() == Some(u64::from(slot)))
                    .and_then(|player| {
                        (!player["controller"].is_null()).then(|| player["controller"].clone())
                    });
                let connected = controller
                    .as_ref()
                    .and_then(|controller| controller["connected"].as_bool())
                    .unwrap_or({
                        matches!(
                            source,
                            Some(snowglass_input::hub::SourceSelector::KeyboardMouse)
                        )
                    });
                Ok(snowglass_input::topology::ControllerMapping {
                    target: snowglass_input::topology::ControllerTarget {
                        group: port.port.group.clone(),
                        port: usize::from(port.port.index),
                    },
                    gamepad: slot,
                    source: source.cloned(),
                    layout: port.layout.clone(),
                    connected,
                    mouse: port.force_mouse || baseline.is_some_and(|mapping| mapping.mouse),
                    projection_mode: baseline
                        .map(|mapping| mapping.projection_mode)
                        .unwrap_or_default(),
                    mapping: baseline
                        .map(|mapping| mapping.mapping.clone())
                        .unwrap_or_default(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(snowglass_input::topology::ControllerTopology { mappings })
    }

    pub fn take_controller_errors(
        &self,
    ) -> Option<(crate::shell_runtime::InstanceToken, Vec<String>)> {
        let active = self.active.as_ref()?;
        let session = active.controller_session.as_ref()?;
        let errors = session.take_errors();
        (!errors.is_empty()).then_some((active.token, errors))
    }

    pub fn surface_targets(&self) -> Vec<GameSurfaceTarget> {
        self.surfaces
            .iter()
            .map(|(&connection, surface)| GameSurfaceTarget {
                connection,
                index: surface.index,
                width: surface.preferred_width,
                height: surface.preferred_height,
            })
            .collect()
    }

    pub fn pointer_sender(&self) -> &SurfacePointerSender {
        &self.pointer
    }

    pub fn pointer_presentation(
        &self,
        extent: Extent,
    ) -> (snowglass_renderer::Presentation, CompositingPlan) {
        let mut received = Vec::new();
        let mut placements = Vec::new();
        let mut surfaces = self
            .surfaces
            .iter()
            .filter(|(_, surface)| surface.pointer_events)
            .collect::<Vec<_>>();
        surfaces.sort_by_key(|(_, surface)| std::cmp::Reverse(surface.z_index));
        for (&connection, surface) in surfaces {
            let received_surfaces = self
                .render_device
                .surface_compositors
                .get(&connection)
                .and_then(|output| {
                    let frame = output.latest_frame?;
                    let (width, height) = crate::game_host::rotated_extent(
                        frame.render_width.max(1),
                        frame.render_height.max(1),
                        output.quarter_turns,
                    );
                    Some(vec![snowglass_renderer::ReceivedSurface {
                        connection,
                        surface_id: output.surface_id,
                        target_window: output.target_window,
                        image_extent: Extent::new(width, height),
                        quarter_turns: output.quarter_turns,
                        flip_y: output.flip_y,
                        frame,
                    }])
                })
                .unwrap_or_default();
            for mut received_surface in received_surfaces {
                received_surface.frame.presentation_transform = normalize_pointer_transform(
                    received_surface.frame.presentation_transform,
                    received_surface.frame.presentation_width,
                    received_surface.frame.presentation_height,
                );
                let rectangle = Rect::new(
                    surface.viewport_x,
                    surface.viewport_y,
                    surface.viewport_width,
                    surface.viewport_height,
                );
                let mut placement = SurfacePlacement::new(
                    connection,
                    received_surface.surface_id,
                    rectangle,
                    surface.corner_radius,
                );
                placement.content = fit_surface_content(
                    rectangle,
                    received_surface.image_extent,
                    surface.game_view_style.as_ref(),
                    surface.opacity,
                );
                placements.push(placement);
                received.push(received_surface);
            }
        }
        (
            snowglass_renderer::Presentation::new(extent, received),
            CompositingPlan::new([0.0; 4], placements),
        )
    }

    pub fn begin_capture_drain(&self) {
        self.dirty
            .store(false, std::sync::atomic::Ordering::Release);
    }

    pub fn finish_capture_drain(&mut self, exhausted_turn_budget: bool) {
        self.wake_pending
            .store(false, std::sync::atomic::Ordering::Release);
        let arrived_while_draining = self.dirty.swap(false, std::sync::atomic::Ordering::AcqRel);
        self.work_pending = exhausted_turn_budget || arrived_while_draining;
    }

    pub const fn has_capture_work(&self) -> bool {
        self.work_pending
    }

    pub fn take_shell_refresh_request(&mut self) -> bool {
        std::mem::take(&mut self.shell_refresh_pending)
    }

    pub fn clear_shell_refresh_request(&mut self) {
        self.shell_refresh_pending = false;
    }

    pub fn launch(
        &mut self,
        token: crate::shell_runtime::InstanceToken,
        options: AttachedLaunchPlan,
        mut filesystem: Option<snowglass_vfs::MountedFilesystem>,
        controller_session: Option<snowglass_pluginhost::ControllerSession>,
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
        let mut gamepad_layouts = options
            .controller_topology
            .mappings
            .iter()
            .collect::<Vec<_>>();
        gamepad_layouts.sort_by_key(|mapping| mapping.gamepad);
        let automation_desktop = options.isolated.then(|| session.clone());
        let config = snowglass_runtime::EmulatorLaunchConfig {
            session,
            command: options.command.clone(),
            arguments: options.arguments.clone(),
            artifact_root,
            hooks: options.hook.iter().copied().map(hook_backend).collect(),
            consumer: snowglass_runtime::ConsumerPreference::D3d12,
            isolate: options.isolated,
            tracked_surfaces: options
                .surfaces
                .iter()
                .map(|surface| snowglass_runtime::TrackedSurfaceConfig {
                    id: snowglass_runtime::TrackedSurfaceId(surface.index),
                    render_target_index: surface.index,
                    window_pattern: surface.selector.clone(),
                })
                .collect(),
            tracking_enabled: !options.surfaces.is_empty(),
            skip_surfaces: 0,
            graph_tracking: options.graph_tracking,
            force_keyed_mutex: false,
            gamepad_layouts: gamepad_layouts
                .into_iter()
                .map(|mapping| mapping.layout.clone())
                .collect(),
        };
        let dirty = self.dirty.clone();
        let wake_pending = self.wake_pending.clone();
        let input_controller_session = controller_session.clone();
        self.session.launch_with_input(
            token.0,
            config,
            filesystem.take(),
            move || {
                dirty.store(true, std::sync::atomic::Ordering::Release);
                if !wake_pending.swap(true, std::sync::atomic::Ordering::AcqRel) && !wake_window() {
                    wake_pending.store(false, std::sync::atomic::Ordering::Release);
                }
            },
            move |connections| {
                if let Some(controller_session) = input_controller_session {
                    controller_session
                        .attach(move |connection| connections.set_connected(connection))?;
                }
                Ok(())
            },
        )?;
        let automation = if let Some(controller_session) = &controller_session {
            let process_id = self
                .session
                .instance()
                .expect("a successful session launch owns an emulator instance")
                .process_id();
            let attached = snowglass_platform_windows::WindowsAutomationDriver::new(
                process_id,
                automation_desktop,
            )
            .and_then(snowglass_automation::AutomationSession::spawn)
            .map_err(|error| format!("could not start emulator UI automation: {error}"))
            .and_then(|automation| {
                controller_session.attach_automation(automation.clone())?;
                Ok(automation)
            });
            match attached {
                Ok(automation) => Some(automation),
                Err(error) => {
                    let _ = self
                        .session
                        .finish(snowglass_vfs::FinalizationReason::LaunchFailed);
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
        self.active = Some(ActivePresentation {
            token,
            options,
            webrtc: None,
            controller_session,
            automation,
        });
        Ok(())
    }

    pub fn start_webrtc(
        &mut self,
        token: crate::shell_runtime::InstanceToken,
        bind: std::net::SocketAddr,
        targets: &[u32],
    ) -> Result<(), String> {
        let Some(active) = self.active.as_mut().filter(|active| active.token == token) else {
            return Err("the emulator instance is not active".to_owned());
        };
        if active.webrtc.is_some() {
            return Err("the emulator instance already has a WebRTC publication".to_owned());
        }
        if let Some(target) = targets.iter().find(|target| {
            !active
                .options
                .surfaces
                .iter()
                .any(|surface| surface.index == **target)
        }) {
            return Err(format!(
                "WebRTC render target {target} is not part of the emulator launch"
            ));
        }
        let device = self
            .render_device
            .d3d12_device()
            .map_err(|error| error.to_string())?;
        let mut publication = snowglass_renderer::webrtc::Publication::start_d3d12_device(
            bind,
            targets,
            self.render_device.adapter_luid(),
            &device,
        )
        .map_err(|error| error.to_string())?;
        for (&connection, surface) in &self.surfaces {
            publication
                .add_stream(connection, surface.index)
                .map_err(|error| error.to_string())?;
        }
        active.webrtc = Some(publication);
        Ok(())
    }

    pub fn stop_webrtc(&mut self, token: crate::shell_runtime::InstanceToken) -> bool {
        let Some(active) = self.active.as_mut().filter(|active| active.token == token) else {
            return false;
        };
        active.webrtc.take().is_some()
    }

    pub fn input_endpoint(&self) -> Option<crate::input::GameInputEndpoint> {
        self.session.instance().map(|instance| {
            crate::input::GameInputEndpoint::new(
                instance.protocol_input_sender(),
                instance.gamepad_input(),
            )
        })
    }

    pub fn request_close(&mut self, token: crate::shell_runtime::InstanceToken) -> bool {
        if !self.session.close(token.0) {
            return false;
        }
        self.capture.detach();
        true
    }

    pub fn set_volume(&self, token: crate::shell_runtime::InstanceToken, volume: f64) -> bool {
        self.session.set_volume(token.0, volume)
    }

    pub fn set_muted(&self, token: crate::shell_runtime::InstanceToken, muted: bool) -> bool {
        self.session.set_muted(token.0, muted)
    }

    pub fn audio_change(
        &mut self,
        force: bool,
    ) -> Option<(
        crate::shell_runtime::InstanceToken,
        snowglass_runtime::AudioState,
    )> {
        self.session
            .audio_change(force)
            .map(|(token, audio)| (crate::shell_runtime::InstanceToken(token), audio))
    }

    pub fn has_exited(&self) -> bool {
        self.session.has_exited()
    }

    pub fn detach_capture(&mut self) {
        self.capture.detach();
    }

    /// Consumes and applies one capture event. Raw runtime offers and frame
    /// updates never leave the game-host boundary.
    pub fn try_capture_event(
        &mut self,
        window: &impl snowglass_renderer::RenderWindow,
    ) -> Option<GameHostCaptureEvent> {
        if let Some(offer) = self.pending_offer.take() {
            return Some(self.handle_surface_offer(window, offer));
        }
        while let Ok(worker) = self.surface_worker_events.try_recv() {
            if let Some((frame, dimensions_changed)) =
                self.update_surface_frame(worker.connection, worker.update)
            {
                return Some(GameHostCaptureEvent::Frame {
                    connection: worker.connection,
                    frame,
                    observed_qpc: Some(worker.observed_qpc),
                    dimensions_changed,
                });
            }
        }
        loop {
            return Some(match self.capture.try_recv().ok()? {
                SurfaceCaptureEvent::Offered(offer) => self.handle_surface_offer(window, offer),
                SurfaceCaptureEvent::Frame { connection, update } => {
                    let Some((frame, dimensions_changed)) =
                        self.update_surface_frame(connection, update)
                    else {
                        continue;
                    };
                    GameHostCaptureEvent::Frame {
                        connection,
                        frame,
                        observed_qpc: None,
                        dimensions_changed,
                    }
                }
                SurfaceCaptureEvent::Removed { connection, .. } => GameHostCaptureEvent::Removed {
                    connection,
                    layout_changed: self.remove_surface(connection),
                },
            });
        }
    }

    fn handle_surface_offer(
        &mut self,
        window: &impl snowglass_renderer::RenderWindow,
        offer: snowglass_runtime::SurfaceOffer,
    ) -> GameHostCaptureEvent {
        let desired_adapter = offer.stream().adapter_luid;
        if self.render_device.adapter_luid() != desired_adapter {
            self.pending_offer = Some(offer);
            return GameHostCaptureEvent::AdapterRequired(desired_adapter);
        }
        let connection = offer.connection_id();
        match self.accept_surface_offer(window, offer) {
            Ok(accepted) => GameHostCaptureEvent::Connected(accepted),
            Err(error) => GameHostCaptureEvent::Rejected {
                connection,
                error: error.to_string(),
            },
        }
    }

    /// Selects the offer's adapter, imports it into the window-spanning
    /// compositor, and accepts the capture stream as one atomic host action.
    fn accept_surface_offer(
        &mut self,
        window: &impl snowglass_renderer::RenderWindow,
        offer: snowglass_runtime::SurfaceOffer,
    ) -> Result<AcceptedSurface, Box<dyn Error>> {
        let Some(surface_capacity) = self
            .active
            .as_ref()
            .map(|active| active.options.surfaces.len())
        else {
            self.capture.reject(offer);
            return Ok(AcceptedSurface { surface: None });
        };
        let connection = offer.connection_id();
        let surface_id = offer.stream().surface_id.0;
        let replaced_connection = offer
            .replaced_connection_id()
            .filter(|connection| self.surfaces.contains_key(connection));
        if replaced_connection.is_none() && self.surfaces.len() >= surface_capacity {
            self.capture.reject(offer);
            return Ok(AcceptedSurface { surface: None });
        }

        let (surface, target_index, quarter_turns) = if let Some(previous) = replaced_connection {
            let surface = self
                .surfaces
                .get(&previous)
                .cloned()
                .ok_or("the replaced tracked surface is no longer connected")?;
            let target_index = surface.index;
            let quarter_turns = surface.quarter_turns;
            (surface, target_index, quarter_turns)
        } else {
            let (target_index, quarter_turns) = self
                .next_target_configuration()
                .ok_or("the game host already has every requested render target")?;
            let stream = offer.stream();
            let (width, height) = rotated_extent(stream.width, stream.height, quarter_turns);
            (
                CapturedSurface {
                    index: target_index,
                    preferred_width: width,
                    preferred_height: height,
                    viewport_width: width.max(1),
                    viewport_height: height.max(1),
                    viewport_x: 0,
                    viewport_y: 0,
                    quarter_turns,
                    pointer_events: true,
                    z_index: target_index as i32,
                    group: None,
                    corner_radius: 0.0,
                    game_view_style: None,
                    opacity: 1.0,
                },
                target_index,
                quarter_turns,
            )
        };
        let backend = format!("{:?}", offer.stream().image_kind);
        let target_window = offer.stream().source_window;
        let flip_y = offer.stream().bottom_up;
        let (output_width, output_height) =
            rotated_extent(offer.stream().width, offer.stream().height, quarter_turns);
        let mut compositor = self.render_device.create_surface_compositor(
            window,
            connection,
            output_width,
            output_height,
        )?;
        Renderer::import_surface_offer(&mut compositor, &offer, quarter_turns)?;
        let content = compositor.composition_content()?;

        let use_worker = self
            .active
            .as_ref()
            .is_some_and(|active| active.webrtc.is_none());
        if use_worker {
            let (_, frames) = match self.capture.accept_detached(offer) {
                Ok(accepted) => accepted,
                Err(error) => {
                    self.render_device.surface_compositors.remove(&connection);
                    return Err(error.into());
                }
            };
            let worker = SurfacePresentationWorker::spawn(
                compositor,
                frames,
                self.pointer.clone(),
                self.surface_worker_sender.clone(),
                connection,
                surface_id,
                output_width.max(1),
                output_height.max(1),
            );
            self.render_device.surface_compositors.insert(
                connection,
                SurfaceCompositor {
                    compositor: None,
                    content,
                    worker: Some(worker),
                    latest_frame: None,
                    surface_id,
                    target_window,
                    quarter_turns,
                    flip_y,
                    width: output_width.max(1),
                    height: output_height.max(1),
                },
            );
        } else if let Err(error) = self.capture.accept(offer) {
            return Err(error.into());
        } else {
            self.render_device.surface_compositors.insert(
                connection,
                SurfaceCompositor {
                    compositor: Some(compositor),
                    content,
                    worker: None,
                    latest_frame: None,
                    surface_id,
                    target_window,
                    quarter_turns,
                    flip_y,
                    width: output_width.max(1),
                    height: output_height.max(1),
                },
            );
        }

        if let Some(publication) = self
            .active
            .as_mut()
            .and_then(|active| active.webrtc.as_mut())
            && let Err(error) = publication.add_stream(connection, target_index)
        {
            self.capture.revoke(connection);
            self.render_device.surface_compositors.remove(&connection);
            return Err(error);
        }
        self.surfaces.insert(connection, surface);
        if let Some(previous) = replaced_connection {
            self.surfaces.remove(&previous);
            self.render_device.surface_compositors.remove(&previous);
            if let Some(publication) = self
                .active
                .as_mut()
                .and_then(|active| active.webrtc.as_mut())
            {
                publication.remove_stream(previous);
            }
        }
        self.mark_composition_dirty();
        Ok(AcceptedSurface {
            surface: Some(ConnectedSurface {
                connection,
                surface_id,
                target_index,
                replaced_connection,
                backend,
            }),
        })
    }

    pub(crate) fn replace_render_device(
        &mut self,
        context: (AdapterLuid, IUnknown),
    ) -> Result<Vec<u64>, Box<dyn Error>> {
        let replacement = RenderDevice::new(context);
        if let Some(publication) = self
            .active
            .as_mut()
            .and_then(|active| active.webrtc.as_mut())
        {
            let device = replacement.d3d12_device()?;
            publication.reattach_d3d12_device(replacement.adapter_luid(), &device)?;
        }
        self.render_device = replacement;
        let stale_connections = self.surfaces.keys().copied().collect::<Vec<_>>();
        for &connection in &stale_connections {
            self.capture.revoke(connection);
            if let Some(publication) = self
                .active
                .as_mut()
                .and_then(|active| active.webrtc.as_mut())
            {
                publication.remove_stream(connection);
            }
        }
        self.surfaces.clear();
        self.mark_composition_dirty();
        self.shell_refresh_pending = true;
        Ok(stale_connections)
    }

    pub(crate) fn reject_pending_offer(&mut self) -> Option<u64> {
        let offer = self.pending_offer.take()?;
        let connection = offer.connection_id();
        self.capture.reject(offer);
        Some(connection)
    }

    fn update_surface_frame(
        &mut self,
        connection: u64,
        update: snowglass_runtime::SurfaceFrameUpdate,
    ) -> Option<(snowglass_protocol::FrameReady, bool)> {
        let frame = update.frame;
        let surface = self.surfaces.get_mut(&connection)?;
        let (width, height) = rotated_extent(
            frame.render_width.max(1),
            frame.render_height.max(1),
            surface.quarter_turns,
        );
        let dimensions_changed =
            (surface.preferred_width, surface.preferred_height) != (width, height);
        if dimensions_changed {
            surface.preferred_width = width;
            surface.preferred_height = height;
        }
        if let Some(output) = self.render_device.surface_compositors.get_mut(&connection) {
            output.latest_frame = Some(frame);
            if let Some(compositor) = output.compositor.as_mut() {
                Renderer::set_latest(compositor, connection, frame);
            }
        }
        if dimensions_changed {
            self.mark_composition_dirty();
        }
        Some((frame, dimensions_changed))
    }

    fn remove_surface(&mut self, connection: u64) -> bool {
        if let Some(publication) = self
            .active
            .as_mut()
            .and_then(|active| active.webrtc.as_mut())
        {
            publication.remove_stream(connection);
        }
        let removed = self.surfaces.remove(&connection).is_some();
        if removed {
            self.render_device.surface_compositors.remove(&connection);
            self.mark_composition_dirty();
        }
        removed
    }

    pub fn finish_active_game(&mut self) -> Option<FinishedGame> {
        let mut active = self.active.take()?;
        let connections = self.surfaces.keys().copied().collect::<Vec<_>>();
        for &connection in &connections {
            if let Some(publication) = &mut active.webrtc {
                publication.remove_stream(connection);
            }
            self.capture.revoke(connection);
            self.render_device.surface_compositors.remove(&connection);
        }
        self.capture.detach();
        self.surfaces.clear();
        self.pending_offer = None;
        self.mark_composition_dirty();
        let token = active.token;
        if let Some(automation) = active.automation.take() {
            automation.close();
        }
        drop(active.controller_session.take());
        let finalization_error = self
            .session
            .finish(snowglass_vfs::FinalizationReason::ProcessExited)
            .err();
        Some(FinishedGame {
            token,
            connections,
            finalization_error,
        })
    }

    fn next_target_configuration(&self) -> Option<(u32, u8)> {
        let active = self.active.as_ref()?;
        active
            .options
            .surfaces
            .iter()
            .find(|target| {
                !self
                    .surfaces
                    .values()
                    .any(|surface| surface.index == target.index)
            })
            .map(|target| (target.index, normalized_quarter_turns(target.rotation)))
    }

    pub fn has_pending_frame(&self) -> bool {
        self.render_device
            .surface_compositors
            .values()
            .filter_map(|output| output.compositor.as_ref())
            .any(Renderer::has_pending_frame)
    }

    pub fn draw(
        &mut self,
        _width: u32,
        _height: u32,
        permit_wait: Duration,
    ) -> Result<Option<bool>, String> {
        let mut publication = self
            .active
            .as_mut()
            .and_then(|active| active.webrtc.as_mut());
        let mut presented = false;
        let mut deferred = false;
        for (&connection, output) in &mut self.render_device.surface_compositors {
            let Some(compositor) = output.compositor.as_mut() else {
                continue;
            };
            if !Renderer::has_pending_frame(compositor) {
                continue;
            }
            if !compositor.begin_frame_within(permit_wait) {
                deferred = true;
                continue;
            }
            let Some(received) = Renderer::received_surfaces(compositor).into_iter().next() else {
                continue;
            };
            let rectangle = Rect::new(0, 0, output.width, output.height);
            let mut placement = SurfacePlacement::new(
                connection,
                received.surface_id,
                rectangle,
                self.surfaces
                    .get(&connection)
                    .map_or(0.0, |surface| surface.corner_radius),
            );
            placement.content = SurfaceContent::fill(rectangle);
            let plan = CompositingPlan::new([0.0, 0.0, 0.0, 1.0], vec![placement]);
            if let Some(publication) = publication.as_deref_mut() {
                publication
                    .present_d3d12(compositor, &plan)
                    .map_err(|error| error.to_string())?;
            } else {
                compositor
                    .present_with_webrtc(&plan, None)
                    .map_err(|error| error.to_string())?;
            }
            presented = true;
        }
        if presented {
            Ok(Some(true))
        } else if deferred {
            Ok(None)
        } else {
            // Worker-owned tracked surfaces present independently. No main
            // compositor submission happened, so do not report a repeated
            // present or keep the compositor clock active.
            Ok(None)
        }
    }

    pub fn apply_tracked_layout(&mut self, placements: &[TrackedSurfaceLayout]) -> bool {
        let mut placed = HashSet::new();
        let mut changed = false;
        for placement in placements {
            let Some((&connection, surface)) = self
                .surfaces
                .iter_mut()
                .find(|(_, surface)| u64::from(surface.index) == placement.source_id)
            else {
                continue;
            };
            // Keep the guest visual in the tracked element's full coordinate
            // space. Shrinking this rectangle to the accumulated paint clip
            // changes object-fit and relocates the sampled image whenever a
            // panel approaches a viewport edge, while its chrome remains in
            // the original space. The desktop composition root already clips
            // visuals at the window boundary; finer clips belong on the WUC
            // visual itself, not in its layout geometry.
            let rect = placement.bounds;
            let next = SurfaceGeometry {
                x: rect.x0.round() as i32,
                y: rect.y0.round() as i32,
                width: rect.width().round().max(0.0) as u32,
                height: rect.height().round().max(0.0) as u32,
                pointer_events: true,
                z_index: placement.z_order,
                group: placement.group,
                corner_radius: placement.corner_radius,
                style: placement.style.clone(),
                opacity: placement.opacity,
            };
            changed |= apply_geometry(surface, next);
            placed.insert(connection);
        }

        let requested = self
            .active
            .as_ref()
            .map(|active| {
                active
                    .options
                    .surfaces
                    .iter()
                    .map(|surface| surface.index)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        for (&connection, surface) in &mut self.surfaces {
            if placed.contains(&connection) || !requested.contains(&surface.index) {
                continue;
            }
            changed |= apply_geometry(
                surface,
                SurfaceGeometry {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                    pointer_events: false,
                    z_index: 1,
                    group: None,
                    corner_radius: 0.0,
                    style: None,
                    opacity: 1.0,
                },
            );
        }
        if changed {
            self.mark_composition_dirty();
        }
        changed
    }
}

struct SurfaceGeometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    pointer_events: bool,
    z_index: i32,
    group: Option<u64>,
    corner_radius: f32,
    style: Option<GameViewStyle>,
    opacity: f32,
}

fn apply_geometry(surface: &mut CapturedSurface, next: SurfaceGeometry) -> bool {
    let changed = surface.viewport_x != next.x
        || surface.viewport_y != next.y
        || surface.viewport_width != next.width
        || surface.viewport_height != next.height
        || surface.pointer_events != next.pointer_events
        || surface.z_index != next.z_index
        || surface.group != next.group
        || surface.corner_radius != next.corner_radius
        || surface.game_view_style != next.style
        || surface.opacity != next.opacity;
    if changed {
        surface.viewport_x = next.x;
        surface.viewport_y = next.y;
        surface.viewport_width = next.width;
        surface.viewport_height = next.height;
        surface.pointer_events = next.pointer_events;
        surface.z_index = next.z_index;
        surface.group = next.group;
        surface.corner_radius = next.corner_radius;
        surface.game_view_style = next.style;
        surface.opacity = next.opacity;
    }
    changed
}

impl Drop for GameHost {
    fn drop(&mut self) {
        for &connection in self.surfaces.keys() {
            self.capture.revoke(connection);
        }
        if let Some(token) = self.session.active_token() {
            self.session.close(token);
        }
    }
}

/// Resolves CSS replaced-element sizing without changing the layout rectangle
/// used for guest resizing and input. Overflow becomes a source crop.
pub(crate) fn fit_surface_content(
    rectangle: Rect,
    source: Extent,
    style: Option<&GameViewStyle>,
    opacity: f32,
) -> SurfaceContent {
    use style::{
        properties::generated::longhands::object_fit::computed_value::T as ObjectFit,
        values::computed::CSSPixelLength,
    };

    let mut content = SurfaceContent::fill(rectangle);
    content.opacity = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if rectangle.is_empty() || source.is_empty() {
        return content;
    }
    let container_width = rectangle.width as f32;
    let container_height = rectangle.height as f32;
    let source_width = source.width as f32;
    let source_height = source.height as f32;
    let Some(style) = style else {
        return content;
    };
    let contain_scale = (container_width / source_width).min(container_height / source_height);
    let cover_scale = (container_width / source_width).max(container_height / source_height);
    let (object_width, object_height) = match style.object_fit {
        ObjectFit::Fill => (container_width, container_height),
        ObjectFit::Contain => (source_width * contain_scale, source_height * contain_scale),
        ObjectFit::Cover => (source_width * cover_scale, source_height * cover_scale),
        ObjectFit::None => (source_width, source_height),
        ObjectFit::ScaleDown
            if source_width <= container_width && source_height <= container_height =>
        {
            (source_width, source_height)
        }
        ObjectFit::ScaleDown => (source_width * contain_scale, source_height * contain_scale),
    };
    let x_offset = style
        .object_position
        .horizontal
        .resolve(CSSPixelLength::new(container_width - object_width))
        .px();
    let y_offset = style
        .object_position
        .vertical
        .resolve(CSSPixelLength::new(container_height - object_height))
        .px();
    let container_left = rectangle.x as f32;
    let container_top = rectangle.y as f32;
    let container_right = container_left + container_width;
    let container_bottom = container_top + container_height;
    let object_left = container_left + x_offset;
    let object_top = container_top + y_offset;
    let object_right = object_left + object_width;
    let object_bottom = object_top + object_height;
    let draw_left = object_left.max(container_left).min(container_right);
    let draw_top = object_top.max(container_top).min(container_bottom);
    let draw_right = object_right.min(container_right).max(container_left);
    let draw_bottom = object_bottom.min(container_bottom).max(container_top);
    content.destination = [draw_left, draw_top, draw_right, draw_bottom];
    if draw_right > draw_left && draw_bottom > draw_top {
        content.source = [
            ((draw_left - object_left) / object_width).clamp(0.0, 1.0),
            ((draw_top - object_top) / object_height).clamp(0.0, 1.0),
            ((draw_right - object_left) / object_width).clamp(0.0, 1.0),
            ((draw_bottom - object_top) / object_height).clamp(0.0, 1.0),
        ];
    }
    content
}

pub(crate) const fn rotated_extent(width: u32, height: u32, quarter_turns: u8) -> (u32, u32) {
    if quarter_turns % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    }
}

const fn normalized_quarter_turns(rotation: i32) -> u8 {
    rotation.rem_euclid(4) as u8
}

fn normalize_pointer_transform(
    transform: PresentationTransform,
    width: u32,
    height: u32,
) -> PresentationTransform {
    if transform.kind == PresentationMappingKind::Unavailable && width != 0 && height != 0 {
        PresentationTransform::axis_aligned(0, 0, width, height)
    } else {
        transform
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn pointer_transform_falls_back_to_the_full_presentation() {
        assert_eq!(
            normalize_pointer_transform(PresentationTransform::UNAVAILABLE, 854, 480),
            PresentationTransform::axis_aligned(0, 0, 854, 480)
        );
        let transform = PresentationTransform::axis_aligned(8, 9, 640, 360);
        assert_eq!(normalize_pointer_transform(transform, 854, 480), transform);
    }

    #[test]
    fn rotations_normalize_and_swap_the_presented_extent() {
        assert_eq!(normalized_quarter_turns(-1), 3);
        assert_eq!(normalized_quarter_turns(5), 1);
        assert_eq!(rotated_extent(854, 480, 0), (854, 480));
        assert_eq!(rotated_extent(854, 480, 1), (480, 854));
        assert_eq!(rotated_extent(854, 480, 2), (854, 480));
        assert_eq!(rotated_extent(854, 480, 3), (480, 854));
    }
}
