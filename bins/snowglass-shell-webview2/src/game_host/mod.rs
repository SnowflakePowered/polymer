//! Native emulator/capture state owned by the game-host subsystem.
//!
//! Validated launch-plan adaptation, process lifecycle, and captured-surface
//! models are contained here; the shell coordinator only forwards commands.

mod chrome;

use crate::{
    launch::AttachedLaunchPlan,
    shell_document::GameViewStyle,
    shell_dom::{ShellFrame, TrackedSurfaceLayout},
};
use chrome::{ChromeResource, ChromeTexture};
use parking_lot::Mutex;
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use snowglass_input::mouse::layout::Rect;
use snowglass_platform_windows::performance_counter;
use snowglass_protocol::{AdapterLuid, PresentationMappingKind, PresentationTransform};
use snowglass_renderer::{
    CompositingPlan, D3d12Compositor, Extent, PlanLayer, Renderer, SurfaceCapture,
    SurfaceCaptureEvent, SurfaceContent, SurfacePlacement, SurfacePointerSender,
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
use vello::wgpu;
use vello::{AaConfig, AaSupport, RenderParams, Renderer as VelloRenderer, RendererOptions};
use windows::core::{IUnknown, Interface};

/// Vello GPU state owned by the native game host.
pub(crate) struct ShellRasterizer {
    renderer: VelloRenderer,
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_luid: AdapterLuid,
    targets: Vec<Option<ChromeTexture>>,
    damage_targets: Vec<Option<ChromeTexture>>,
    /// The complete scene currently represented by each retained band texture.
    /// Comparing encodings is cheap relative to another Vello dispatch and, unlike
    /// damage-rectangle guesses, accounts for effects that cross band boundaries.
    rendered_scenes: Vec<Option<vello::Scene>>,
}

fn same_scene(left: &vello::Scene, right: &vello::Scene) -> bool {
    let left = left.encoding();
    let right = right.encoding();
    left.path_tags == right.path_tags
        && left.path_data == right.path_data
        && left.draw_tags == right.draw_tags
        && left.draw_data == right.draw_data
        && left.transforms == right.transforms
        && left.styles == right.styles
        && left.n_paths == right.n_paths
        && left.n_path_segments == right.n_path_segments
        && left.n_clips == right.n_clips
        && left.n_open_clips == right.n_open_clips
        && left.flags == right.flags
        && same_resources(&left.resources, &right.resources)
}

fn same_resources(left: &vello_encoding::Resources, right: &vello_encoding::Resources) -> bool {
    left.color_stops == right.color_stops
        && left.normalized_coords == right.normalized_coords
        && left.glyphs.len() == right.glyphs.len()
        && left.glyphs.iter().zip(&right.glyphs).all(|(left, right)| {
            left.id == right.id
                && left.x.to_bits() == right.x.to_bits()
                && left.y.to_bits() == right.y.to_bits()
        })
        && left.glyph_runs.len() == right.glyph_runs.len()
        && left
            .glyph_runs
            .iter()
            .zip(&right.glyph_runs)
            .all(|(left, right)| {
                let left_offsets = left.stream_offsets;
                let right_offsets = right.stream_offsets;
                left.font == right.font
                    && left.transform == right.transform
                    && left.glyph_transform == right.glyph_transform
                    && left.brush_transform == right.brush_transform
                    && left.font_size.to_bits() == right.font_size.to_bits()
                    && left.font_embolden.amount.xx.to_bits()
                        == right.font_embolden.amount.xx.to_bits()
                    && left.font_embolden.amount.yy.to_bits()
                        == right.font_embolden.amount.yy.to_bits()
                    && left.font_embolden.join == right.font_embolden.join
                    && left.font_embolden.miter_limit.to_bits()
                        == right.font_embolden.miter_limit.to_bits()
                    && left.font_embolden.tolerance.to_bits()
                        == right.font_embolden.tolerance.to_bits()
                    && left.hint == right.hint
                    && left.normalized_coords == right.normalized_coords
                    && left.style == right.style
                    && left.glyphs == right.glyphs
                    && left_offsets.path_tags == right_offsets.path_tags
                    && left_offsets.path_data == right_offsets.path_data
                    && left_offsets.draw_tags == right_offsets.draw_tags
                    && left_offsets.draw_data == right_offsets.draw_data
                    && left_offsets.transforms == right_offsets.transforms
                    && left_offsets.styles == right_offsets.styles
            })
        && left.patches.len() == right.patches.len()
        && left
            .patches
            .iter()
            .zip(&right.patches)
            .all(|(left, right)| match (left, right) {
                (
                    vello_encoding::Patch::Ramp {
                        draw_data_offset: left_offset,
                        stops: left_stops,
                        extend: left_extend,
                    },
                    vello_encoding::Patch::Ramp {
                        draw_data_offset: right_offset,
                        stops: right_stops,
                        extend: right_extend,
                    },
                ) => {
                    left_offset == right_offset
                        && left_stops == right_stops
                        && left_extend == right_extend
                }
                (
                    vello_encoding::Patch::GlyphRun { index: left },
                    vello_encoding::Patch::GlyphRun { index: right },
                ) => left == right,
                (
                    vello_encoding::Patch::Image {
                        image: left_image,
                        draw_data_offset: left_offset,
                    },
                    vello_encoding::Patch::Image {
                        image: right_image,
                        draw_data_offset: right_offset,
                    },
                ) => left_offset == right_offset && left_image == right_image,
                _ => false,
            })
}

fn shell_instance_options() -> wgpu::InstanceDescriptor {
    let mut options = wgpu::InstanceDescriptor::new_without_display_handle();
    options.backends = wgpu::Backends::DX12;
    // `Auto` may select a dxcompiler.dll without its matching DXIL.dll. The
    // Shell chrome does not need Shader Model 6, so use the in-box FXC.
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

impl ShellRasterizer {
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
                label: Some("snowglass shell chrome"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::default(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::default(),
            })
            .await
            .map_err(|error| format!("shell chrome could not open a D3D12 device: {error}"))?;
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
        .map_err(|error| format!("shell chrome could not start Vello: {error}"))?;
        let adapter_luid = adapter_luid(adapter)
            .ok_or("the selected D3D12 adapter did not expose an adapter LUID")?;
        Ok(Self {
            renderer,
            device,
            queue,
            adapter_luid,
            targets: Vec::new(),
            damage_targets: Vec::new(),
            rendered_scenes: Vec::new(),
        })
    }
    /// Prepares the initial adapter before the shell document starts. A later foreign
    /// adapter causes the complete RenderDevice to be recreated on demand.
    pub async fn new() -> Result<Self, Box<dyn Error>> {
        Self::new_for_adapter(None).await
    }

    async fn new_for_adapter(desired: Option<AdapterLuid>) -> Result<Self, Box<dyn Error>> {
        let instance = wgpu::Instance::new(shell_instance_options());
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
                    "shell chrome could not find requested D3D12 adapter {desired:?}; available: {}",
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
                .map_err(|error| format!("shell chrome found no D3D12 adapter: {error}"))?
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
                .ok_or("the shell renderer is not using D3D12")?;
            Ok((
                hal.raw_device().clone().into_raw(),
                hal.raw_queue().clone().into_raw(),
            ))
        }
    }

    fn rasterize_frame(&mut self, frame: &ShellFrame, mut band: impl FnMut(usize, &ChromeTexture)) {
        let rasterized = std::time::Instant::now();
        let parameters = RenderParams {
            base_color: vello::peniko::Color::TRANSPARENT,
            width: frame.width,
            height: frame.height,
            antialiasing_method: AaConfig::Area,
        };
        let mut drawn = Vec::with_capacity(frame.chrome_bands.len());
        let mut reused = Vec::with_capacity(frame.chrome_bands.len());
        let mut band_timings = Vec::with_capacity(frame.chrome_bands.len());
        for (index, scene) in frame.chrome_bands.iter().enumerate() {
            let target_is_current_size = self.targets.get(index).is_some_and(|target| {
                target.as_ref().is_some_and(|target| {
                    (target.width, target.height) == (frame.width, frame.height)
                })
            });
            if target_is_current_size
                && self
                    .rendered_scenes
                    .get(index)
                    .and_then(Option::as_ref)
                    .is_some_and(|rendered| same_scene(rendered, scene))
            {
                reused.push(index);
                continue;
            }
            let band_started = std::time::Instant::now();
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
                            "snowglass: shell chrome could not make band {index}: {error}"
                        );
                        continue;
                    }
                },
            };
            let damage = frame.damage.and_then(|damage| {
                let bounds =
                    kurbo::Rect::new(0.0, 0.0, f64::from(frame.width), f64::from(frame.height));
                let damage = damage.intersect(bounds);
                (damage.width() > 0.0 && damage.height() > 0.0).then_some(damage)
            });
            if let Some(damage) = damage {
                let x = damage.x0.floor().max(0.0) as u32;
                let y = damage.y0.floor().max(0.0) as u32;
                let right = damage.x1.ceil().min(f64::from(frame.width)) as u32;
                let bottom = damage.y1.ceil().min(f64::from(frame.height)) as u32;
                let (width, height) = (right.saturating_sub(x), bottom.saturating_sub(y));
                if self.damage_targets.len() <= index {
                    self.damage_targets.resize_with(index + 1, || None);
                }
                let scratch = match &mut self.damage_targets[index] {
                    Some(scratch)
                        if (scratch.width, scratch.height) == (frame.width, frame.height) =>
                    {
                        scratch
                    }
                    slot => match ChromeTexture::new(&self.device, frame.width, frame.height) {
                        Ok(scratch) => slot.insert(scratch),
                        Err(error) => {
                            tracing::error!(
                                "snowglass: shell chrome could not make damage target {index}: {error}"
                            );
                            continue;
                        }
                    },
                };
                let mut local_scene = vello::Scene::new();
                local_scene.append(
                    scene,
                    Some(kurbo::Affine::translate((-(x as f64), -(y as f64)))),
                );
                let damage_parameters = RenderParams {
                    width,
                    height,
                    ..parameters
                };
                if let Err(error) = self.renderer.render_to_texture(
                    &self.device,
                    &self.queue,
                    &local_scene,
                    &scratch.view,
                    &damage_parameters,
                ) {
                    tracing::error!(
                        "snowglass: rasterizing shell band {index} damage failed: {error}"
                    );
                    continue;
                }
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("snowglass shell damage copy"),
                        });
                encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &scratch.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &target.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x, y, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );
                self.queue.submit(Some(encoder.finish()));
            } else if let Err(error) = self.renderer.render_to_texture(
                &self.device,
                &self.queue,
                scene,
                &target.view,
                &parameters,
            ) {
                tracing::error!("snowglass: rasterizing shell band {index} failed: {error}");
                continue;
            }
            if self.rendered_scenes.len() <= index {
                self.rendered_scenes.resize_with(index + 1, || None);
            }
            self.rendered_scenes[index] = Some(scene.clone());
            band_timings.push((index, band_started.elapsed()));
            drawn.push(index);
        }
        // Vello and the compositor intentionally share this exact D3D12
        // command queue. The compositor's sampling submission is therefore
        // ordered after these raster submissions without a CPU wait. Blocking
        // here serialized every DOM transform update with the GPU and
        // starved both input delivery and guest presentation while dragging.
        self.device.poll(wgpu::PollType::Poll).ok();
        let drawn_count = drawn.len();
        for index in drawn {
            if let Some(Some(target)) = self.targets.get(index) {
                band(index, target);
            }
        }
        if std::env::var_os("SNOWGLASS_GAME_LAYER_TIMING").is_some() {
            tracing::error!(
                "snowglass: shell chrome rasterized {}x{} in {:.2}ms over {}/{} band(s){}",
                frame.width,
                frame.height,
                rasterized.elapsed().as_secs_f64() * 1000.0,
                drawn_count,
                frame.chrome_bands.len(),
                frame.damage.map_or(String::new(), |damage| format!(
                    " damage {:.0}x{:.0}",
                    damage.width(),
                    damage.height()
                )),
            );
            tracing::error!(
                "snowglass: shell band raster timings {}",
                band_timings
                    .iter()
                    .map(|(band, elapsed)| format!(
                        "{band}:{:.2}ms",
                        elapsed.as_secs_f64() * 1000.0
                    ))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            if !reused.is_empty() {
                tracing::error!(
                    "snowglass: shell reused unchanged band(s) {}",
                    reused
                        .iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
        }
    }
}

struct ShellRasterResult {
    band_count: usize,
    updates: Vec<(usize, ChromeResource)>,
}

#[derive(Default)]
struct ShellRasterWorkerState {
    pending: Option<ShellFrame>,
    busy: bool,
    ready: usize,
    stopping: bool,
}

/// Latest-only Vello worker. DOM/layout stays on the UI thread, but shader
/// dispatch, command encoding, and full-frame band raster no longer delay
/// capture draining or a guest Present.
struct ShellRasterWorker {
    state: Arc<Mutex<ShellRasterWorkerState>>,
    results: mpsc::Receiver<ShellRasterResult>,
    worker: Option<thread::JoinHandle<()>>,
}

impl ShellRasterWorker {
    fn spawn(mut rasterizer: ShellRasterizer) -> Self {
        let state = Arc::new(Mutex::new(ShellRasterWorkerState::default()));
        let worker_state = Arc::clone(&state);
        let (sender, results) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("snowglass-blitz-raster".to_owned())
            .spawn(move || {
                loop {
                    let frame = {
                        let mut state = worker_state.lock();
                        if state.stopping {
                            break;
                        }
                        state.pending.take()
                    };
                    let Some(frame) = frame else {
                        thread::park();
                        continue;
                    };
                    let mut updates = Vec::new();
                    rasterizer.rasterize_frame(&frame, |band, texture| {
                        updates.push((band, texture.retained_resource()));
                    });
                    {
                        let mut state = worker_state.lock();
                        state.ready += 1;
                        if state.pending.is_none() {
                            state.busy = false;
                        }
                    }
                    if sender
                        .send(ShellRasterResult {
                            band_count: frame.chrome_bands.len(),
                            updates,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("failed to create the Blitz raster worker");
        Self {
            state,
            results,
            worker: Some(worker),
        }
    }

    fn submit(&self, frame: ShellFrame) {
        let mut state = self.state.lock();
        state.pending = Some(frame);
        state.busy = true;
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
    }

    fn try_result(&self) -> Option<ShellRasterResult> {
        let result = self.results.try_recv().ok()?;
        let mut state = self.state.lock();
        state.ready = state.ready.saturating_sub(1);
        Some(result)
    }

    fn is_pending(&self) -> bool {
        let state = self.state.lock();
        state.busy || state.pending.is_some() || state.ready != 0
    }
}

impl Drop for ShellRasterWorker {
    fn drop(&mut self) {
        {
            let mut state = self.state.lock();
            state.stopping = true;
            state.pending = None;
        }
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

/// All renderer state local to one D3D12 adapter. Replacing this value moves
/// composition, Vello, chrome, and foreign imports as one device domain.
struct RenderDevice {
    adapter: AdapterLuid,
    chrome_device: IUnknown,
    chrome_queue: IUnknown,
    shell_worker: ShellRasterWorker,
    chrome_compositors: Vec<D3d12Compositor>,
    surface_compositors: HashMap<u64, SurfaceCompositor>,
}

struct SurfaceCompositor {
    compositor: Option<D3d12Compositor>,
    content: Option<IUnknown>,
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
    fn new(
        window: &impl snowglass_renderer::RenderWindow,
        shell: ShellRasterizer,
    ) -> Result<Self, Box<dyn Error>> {
        let adapter = shell.adapter_luid();
        let (device, queue) = shell.retained_device_context()?;
        // SAFETY: retained_device_context transfers one owned reference for
        // each pointer. These anchors mint a fresh retained pair whenever a
        // new band compositor is required.
        let chrome_device = unsafe { IUnknown::from_raw(device) };
        let chrome_queue = unsafe { IUnknown::from_raw(queue) };
        // SAFETY: the shell context supplied retained references to its exact
        // D3D12 device and queue. Band zero is always present, even for an
        // empty document, and is the stable adapter anchor for WebRTC.
        let chrome = unsafe {
            D3d12Compositor::new_for_transparent_composition_from_external(
                window,
                adapter,
                chrome_device.clone().into_raw(),
                chrome_queue.clone().into_raw(),
            )?
        };
        Ok(Self {
            adapter,
            chrome_device,
            chrome_queue,
            shell_worker: ShellRasterWorker::spawn(shell),
            chrome_compositors: vec![chrome],
            surface_compositors: HashMap::new(),
        })
    }

    fn recreate(
        window: &impl snowglass_renderer::RenderWindow,
        adapter: AdapterLuid,
    ) -> Result<Self, Box<dyn Error>> {
        let shell = pollster::block_on(ShellRasterizer::new_for_adapter(Some(adapter)))?;
        Self::new(window, shell)
    }

    fn adapter_luid(&self) -> AdapterLuid {
        self.adapter
    }

    fn ensure_chrome_bands(
        &mut self,
        window: &impl snowglass_renderer::RenderWindow,
        count: usize,
    ) -> Result<bool, String> {
        let mut changed = false;
        while self.chrome_compositors.len() < count.max(1) {
            // SAFETY: each call transfers its own retained references into a
            // new transparent composition swapchain on the shell device.
            let compositor = unsafe {
                D3d12Compositor::new_for_transparent_composition_from_external(
                    window,
                    self.adapter,
                    self.chrome_device.clone().into_raw(),
                    self.chrome_queue.clone().into_raw(),
                )
            }
            .map_err(|error| error.to_string())?;
            self.chrome_compositors.push(compositor);
            changed = true;
        }
        Ok(changed)
    }

    fn submit_shell(&self, frame: ShellFrame) {
        self.shell_worker.submit(frame);
    }

    fn apply_shell_result(
        &mut self,
        window: &impl snowglass_renderer::RenderWindow,
        result: ShellRasterResult,
    ) -> Result<(usize, Vec<usize>), String> {
        self.ensure_chrome_bands(window, result.band_count)?;
        let mut updated = Vec::with_capacity(result.updates.len());
        for (band, resource) in result.updates {
            let Some(compositor) = self.chrome_compositors.get_mut(band) else {
                continue;
            };
            // SAFETY: the worker retained this exact device-local resource;
            // queue submission order on the shared chrome queue makes the
            // completed Vello write visible before this compositor samples it.
            let (width, height) = (resource.width, resource.height);
            unsafe {
                compositor.set_chrome_layer_resource(
                    band as u64,
                    resource.into_abi_reference(),
                    width,
                    height,
                );
            }
            updated.push(band);
        }
        Ok((result.band_count, updated))
    }

    fn create_surface(
        &mut self,
        window: &impl snowglass_renderer::RenderWindow,
        connection: u64,
        width: u32,
        height: u32,
    ) -> Result<(), Box<dyn Error>> {
        let local = LocalRenderWindow {
            window,
            extent: Extent::new(width.max(1), height.max(1)),
        };
        let compositor =
            D3d12Compositor::new_for_stream_composition(&local, self.adapter, connection)?;
        self.surface_compositors.insert(
            connection,
            SurfaceCompositor {
                compositor: Some(compositor),
                content: None,
                worker: None,
                latest_frame: None,
                surface_id: 0,
                target_window: 0,
                quarter_turns: 0,
                flip_y: false,
                width: width.max(1),
                height: height.max(1),
            },
        );
        Ok(())
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
    corner_radius: f32,
    game_view_style: Option<GameViewStyle>,
    opacity: f32,
}

struct ActiveEmulator {
    token: crate::shell_runtime::InstanceToken,
    runtime: snowglass_runtime::EmulatorInstance,
    options: AttachedLaunchPlan,
    webrtc: Option<snowglass_renderer::webrtc::Publication>,
    audio: snowglass_runtime::AudioState,
    filesystem: Option<snowglass_vfs::MountedFilesystem>,
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
    surface_worker_events: mpsc::Receiver<SurfaceWorkerFrame>,
    surface_worker_sender: mpsc::Sender<SurfaceWorkerFrame>,
    surfaces: HashMap<u64, CapturedSurface>,
    /// One adapter-local device domain shared by native chrome, composition,
    /// and every imported captured stream.
    render_device: RenderDevice,
    composition_content_dirty: bool,
    chrome_bands: Vec<usize>,
    chrome_dirty: HashSet<usize>,
    shell_refresh_pending: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum CompositionLayerId {
    Band(usize),
    Surface(u64),
}

#[derive(Clone, Copy)]
pub(crate) enum CompositionLayerStretch {
    Fill,
    UniformToFill,
}

pub(crate) struct CompositionLayer {
    pub id: CompositionLayerId,
    pub content: IUnknown,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub opacity: f32,
    pub stretch: CompositionLayerStretch,
    pub horizontal_alignment: f32,
    pub vertical_alignment: f32,
}

pub(crate) struct CompositionContents {
    pub layers: Vec<CompositionLayer>,
}

impl GameHost {
    pub fn new(
        window: &impl snowglass_renderer::RenderWindow,
        rasterizer: ShellRasterizer,
    ) -> Result<Self, Box<dyn Error>> {
        let capture = SurfaceCapture::default();
        let pointer = capture.pointer_sender();
        let render_device = RenderDevice::new(window, rasterizer)?;
        let (surface_worker_sender, surface_worker_events) = mpsc::channel();
        Ok(Self {
            active: None,
            capture,
            pointer,
            wake_pending: Arc::new(AtomicBool::new(false)),
            dirty: Arc::new(AtomicBool::new(false)),
            work_pending: false,
            surface_worker_events,
            surface_worker_sender,
            surfaces: HashMap::new(),
            render_device,
            composition_content_dirty: false,
            chrome_bands: Vec::new(),
            chrome_dirty: HashSet::new(),
            shell_refresh_pending: false,
        })
    }

    pub fn composition_content(
        &self,
        width: u32,
        height: u32,
    ) -> Result<CompositionContents, Box<dyn Error>> {
        let mut ordered = Vec::new();
        for &band in &self.chrome_bands {
            let Some(compositor) = self.render_device.chrome_compositors.get(band) else {
                continue;
            };
            ordered.push((
                2 * band as i32,
                CompositionLayer {
                    id: CompositionLayerId::Band(band),
                    content: compositor.composition_content()?,
                    x: 0.0,
                    y: 0.0,
                    width: width as f32,
                    height: height as f32,
                    opacity: 1.0,
                    stretch: CompositionLayerStretch::Fill,
                    horizontal_alignment: 0.5,
                    vertical_alignment: 0.5,
                },
            ));
        }
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
            let cropped_width = 1.0 - (fitted.source[0] + (1.0 - fitted.source[2]));
            let cropped_height = 1.0 - (fitted.source[1] + (1.0 - fitted.source[3]));
            let horizontal_alignment = if cropped_width < 1.0 {
                (fitted.source[0] / (1.0 - cropped_width)).clamp(0.0, 1.0)
            } else {
                0.5
            };
            let vertical_alignment = if cropped_height < 1.0 {
                (fitted.source[1] / (1.0 - cropped_height)).clamp(0.0, 1.0)
            } else {
                0.5
            };
            ordered.push((
                surface.z_index,
                CompositionLayer {
                    id: CompositionLayerId::Surface(connection),
                    content: if let Some(content) = &output.content {
                        content.clone()
                    } else {
                        output
                            .compositor
                            .as_ref()
                            .ok_or("surface compositor has neither content nor an owner")?
                            .composition_content()?
                    },
                    x: fitted.destination[0],
                    y: fitted.destination[1],
                    width: (fitted.destination[2] - fitted.destination[0]).max(0.0),
                    height: (fitted.destination[3] - fitted.destination[1]).max(0.0),
                    opacity: fitted.opacity,
                    stretch: if fitted.source == [0.0, 0.0, 1.0, 1.0] {
                        CompositionLayerStretch::Fill
                    } else {
                        CompositionLayerStretch::UniformToFill
                    },
                    horizontal_alignment,
                    vertical_alignment,
                },
            ));
        }
        ordered.sort_by_key(|(z, _)| *z);
        Ok(CompositionContents {
            layers: ordered.into_iter().map(|(_, layer)| layer).collect(),
        })
    }

    /// Publishes a replacement swapchain to the window visual slot exactly
    /// once, including when the capture offer that prompted the adapter switch
    /// is subsequently rejected.
    pub fn take_composition_content_update(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<Option<CompositionContents>, Box<dyn Error>> {
        if !self.composition_content_dirty {
            return Ok(None);
        }
        let content = self.composition_content(width, height)?;
        self.composition_content_dirty = false;
        Ok(Some(content))
    }

    pub fn is_active(&self, token: crate::shell_runtime::InstanceToken) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.token == token)
    }

    pub fn active_token(&self) -> Option<crate::shell_runtime::InstanceToken> {
        self.active.as_ref().map(|active| active.token)
    }

    pub fn has_active_game(&self) -> bool {
        self.active.is_some()
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

    pub fn resize_compositor(
        &mut self,
        window: &impl snowglass_renderer::RenderWindow,
    ) -> Result<bool, String> {
        let mut changed = false;
        for (band, compositor) in self.render_device.chrome_compositors.iter_mut().enumerate() {
            if Renderer::recreate_swapchain(compositor, window)
                .map_err(|error| error.to_string())?
            {
                self.chrome_dirty.insert(band);
                changed = true;
            }
        }
        self.composition_content_dirty |= changed;
        Ok(changed)
    }

    pub fn launch(
        &mut self,
        token: crate::shell_runtime::InstanceToken,
        options: AttachedLaunchPlan,
        mut filesystem: Option<snowglass_vfs::MountedFilesystem>,
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
        let mut gamepad_layouts = options.gamepad_mapping.iter().collect::<Vec<_>>();
        gamepad_layouts.sort_by_key(|mapping| mapping.slot);
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
            graph_tracking: false,
            force_keyed_mutex: false,
            gamepad_layouts: gamepad_layouts
                .into_iter()
                .map(|mapping| mapping.layout.clone())
                .collect(),
            generated_configuration: options.generated_configuration.clone(),
        };
        let dirty = self.dirty.clone();
        let wake_pending = self.wake_pending.clone();
        let runtime = snowglass_runtime::EmulatorInstance::launch_with_waker(config, move || {
            dirty.store(true, std::sync::atomic::Ordering::Release);
            if !wake_pending.swap(true, std::sync::atomic::Ordering::AcqRel) && !wake_window() {
                wake_pending.store(false, std::sync::atomic::Ordering::Release);
            }
        });
        let runtime = match runtime {
            Ok(runtime) => runtime,
            Err(error) => {
                if let Some(filesystem) = filesystem.take() {
                    filesystem.finish(snowglass_vfs::FinalizationReason::LaunchFailed);
                }
                return Err(error.to_string());
            }
        };
        self.capture.attach(&runtime);
        let audio = runtime.audio_state();
        self.active = Some(ActiveEmulator {
            token,
            runtime,
            options,
            webrtc: None,
            audio,
            filesystem,
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
        let mut publication = snowglass_renderer::webrtc::Publication::start_d3d12(
            bind,
            targets,
            &self.render_device.chrome_compositors[0],
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
        self.active.as_ref().map(|active| {
            crate::input::GameInputEndpoint::new(
                active.runtime.protocol_input_sender(),
                active.runtime.gamepad_input(),
            )
        })
    }

    pub fn request_close(&mut self, token: crate::shell_runtime::InstanceToken) -> bool {
        let Some(active) = self.active.as_mut().filter(|active| active.token == token) else {
            return false;
        };
        active.runtime.close();
        self.capture.detach();
        true
    }

    pub fn set_volume(&self, token: crate::shell_runtime::InstanceToken, volume: f64) -> bool {
        let Some(volume) = snowglass_runtime::NormalizedVolume::new(volume as f32) else {
            return false;
        };
        let Some(active) = self.active.as_ref().filter(|active| active.token == token) else {
            return false;
        };
        active.runtime.set_volume(volume);
        true
    }

    pub fn set_muted(&self, token: crate::shell_runtime::InstanceToken, muted: bool) -> bool {
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
        crate::shell_runtime::InstanceToken,
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
        if let Ok(worker) = self.surface_worker_events.try_recv() {
            let (frame, dimensions_changed) =
                self.update_surface_frame(worker.connection, worker.update)?;
            return Some(GameHostCaptureEvent::Frame {
                connection: worker.connection,
                frame,
                observed_qpc: Some(worker.observed_qpc),
                dimensions_changed,
            });
        }
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
                    observed_qpc: None,
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
        let target_window = offer.stream().source_window;
        let flip_y = offer.stream().bottom_up;
        let (output_width, output_height) =
            rotated_extent(offer.stream().width, offer.stream().height, quarter_turns);
        self.render_device
            .create_surface(window, connection, output_width, output_height)?;
        {
            let output = self
                .render_device
                .surface_compositors
                .get_mut(&connection)
                .ok_or("the dedicated surface compositor was not retained")?;
            output.surface_id = surface_id;
            output.target_window = target_window;
            output.quarter_turns = quarter_turns;
            output.flip_y = flip_y;
            let compositor = output
                .compositor
                .as_mut()
                .ok_or("the surface compositor was already transferred")?;
            if let Err(error) = Renderer::import_surface_offer(compositor, &offer, quarter_turns) {
                self.render_device.surface_compositors.remove(&connection);
                return Err(error);
            }
        }

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
            let output = self
                .render_device
                .surface_compositors
                .get_mut(&connection)
                .ok_or("the dedicated surface compositor was not retained")?;
            let compositor = output
                .compositor
                .take()
                .ok_or("the surface compositor was already transferred")?;
            let content = compositor.composition_content()?;
            output.content = Some(content);
            output.worker = Some(SurfacePresentationWorker::spawn(
                compositor,
                frames,
                self.pointer.clone(),
                self.surface_worker_sender.clone(),
                connection,
                surface_id,
                output.width,
                output.height,
            ));
        } else if let Err(error) = self.capture.accept(offer) {
            self.render_device.surface_compositors.remove(&connection);
            return Err(error.into());
        }

        if let Some(publication) = self
            .active
            .as_mut()
            .and_then(|active| active.webrtc.as_mut())
        {
            if let Err(error) = publication.add_stream(connection, target_index) {
                self.capture.revoke(connection);
                self.render_device.surface_compositors.remove(&connection);
                return Err(error);
            }
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
        self.composition_content_dirty = true;
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
        if let Some(publication) = self
            .active
            .as_mut()
            .and_then(|active| active.webrtc.as_mut())
        {
            publication.reattach_d3d12(&replacement.chrome_compositors[0])?;
        }
        self.render_device = replacement;
        let stale_connections = self.surfaces.keys().copied().collect::<Vec<_>>();
        for connection in stale_connections {
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
        self.composition_content_dirty = true;
        self.chrome_bands.clear();
        self.chrome_dirty.clear();
        self.shell_refresh_pending = true;
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
        if let Some(output) = self.render_device.surface_compositors.get_mut(&connection) {
            output.latest_frame = Some(frame);
            if let Some(compositor) = output.compositor.as_mut() {
                Renderer::set_latest(compositor, connection, frame);
            }
        }
        self.composition_content_dirty |= dimensions_changed;
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
            self.composition_content_dirty = true;
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
        self.composition_content_dirty = true;
        let token = active.token;
        let filesystem = active.filesystem.take();
        drop(active.runtime);
        if let Some(filesystem) = filesystem {
            filesystem.finish(snowglass_vfs::FinalizationReason::ProcessExited);
        }
        Some(FinishedGame { token, connections })
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

    pub fn has_pending_chrome(&self) -> bool {
        !self.chrome_dirty.is_empty() || self.render_device.shell_worker.is_pending()
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

    /// Presents shell pixels into their independent transparent swapchain.
    /// WUC retains the last chrome buffer, so this runs only after Blitz
    /// changed a band and cannot consume the guest swapchain's latency permit.
    pub fn draw_chrome(
        &mut self,
        window: &impl snowglass_renderer::RenderWindow,
    ) -> Result<bool, String> {
        while let Some(result) = self.render_device.shell_worker.try_result() {
            let previous_bands = self.chrome_bands.len();
            let (bands, updated) = self.render_device.apply_shell_result(window, result)?;
            self.chrome_bands = (0..bands).collect();
            self.chrome_dirty.extend(updated);
            for band in bands..previous_bands {
                if let Some(compositor) = self.render_device.chrome_compositors.get_mut(band) {
                    compositor.drop_chrome_layer(band as u64);
                }
                self.chrome_dirty.remove(&band);
            }
            self.composition_content_dirty |= bands != previous_bands;
        }
        let extent = window.render_extent();
        let dirty = self.chrome_dirty.iter().copied().collect::<Vec<_>>();
        for band in dirty {
            let Some(compositor) = self.render_device.chrome_compositors.get_mut(band) else {
                self.chrome_dirty.remove(&band);
                continue;
            };
            if !compositor.try_begin_frame() {
                continue;
            }
            let plan = CompositingPlan::layered(
                [0.0, 0.0, 0.0, 0.0],
                vec![PlanLayer::Chrome {
                    id: band as u64,
                    rectangle: Rect::new(0, 0, extent.width, extent.height),
                }],
            );
            compositor
                .present_with_webrtc(&plan, None)
                .map_err(|error| error.to_string())?;
            self.chrome_dirty.remove(&band);
        }
        Ok(self.chrome_dirty.is_empty() && !self.render_device.shell_worker.is_pending())
    }

    pub fn rasterize_shell(&mut self, frame: ShellFrame) -> Result<bool, String> {
        self.render_device.submit_shell(frame);
        Ok(true)
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
        self.composition_content_dirty |= changed;
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
    fn shell_renderer_uses_the_self_contained_d3d12_shader_compiler() {
        let options = shell_instance_options();
        assert_eq!(options.backends, wgpu::Backends::DX12);
        assert!(matches!(
            options.backend_options.dx12.shader_compiler,
            wgpu::Dx12Compiler::Fxc
        ));
    }

    #[test]
    fn scene_reuse_requires_identical_vello_commands() {
        let mut scene = vello::Scene::new();
        scene.fill(
            vello::peniko::Fill::NonZero,
            kurbo::Affine::IDENTITY,
            vello::peniko::Color::WHITE,
            None,
            &kurbo::Rect::new(1.0, 2.0, 30.0, 40.0),
        );
        let identical = scene.clone();
        assert!(same_scene(&scene, &identical));

        let mut changed = identical;
        changed.encoding_mut().draw_data[0] ^= 1;
        assert!(!same_scene(&scene, &changed));
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
