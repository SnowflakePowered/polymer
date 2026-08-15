//! Native emulator/capture state owned by the game-host subsystem.
//!
//! Validated launch-plan adaptation, process lifecycle, and captured-surface
//! models are contained here; the shell coordinator only forwards commands.

mod chrome;

use crate::{
    background_document::GameViewStyle,
    background_dom::{BackgroundFrame, TrackedSurfaceLayout},
    launch::AttachedLaunchPlan,
};
use chrome::ChromeTexture;
use snowglass_input::mouse::layout::Rect;
use snowglass_protocol::{AdapterLuid, PresentationMappingKind, PresentationTransform};
use snowglass_renderer::{
    CompositingPlan, D3d12Compositor, Extent, PlanLayer, Renderer, SurfaceCapture,
    SurfaceCaptureEvent, SurfaceContent, SurfacePlacement, SurfacePointerSender,
};
use std::error::Error;
use std::path::{Component, Path};
use std::time::Duration;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, atomic::AtomicBool},
};
use vello::wgpu;
use vello::{AaConfig, AaSupport, RenderParams, Renderer as VelloRenderer, RendererOptions};
use windows::core::IUnknown;

/// Vello GPU state owned by the native game host.
pub(crate) struct BackgroundRasterizer {
    renderer: VelloRenderer,
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_luid: AdapterLuid,
    targets: Vec<Option<ChromeTexture>>,
}

fn background_instance_options() -> wgpu::InstanceDescriptor {
    let mut options = wgpu::InstanceDescriptor::new_without_display_handle();
    options.backends = wgpu::Backends::DX12;
    // `Auto` may select a dxcompiler.dll without its matching DXIL.dll. The
    // background chrome does not need Shader Model 6, so use the in-box FXC.
    options.backend_options.dx12.shader_compiler = wgpu::Dx12Compiler::Fxc;
    options
}

fn adapter_luid(adapter: &wgpu::Adapter) -> Option<AdapterLuid> {
    // SAFETY: the HAL adapter is borrowed only to read its immutable DXGI
    // description.
    let adapter = unsafe { adapter.as_hal::<wgpu_hal::api::Dx12>()? };
    let description = unsafe { adapter.raw_adapter().GetDesc2().ok()? };
    Some(AdapterLuid {
        low_part: description.AdapterLuid.LowPart,
        high_part: description.AdapterLuid.HighPart,
    })
}

impl BackgroundRasterizer {
    async fn from_adapter(adapter: &wgpu::Adapter) -> Result<Self, Box<dyn Error>> {
        let adapter_info = adapter.get_info();
        let use_cpu_stages = matches!(
            adapter_info.device_type,
            wgpu::DeviceType::VirtualGpu | wgpu::DeviceType::Cpu
        );
        if use_cpu_stages {
            tracing::info!(
                adapter = %adapter_info.name,
                "snowglass: using Vello CPU geometry stages on a virtual/software D3D12 adapter"
            );
        }
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("snowglass background chrome"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::default(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::default(),
            })
            .await
            .map_err(|error| format!("background chrome could not open a D3D12 device: {error}"))?;
        let renderer = VelloRenderer::new(
            &device,
            RendererOptions {
                // The final fine raster still writes the compositor-owned GPU
                // texture. Only Vello's compute-heavy geometry stages move to
                // the CPU on virtual adapters whose WARP path cannot run them.
                use_cpu: use_cpu_stages,
                num_init_threads: None,
                antialiasing_support: AaSupport::area_only(),
                pipeline_cache: None,
            },
        )
        .map_err(|error| format!("background chrome could not start Vello: {error}"))?;
        let adapter_luid = adapter_luid(adapter)
            .ok_or("the selected D3D12 adapter did not expose an adapter LUID")?;
        Ok(Self {
            renderer,
            device,
            queue,
            adapter_luid,
            targets: Vec::new(),
        })
    }
    /// Prepares the initial adapter before Chromium starts. A later foreign
    /// adapter causes the complete RenderDevice to be recreated on demand.
    pub async fn new() -> Result<Self, Box<dyn Error>> {
        Self::new_for_adapter(None).await
    }

    async fn new_for_adapter(desired: Option<AdapterLuid>) -> Result<Self, Box<dyn Error>> {
        let instance = wgpu::Instance::new(background_instance_options());
        let adapter = if let Some(desired) = desired {
            let mut selected = None;
            let mut available = Vec::new();
            for adapter in instance.enumerate_adapters(wgpu::Backends::DX12).await {
                let luid = adapter_luid(&adapter);
                available.push(format!("{} ({luid:?})", adapter.get_info().name));
                if luid == Some(desired) {
                    selected = Some(adapter);
                    break;
                }
            }
            selected.ok_or_else(|| {
                format!(
                    "background chrome could not find requested D3D12 adapter {desired:?}; available: {}",
                    available.join(", ")
                )
            })?
        } else {
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
                .map_err(|error| format!("background chrome found no D3D12 adapter: {error}"))?
        };
        Self::from_adapter(&adapter).await
    }

    pub const fn adapter_luid(&self) -> AdapterLuid {
        self.adapter_luid
    }

    fn retained_device_context(
        &self,
    ) -> Result<(*mut std::ffi::c_void, *mut std::ffi::c_void), Box<dyn Error>> {
        use windows_wgpu::core::Interface as _;
        // SAFETY: the returned COM clones each transfer one owned reference to
        // the compositor constructor.
        unsafe {
            let hal = self
                .device
                .as_hal::<wgpu_hal::api::Dx12>()
                .ok_or("the background renderer is not using D3D12")?;
            Ok((
                hal.raw_device().clone().into_raw(),
                hal.raw_queue().clone().into_raw(),
            ))
        }
    }

    fn rasterize_frame(
        &mut self,
        frame: &BackgroundFrame,
        mut band: impl FnMut(usize, &ChromeTexture),
    ) {
        if !frame.dirty {
            return;
        }
        let rasterized = std::time::Instant::now();
        let parameters = RenderParams {
            base_color: vello::peniko::Color::TRANSPARENT,
            width: frame.width,
            height: frame.height,
            antialiasing_method: AaConfig::Area,
        };
        let mut drawn = Vec::with_capacity(frame.chrome_bands.len());
        for (index, scene) in frame.chrome_bands.iter().enumerate() {
            if self.targets.len() <= index {
                self.targets.resize_with(index + 1, || None);
            }
            let target = match &mut self.targets[index] {
                Some(target) if (target.width, target.height) == (frame.width, frame.height) => {
                    target
                }
                slot => match ChromeTexture::new(&self.device, frame.width, frame.height) {
                    Ok(target) => slot.insert(target),
                    Err(error) => {
                        tracing::error!(
                            "snowglass: background chrome could not make band {index}: {error}"
                        );
                        continue;
                    }
                },
            };
            if let Err(error) = self.renderer.render_to_texture(
                &self.device,
                &self.queue,
                scene,
                &target.view,
                &parameters,
            ) {
                tracing::error!("snowglass: rasterizing background band {index} failed: {error}");
                continue;
            }
            drawn.push(index);
        }
        if let Err(error) = self.device.poll(wgpu::PollType::wait_indefinitely()) {
            tracing::error!("snowglass: waiting for background chrome failed: {error}");
        }
        for index in drawn {
            if let Some(Some(target)) = self.targets.get(index) {
                band(index, target);
            }
        }
        if std::env::var_os("SNOWGLASS_GAME_LAYER_TIMING").is_some() {
            tracing::error!(
                "snowglass: background chrome rasterized {}x{} in {:.2}ms over {} band(s)",
                frame.width,
                frame.height,
                rasterized.elapsed().as_secs_f64() * 1000.0,
                frame.chrome_bands.len(),
            );
        }
    }
}

/// All renderer state local to one D3D12 adapter. Replacing this value moves
/// composition, Vello, chrome, and foreign imports as one device domain.
struct RenderDevice {
    compositor: D3d12Compositor,
    background: BackgroundRasterizer,
}

impl RenderDevice {
    fn new(
        window: &impl snowglass_renderer::RenderWindow,
        background: BackgroundRasterizer,
    ) -> Result<Self, Box<dyn Error>> {
        let adapter = background.adapter_luid();
        let (device, queue) = background.retained_device_context()?;
        // SAFETY: the background context supplied retained references to its
        // exact D3D12 device and presentation queue.
        let compositor = unsafe {
            D3d12Compositor::new_for_shared_composition_from_external(
                window, adapter, device, queue,
            )?
        };
        Ok(Self {
            compositor,
            background,
        })
    }

    fn recreate(
        window: &impl snowglass_renderer::RenderWindow,
        adapter: AdapterLuid,
    ) -> Result<Self, Box<dyn Error>> {
        let background = pollster::block_on(BackgroundRasterizer::new_for_adapter(Some(adapter)))?;
        Self::new(window, background)
    }

    fn adapter_luid(&self) -> AdapterLuid {
        self.compositor.adapter_luid()
    }

    fn render_background(&mut self, frame: &BackgroundFrame) -> Result<usize, String> {
        self.compositor
            .synchronize_chrome_update()
            .map_err(|error| error.to_string())?;
        let mut bands = 0;
        self.background.rasterize_frame(frame, |band, chrome| {
            bands = band + 1;
            // SAFETY: this retained resource belongs to the exact device and
            // queue used by the compositor in this RenderDevice.
            unsafe {
                self.compositor.set_chrome_layer_resource(
                    band as u64,
                    chrome.retained_abi_reference(),
                    chrome.width,
                    chrome.height,
                );
            }
        });
        Ok(bands)
    }
}

#[derive(Debug)]
pub(crate) enum GameHostCommand {
    Emulator(crate::background_runtime::EmulatorCommand),
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
    corner_radius: f32,
    game_view_style: Option<GameViewStyle>,
    opacity: f32,
}

struct ActiveEmulator {
    token: crate::background_runtime::InstanceToken,
    runtime: snowglass_runtime::EmulatorInstance,
    options: AttachedLaunchPlan,
    webrtc: Option<snowglass_renderer::webrtc::Publisher>,
    audio: snowglass_runtime::AudioState,
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
    Connected(AcceptedSurface),
    Frame {
        connection: u64,
        frame: snowglass_protocol::FrameReady,
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
    pub token: crate::background_runtime::InstanceToken,
    pub connections: Vec<u64>,
}

pub(crate) struct GameSurfaceTarget {
    pub connection: u64,
    pub index: u32,
    pub width: u32,
    pub height: u32,
}

pub(crate) struct GameHost {
    active: Option<ActiveEmulator>,
    capture: SurfaceCapture,
    pointer: SurfacePointerSender,
    wake_pending: Arc<AtomicBool>,
    dirty: Arc<AtomicBool>,
    work_pending: bool,
    surfaces: HashMap<u64, CapturedSurface>,
    /// One adapter-local device domain shared by native chrome, composition,
    /// and every imported captured stream.
    render_device: RenderDevice,
    composition_content_dirty: bool,
    compositor_dirty: bool,
    chrome_bands: Vec<usize>,
    chrome_dirty: bool,
    background_refresh_pending: bool,
}

impl GameHost {
    pub fn new(
        window: &impl snowglass_renderer::RenderWindow,
        rasterizer: BackgroundRasterizer,
    ) -> Result<Self, Box<dyn Error>> {
        let capture = SurfaceCapture::default();
        let pointer = capture.pointer_sender();
        let render_device = RenderDevice::new(window, rasterizer)?;
        Ok(Self {
            active: None,
            capture,
            pointer,
            wake_pending: Arc::new(AtomicBool::new(false)),
            dirty: Arc::new(AtomicBool::new(false)),
            work_pending: false,
            surfaces: HashMap::new(),
            render_device,
            composition_content_dirty: false,
            compositor_dirty: true,
            chrome_bands: Vec::new(),
            chrome_dirty: false,
            background_refresh_pending: false,
        })
    }

    pub fn composition_content(&self) -> Result<IUnknown, Box<dyn Error>> {
        self.render_device
            .compositor
            .composition_content()
            .map_err(Into::into)
    }

    /// Publishes a replacement swapchain to the window visual slot exactly
    /// once, including when the capture offer that prompted the adapter switch
    /// is subsequently rejected.
    pub fn take_composition_content_update(&mut self) -> Result<Option<IUnknown>, Box<dyn Error>> {
        if !self.composition_content_dirty {
            return Ok(None);
        }
        let content = self.composition_content()?;
        self.composition_content_dirty = false;
        Ok(Some(content))
    }

    pub fn is_active(&self, token: crate::background_runtime::InstanceToken) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.token == token)
    }

    pub fn active_token(&self) -> Option<crate::background_runtime::InstanceToken> {
        self.active.as_ref().map(|active| active.token)
    }

    pub fn has_active_game(&self) -> bool {
        self.active.is_some()
    }

    pub fn has_connected_surfaces(&self) -> bool {
        !self.surfaces.is_empty()
    }

    pub fn gamepad_mapping(&self) -> &[crate::input::GamepadMapping] {
        self.active
            .as_ref()
            .map(|active| active.options.gamepad_mapping.as_slice())
            .unwrap_or_default()
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
            let received_surfaces = Renderer::received_surfaces(&self.render_device.compositor)
                .into_iter()
                .filter(|received| received.connection == connection)
                .collect::<Vec<_>>();
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

    pub fn request_background_refresh(&mut self) {
        self.background_refresh_pending = true;
    }

    pub fn take_background_refresh_request(&mut self) -> bool {
        std::mem::take(&mut self.background_refresh_pending)
    }

    pub fn clear_background_refresh_request(&mut self) {
        self.background_refresh_pending = false;
    }

    pub fn resize_compositor(
        &mut self,
        window: &impl snowglass_renderer::RenderWindow,
    ) -> Result<bool, String> {
        let changed = Renderer::recreate_swapchain(&mut self.render_device.compositor, window)
            .map_err(|error| error.to_string())?;
        self.compositor_dirty |= changed;
        Ok(changed)
    }

    pub fn launch(
        &mut self,
        token: crate::background_runtime::InstanceToken,
        options: AttachedLaunchPlan,
        session: String,
        wake_window: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<(), String> {
        if self.active.is_some() {
            return Err("another emulator instance is active".to_owned());
        }
        let artifact_root = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            .ok_or_else(|| "shell executable has no parent directory".to_owned())?;
        if let Some(configuration) = &options.generated_configuration {
            install_generated_configuration(configuration)?;
        }
        let command = std::iter::once(file_url_or_path(&options.command)?)
            .chain(options.arguments.iter().cloned())
            .collect();
        let mut gamepad_layouts = options.gamepad_mapping.iter().collect::<Vec<_>>();
        gamepad_layouts.sort_by_key(|mapping| mapping.slot);
        let config = snowglass_runtime::EmulatorConfig {
            session,
            command,
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
            graph_tracking: false,
            force_keyed_mutex: false,
            gamepad_layouts: gamepad_layouts
                .into_iter()
                .map(|mapping| mapping.layout.clone())
                .collect(),
        };
        let webrtc_targets = options
            .surfaces
            .iter()
            .filter(|surface| surface.webrtc)
            .map(|surface| surface.index)
            .collect::<Vec<_>>();
        let publisher = if webrtc_targets.is_empty() {
            None
        } else {
            let bind = options
                .webrtc_bind
                .as_deref()
                .unwrap_or("127.0.0.1:8080")
                .parse()
                .map_err(|error| format!("invalid WebRTC bind address: {error}"))?;
            Some(
                snowglass_renderer::webrtc::Publisher::start(bind, &webrtc_targets)
                    .map_err(|error| error.to_string())?,
            )
        };
        let dirty = self.dirty.clone();
        let wake_pending = self.wake_pending.clone();
        let runtime = snowglass_runtime::EmulatorInstance::spawn_with_waker(config, move || {
            dirty.store(true, std::sync::atomic::Ordering::Release);
            if !wake_pending.swap(true, std::sync::atomic::Ordering::AcqRel) && !wake_window() {
                wake_pending.store(false, std::sync::atomic::Ordering::Release);
            }
        })
        .map_err(|error| error.to_string())?;
        self.capture.attach(&runtime);
        let audio = runtime.audio_state();
        self.active = Some(ActiveEmulator {
            token,
            runtime,
            options,
            webrtc: publisher,
            audio,
        });
        Ok(())
    }

    pub fn input_endpoint(&self) -> Option<crate::input::GameInputEndpoint> {
        self.active.as_ref().map(|active| {
            crate::input::GameInputEndpoint::new(
                active.runtime.protocol_input_sender(),
                active.runtime.gamepad_input(),
            )
        })
    }

    pub fn request_close(&mut self, token: crate::background_runtime::InstanceToken) -> bool {
        let Some(active) = self.active.as_mut().filter(|active| active.token == token) else {
            return false;
        };
        active.runtime.close();
        self.capture.detach();
        true
    }

    pub fn set_volume(&self, token: crate::background_runtime::InstanceToken, volume: f64) -> bool {
        let Some(volume) = snowglass_runtime::NormalizedVolume::new(volume as f32) else {
            return false;
        };
        let Some(active) = self.active.as_ref().filter(|active| active.token == token) else {
            return false;
        };
        active.runtime.set_volume(volume);
        true
    }

    pub fn set_muted(&self, token: crate::background_runtime::InstanceToken, muted: bool) -> bool {
        let Some(active) = self.active.as_ref().filter(|active| active.token == token) else {
            return false;
        };
        active.runtime.set_muted(muted);
        true
    }

    pub fn audio_change(
        &mut self,
        force: bool,
    ) -> Option<(
        crate::background_runtime::InstanceToken,
        snowglass_runtime::AudioState,
    )> {
        let active = self.active.as_mut()?;
        let audio = active.runtime.audio_state();
        if !force && active.audio == audio {
            return None;
        }
        active.audio = audio;
        Some((active.token, audio))
    }

    pub fn has_exited(&self) -> bool {
        self.active.as_ref().is_some_and(|active| {
            matches!(
                active.runtime.state(),
                snowglass_runtime::InstanceState::Exited(_)
            )
        })
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
        Some(match self.capture.try_recv().ok()? {
            SurfaceCaptureEvent::Offered(offer) => {
                let connection = offer.connection_id();
                match self.accept_surface_offer(window, offer) {
                    Ok(accepted) => GameHostCaptureEvent::Connected(accepted),
                    Err(error) => GameHostCaptureEvent::Rejected {
                        connection,
                        error: error.to_string(),
                    },
                }
            }
            SurfaceCaptureEvent::Frame { connection, update } => {
                let (frame, dimensions_changed) = self.update_surface_frame(connection, update)?;
                GameHostCaptureEvent::Frame {
                    connection,
                    frame,
                    dimensions_changed,
                }
            }
            SurfaceCaptureEvent::Removed { connection, .. } => GameHostCaptureEvent::Removed {
                connection,
                layout_changed: self.remove_surface(connection),
            },
        })
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
        let desired_adapter = offer.stream().adapter_luid;
        let recreated = self.ensure_render_device(window, desired_adapter)?;
        let connection = offer.connection_id();
        let surface_id = offer.stream().surface_id.0;
        let replaced_connection = (!recreated)
            .then(|| offer.replaced_connection_id())
            .flatten();
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
                    corner_radius: 0.0,
                    game_view_style: None,
                    opacity: 1.0,
                },
                target_index,
                quarter_turns,
            )
        };
        let backend = format!("{:?}", offer.stream().image_kind);
        Renderer::import_surface_offer(&mut self.render_device.compositor, &offer, quarter_turns)?;
        if let Err(error) = self.capture.accept(offer) {
            let _ = Renderer::disconnect_stream(&mut self.render_device.compositor, connection);
            return Err(error.into());
        }

        self.surfaces.insert(connection, surface);
        if let Some(previous) = replaced_connection {
            self.surfaces.remove(&previous);
            let _ = Renderer::disconnect_stream(&mut self.render_device.compositor, previous);
            if let Some(publisher) = self
                .active
                .as_ref()
                .and_then(|active| active.webrtc.as_ref())
            {
                publisher.remove_stream(previous);
            }
        }
        self.compositor_dirty = true;
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

    fn ensure_render_device(
        &mut self,
        window: &impl snowglass_renderer::RenderWindow,
        desired: AdapterLuid,
    ) -> Result<bool, Box<dyn Error>> {
        if self.render_device.adapter_luid() == desired {
            return Ok(false);
        }

        // Construct the complete replacement before disturbing the active
        // device domain. A failure leaves the old compositor usable.
        let replacement = RenderDevice::recreate(window, desired)?;
        self.render_device = replacement;
        let stale_connections = self.surfaces.keys().copied().collect::<Vec<_>>();
        for connection in stale_connections {
            self.capture.revoke(connection);
            if let Some(publisher) = self
                .active
                .as_ref()
                .and_then(|active| active.webrtc.as_ref())
            {
                publisher.remove_stream(connection);
            }
        }
        self.surfaces.clear();
        self.composition_content_dirty = true;
        self.compositor_dirty = true;
        self.chrome_bands.clear();
        self.chrome_dirty = true;
        self.background_refresh_pending = true;
        Ok(true)
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
        Renderer::set_latest(&mut self.render_device.compositor, connection, frame);
        Some((frame, dimensions_changed))
    }

    fn remove_surface(&mut self, connection: u64) -> bool {
        if let Some(publisher) = self
            .active
            .as_ref()
            .and_then(|active| active.webrtc.as_ref())
        {
            publisher.remove_stream(connection);
        }
        let removed = self.surfaces.remove(&connection).is_some();
        if removed {
            let _ = Renderer::disconnect_stream(&mut self.render_device.compositor, connection);
            self.compositor_dirty = true;
        }
        removed
    }

    pub fn finish_active_game(&mut self) -> Option<FinishedGame> {
        let active = self.active.take()?;
        let connections = self.surfaces.keys().copied().collect::<Vec<_>>();
        for &connection in &connections {
            if let Some(publisher) = &active.webrtc {
                publisher.remove_stream(connection);
            }
            self.capture.revoke(connection);
            let _ = Renderer::disconnect_stream(&mut self.render_device.compositor, connection);
        }
        self.capture.detach();
        self.surfaces.clear();
        self.compositor_dirty = true;
        Some(FinishedGame {
            token: active.token,
            connections,
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
        Renderer::has_pending_frame(&self.render_device.compositor)
            || self.compositor_dirty
            || self.chrome_dirty
    }

    pub fn compositing_plan(&self, width: u32, height: u32) -> CompositingPlan {
        let mut ordered = Renderer::received_surfaces(&self.render_device.compositor)
            .into_iter()
            .filter_map(|received| {
                let surface = self.surfaces.get(&received.connection)?;
                let rectangle = Rect::new(
                    surface.viewport_x,
                    surface.viewport_y,
                    surface.viewport_width,
                    surface.viewport_height,
                );
                let mut placement = SurfacePlacement::new(
                    received.connection,
                    received.surface_id,
                    rectangle,
                    surface.corner_radius,
                );
                placement.content = fit_surface_content(
                    rectangle,
                    received.image_extent,
                    surface.game_view_style.as_ref(),
                    surface.opacity,
                );
                Some((surface.z_index, PlanLayer::Surface(placement)))
            })
            .collect::<Vec<_>>();
        ordered.extend(self.chrome_bands.iter().map(|&band| {
            (
                2 * band as i32,
                PlanLayer::Chrome {
                    id: band as u64,
                    rectangle: Rect::new(0, 0, width, height),
                },
            )
        }));
        ordered.sort_by_key(|(z_index, _)| *z_index);
        CompositingPlan::layered(
            [0.0, 0.0, 0.0, 1.0],
            ordered.into_iter().map(|(_, layer)| layer).collect(),
        )
    }

    pub fn draw(
        &mut self,
        width: u32,
        height: u32,
        permit_wait: Duration,
    ) -> Result<Option<bool>, String> {
        let plan = self.compositing_plan(width, height);
        let compositor = &mut self.render_device.compositor;
        let carried_new_frame = Renderer::has_pending_frame(compositor);
        if !compositor.begin_frame_within(permit_wait) {
            return Ok(None);
        }
        compositor
            .present_with_webrtc(&plan, None)
            .map_err(|error| error.to_string())?;
        self.compositor_dirty = false;
        self.chrome_dirty = false;
        Ok(Some(carried_new_frame))
    }

    pub fn rasterize_background(&mut self, frame: &BackgroundFrame) -> Result<bool, String> {
        if !frame.dirty {
            return Ok(false);
        }
        let device = &mut self.render_device;
        let bands = device.render_background(frame)?;
        let previous_bands = self.chrome_bands.len();
        self.chrome_bands = (0..bands).collect();
        self.chrome_dirty = true;
        for band in bands..previous_bands {
            device.compositor.drop_chrome_layer(band as u64);
        }
        Ok(true)
    }

    pub fn clear_background(&mut self) {
        for band in self.chrome_bands.drain(..) {
            self.render_device.compositor.drop_chrome_layer(band as u64);
        }
        self.chrome_dirty = true;
        self.apply_tracked_layout(&[]);
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
            let rect = placement
                .clipping
                .map(|clip| placement.bounds.intersect(clip))
                .unwrap_or(placement.bounds);
            let next = SurfaceGeometry {
                x: rect.x0.round() as i32,
                y: rect.y0.round() as i32,
                width: rect.width().round().max(0.0) as u32,
                height: rect.height().round().max(0.0) as u32,
                pointer_events: true,
                z_index: 2 * placement.z_order as i32 + 1,
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
                    corner_radius: 0.0,
                    style: None,
                    opacity: 1.0,
                },
            );
        }
        self.compositor_dirty |= changed;
        changed
    }
}

fn hook_backend(hook: snowglass_pluginhost::GraphicsHook) -> snowglass_runtime::HookBackend {
    match hook {
        snowglass_pluginhost::GraphicsHook::Opengl => snowglass_runtime::HookBackend::OpenGl,
        snowglass_pluginhost::GraphicsHook::Vulkan => snowglass_runtime::HookBackend::Vulkan,
        snowglass_pluginhost::GraphicsHook::D3d11 => snowglass_runtime::HookBackend::D3d11,
        snowglass_pluginhost::GraphicsHook::D3d12 => snowglass_runtime::HookBackend::D3d12,
    }
}

fn file_url_or_path(value: &str) -> Result<String, String> {
    if !value.starts_with("file:") {
        return Ok(value.to_owned());
    }
    let url = deno_core::ModuleSpecifier::parse(value)
        .map_err(|error| format!("invalid file URL '{value}': {error}"))?;
    url.to_file_path()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|()| format!("file URL is not a local path: {value}"))
}

fn install_generated_configuration(
    configuration: &crate::launch::GeneratedConfiguration,
) -> Result<(), String> {
    let root = std::path::PathBuf::from(file_url_or_path(&configuration.root)?);
    if !root.is_dir() {
        return Err(format!(
            "plugin config root does not exist: {}",
            root.display()
        ));
    }
    let root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve config root {}: {error}", root.display()))?;
    let mut written = 0usize;
    for (relative, bytes) in &configuration.files {
        let relative = Path::new(relative);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(format!(
                "plugin config path must stay beneath its root: {}",
                relative.display()
            ));
        }
        let target = root.join(relative);
        if target.exists() {
            let resolved_target = target.canonicalize().map_err(|error| {
                format!(
                    "cannot resolve existing config {}: {error}",
                    target.display()
                )
            })?;
            if !resolved_target.starts_with(&root) {
                return Err(format!(
                    "plugin config path escapes its root through a link: {}",
                    target.display()
                ));
            }
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "cannot create config directory {}: {error}",
                    parent.display()
                )
            })?;
            let resolved_parent = parent.canonicalize().map_err(|error| {
                format!(
                    "cannot resolve config directory {}: {error}",
                    parent.display()
                )
            })?;
            if !resolved_parent.starts_with(&root) {
                return Err(format!(
                    "plugin config path escapes its root through a link: {}",
                    target.display()
                ));
            }
        }
        std::fs::write(&target, bytes).map_err(|error| {
            format!(
                "cannot write generated config {}: {error}",
                target.display()
            )
        })?;
        written += 1;
    }
    tracing::info!(
        "snowglass: plugin configuration realized: {written} file(s) written under {}",
        root.display()
    );
    Ok(())
}

struct SurfaceGeometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    pointer_events: bool,
    z_index: i32,
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
        if let Some(active) = self.active.as_mut() {
            active.runtime.close();
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
    use std::collections::BTreeMap;
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
    fn background_renderer_uses_the_self_contained_d3d12_shader_compiler() {
        let options = background_instance_options();
        assert_eq!(options.backends, wgpu::Backends::DX12);
        assert!(matches!(
            options.backend_options.dx12.shader_compiler,
            wgpu::Dx12Compiler::Fxc
        ));
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

    #[test]
    fn game_host_installs_generated_configuration_at_launch_boundary() {
        let root = tempfile::tempdir().unwrap();
        let settings = root.path().join("settings.xml");
        std::fs::write(&settings, b"old").unwrap();
        let configuration = crate::launch::GeneratedConfiguration {
            root: root.path().to_string_lossy().into_owned(),
            files: BTreeMap::from([("settings.xml".to_owned(), b"generated".to_vec())]),
        };

        install_generated_configuration(&configuration).unwrap();

        assert_eq!(std::fs::read(settings).unwrap(), b"generated");
    }
}
