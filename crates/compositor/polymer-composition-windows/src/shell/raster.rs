use super::{
    ShellDeviceContext,
    chrome::{ChromeResource, ChromeTexture},
};
use anyrender::PaintScene as _;
use anyrender_vello::VelloScenePainter;
use parking_lot::{Condvar, Mutex};
use polymer_banner::BannerTextures;
use polymer_composition::{
    CompositionScene as ShellFrame, RasterChunkId, RasterChunkLayout, ShellCompositionGroup,
};
use polymer_consumer_d3d12::D3d12ConsumerDevice;
use polymer_mipmap::MipTextures;
use polymer_protocol::AdapterLuid;
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    sync::{Arc, mpsc},
    thread,
};
use vello::wgpu;
use vello::{AaConfig, AaSupport, RenderParams, Renderer as VelloRenderer, RendererOptions};
use windows::{
    Win32::Graphics::Direct3D12::ID3D12Device,
    core::{IUnknown, Interface},
};

/// Vello GPU state owned by the shell compositor.
pub(crate) struct ShellRasterizer {
    renderer: VelloRenderer,
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_luid: AdapterLuid,
    targets: HashMap<RasterChunkId, ChromeTexture>,
    rendered_scenes: HashMap<RasterChunkId, vello::Scene>,
    recorded_scenes: HashMap<RasterChunkId, anyrender::Scene>,
    banners: BannerTextures,
    mips: MipTextures,
    animated_chunks: HashSet<RasterChunkId>,
    atlas_images: HashSet<u64>,
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
                        ..
                    },
                    vello_encoding::Patch::Ramp {
                        draw_data_offset: right_offset,
                        stops: right_stops,
                        extend: right_extend,
                        ..
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

fn scene_images(scene: &vello::Scene) -> impl Iterator<Item = &vello::peniko::ImageData> {
    scene
        .encoding()
        .resources
        .patches
        .iter()
        .filter_map(|patch| match patch {
            vello_encoding::Patch::Image { image, .. } => Some(image),
            _ => None,
        })
}

fn shell_instance_options() -> wgpu::InstanceDescriptor {
    let mut options = wgpu::InstanceDescriptor::new_without_display_handle();
    options.backends = wgpu::Backends::DX12;
    // The D3D12 debug layer costs several percent of a core per shell frame.
    // Opt back in with `WGPU_DEBUG=1` / `WGPU_VALIDATION=1`.
    options.flags = wgpu::InstanceFlags::empty().with_env();
    options.backend_options.dx12.shader_compiler = wgpu::Dx12Compiler::StaticDxc;
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
                "polymer: using Vello CPU geometry stages on a virtual/software D3D12 adapter"
            );
        }
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("polymer shell chrome"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::default(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::default(),
            })
            .await
            .map_err(|error| format!("shell chrome could not open a D3D12 device: {error}"))?;
        if let Some(hal) = unsafe { queue.as_hal::<wgpu_hal::api::Dx12>() } {
            let _ = unsafe {
                hal.as_raw()
                    .SetName(windows_wgpu::core::w!("Polymer shell chrome queue"))
            };
        }
        // SAFETY: the raw device is only read while wgpu reports it lost, before it is destroyed.
        if let Some(raw) = unsafe { device.as_hal::<wgpu_hal::api::Dx12>() }
            .map(|hal| windows_wgpu::core::Interface::as_raw(hal.raw_device()) as usize)
        {
            device.set_device_lost_callback(move |reason, message| {
                if reason == wgpu::DeviceLostReason::Destroyed {
                    return;
                }
                let raw = raw as *mut std::ffi::c_void;
                let report = unsafe { ID3D12Device::from_raw_borrowed(&raw) }
                    .map(D3d12ConsumerDevice::removal_report_for)
                    .unwrap_or_default();
                tracing::error!("polymer: shell chrome D3D12 device lost: {message}; {report}");
            });
        }
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
            targets: HashMap::new(),
            rendered_scenes: HashMap::new(),
            recorded_scenes: HashMap::new(),
            banners: BannerTextures::default(),
            mips: MipTextures::default(),
            animated_chunks: HashSet::new(),
            atlas_images: HashSet::new(),
        })
    }
    /// Prepares the initial adapter before the shell document starts. A later foreign
    /// adapter causes the complete RenderDevice to be recreated on demand.
    pub async fn new() -> Result<Self, Box<dyn Error>> {
        Self::new_for_adapter(None).await
    }

    pub(crate) async fn new_for_adapter(
        desired: Option<AdapterLuid>,
    ) -> Result<Self, Box<dyn Error>> {
        D3d12ConsumerDevice::enable_removal_diagnostics();
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
                    apply_limit_buckets: false,
                })
                .await
                .map_err(|error| format!("shell chrome found no D3D12 adapter: {error}"))?
        };
        Self::from_adapter(&adapter).await
    }

    pub(crate) fn retained_device_context(
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

    pub(super) fn device_context(&self) -> Result<ShellDeviceContext, Box<dyn Error>> {
        let (device, queue) = self.retained_device_context()?;
        Ok(ShellDeviceContext {
            adapter: self.adapter_luid,
            // SAFETY: retained_device_context transferred one owned reference
            // for each pointer into this capture-side device anchor.
            device: unsafe { IUnknown::from_raw(device) },
            queue: unsafe { IUnknown::from_raw(queue) },
        })
    }

    fn rasterize_frame(&mut self, mut frame: ShellFrame) -> Vec<(RasterChunkId, ChromeResource)> {
        let rasterized = std::time::Instant::now();
        let chunk_count = frame.raster_chunks.len();
        let live = frame
            .raster_chunks
            .iter()
            .map(|chunk| chunk.id)
            .collect::<HashSet<_>>();
        let mut updates = Vec::with_capacity(chunk_count);
        let mut reused = Vec::with_capacity(chunk_count);
        for image in self.mips.begin_frame(frame.mip_generation) {
            self.renderer.override_image(&image, None);
        }
        let mut timings = Vec::with_capacity(chunk_count);
        for layer in frame.raster_chunks.drain(..) {
            let id = layer.id;
            let bounds = layer.bounds;
            let target_width = bounds.width().ceil().max(1.0) as u32;
            let target_height = bounds.height().ceil().max(1.0) as u32;
            let parameters = RenderParams {
                base_color: vello::peniko::Color::TRANSPARENT,
                width: target_width,
                height: target_height,
                antialiasing_method: AaConfig::Area,
            };
            let recorded_scene = layer.scene;
            let target_is_current_size = self.targets.get(&id).is_some_and(|target| {
                (target.width, target.height) == (target_width, target_height)
            });
            let animated = self.animated_chunks.contains(&id);
            if target_is_current_size
                && !animated
                && let Some(retained) = self.recorded_scenes.get(&id)
                && retained == &recorded_scene
            {
                reused.push(id);
                continue;
            }
            // Blitz records backend-neutral commands on the UI thread. The
            // expensive Vello path/glyph encoding belongs here on the
            // latest-only worker alongside rasterization.
            let mut scene = vello::Scene::new();
            VelloScenePainter::new(&mut scene)
                .append_scene(recorded_scene.clone(), kurbo::Affine::IDENTITY);
            if target_is_current_size
                && !animated
                && self
                    .rendered_scenes
                    .get(&id)
                    .is_some_and(|rendered| same_scene(rendered, &scene))
            {
                self.recorded_scenes.insert(id, recorded_scene);
                reused.push(id);
                continue;
            }
            let started = std::time::Instant::now();
            if !target_is_current_size {
                let target = match ChromeTexture::new(&self.device, target_width, target_height) {
                    Ok(target) => target,
                    Err(error) => {
                        tracing::error!(?id, "polymer: shell chrome could not make chunk: {error}");
                        continue;
                    }
                };
                self.targets.insert(id, target);
            }
            let target = self
                .targets
                .get_mut(&id)
                .expect("chunk target was inserted");
            // One Vello renderer rasterizes several independent retained
            // chunks. Reassert an image only when it is animated or has not
            // been written since the atlas texture was last replaced.
            if scene.encoding().resources.patches.is_empty() {
                self.atlas_images.clear();
            }
            let mut animated = false;
            for image in scene_images(&scene) {
                if let Some(texture) = self
                    .banners
                    .prepare(&self.device, &self.queue, image)
                    .or_else(|| self.mips.prepare(&self.device, &self.queue, image))
                {
                    self.renderer.override_image(
                        image,
                        Some(wgpu::TexelCopyTextureInfoBase {
                            texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        }),
                    );
                }
                let banner = self.banners.is_animated(image);
                animated |= banner;
                if self.atlas_images.insert(image.data.id()) || banner {
                    self.renderer.mark_override_image_dirty(image);
                }
            }
            if animated {
                self.animated_chunks.insert(id);
            } else {
                self.animated_chunks.remove(&id);
            }
            // Retained WUC visuals make translated and unchanged scenes free
            // above. When pixels truly change, redraw the complete local
            // fragment into a transparently cleared target. Partial copies
            // cannot safely account for alpha masks, antialiasing, filters, or
            // native shadows and were the source of accumulated trails.
            if let Err(error) = self.renderer.render_to_texture(
                &self.device,
                &self.queue,
                &scene,
                &target.view,
                &parameters,
            ) {
                tracing::error!(?id, "polymer: rasterizing shell chunk failed: {error}");
                continue;
            }
            self.rendered_scenes.insert(id, scene);
            self.recorded_scenes.insert(id, recorded_scene);
            timings.push((id, started.elapsed()));
            updates.push((id, target.retained_resource()));
        }
        // Vello and the compositor intentionally share this exact D3D12
        // command queue. The compositor's sampling submission is therefore
        // ordered after these raster submissions without a CPU wait. Blocking
        // here serialized every DOM transform update with the GPU and
        // starved both input delivery and guest presentation while dragging.
        self.device.poll(wgpu::PollType::Poll).ok();
        for image in self
            .banners
            .release_stale()
            .into_iter()
            .chain(self.mips.release_stale())
        {
            self.renderer.override_image(&image, None);
        }
        self.targets.retain(|id, _| live.contains(id));
        self.animated_chunks.retain(|id| live.contains(id));
        self.rendered_scenes.retain(|id, _| live.contains(id));
        self.recorded_scenes.retain(|id, _| live.contains(id));
        if std::env::var_os("POLYMER_GAME_LAYER_TIMING").is_some() {
            tracing::error!(
                "polymer: shell chrome rasterized {}x{} in {:.2}ms over {}/{} chunk(s)",
                frame.width,
                frame.height,
                rasterized.elapsed().as_secs_f64() * 1000.0,
                updates.len(),
                chunk_count,
            );
            tracing::error!(
                "polymer: shell chunk raster timings {}",
                timings
                    .iter()
                    .map(|(id, elapsed)| format!("{id:?}:{:.2}ms", elapsed.as_secs_f64() * 1000.0))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            if !reused.is_empty() {
                tracing::error!(
                    "polymer: shell reused unchanged chunk(s) {}",
                    reused
                        .iter()
                        .map(|id| format!("{id:?}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
        }
        updates
    }
}

pub(crate) struct ShellRasterResult {
    pub(crate) updates: Vec<(RasterChunkId, ChromeResource)>,
    pub(crate) raster_chunks: Vec<RasterChunkLayout>,
    pub(crate) composition_groups: Vec<ShellCompositionGroup>,
    pub(crate) revision: u64,
}

#[derive(Default)]
struct ShellRasterWorkerState {
    pending: Option<ShellFrame>,
    /// One raster result is waiting for the presentation thread to submit all
    /// of its raster chunks and matching tracked-surface layout. The rasterizer
    /// retains and updates its chunk textures, so it must not begin the next
    /// generation until that submission has sampled the current one.
    awaiting_publication: bool,
    stopping: bool,
}

/// Latest-only Vello worker. DOM/layout stays on the UI thread, but shader
/// dispatch, command encoding, and local chunk raster no longer delay
/// capture draining or a guest Present.
pub(crate) struct ShellRasterWorker {
    state: Arc<Mutex<ShellRasterWorkerState>>,
    wake: Arc<Condvar>,
    results: mpsc::Receiver<ShellRasterResult>,
    worker: Option<thread::JoinHandle<()>>,
}

impl ShellRasterWorker {
    pub(crate) fn spawn(
        mut rasterizer: ShellRasterizer,
        publication_ready: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let state = Arc::new(Mutex::new(ShellRasterWorkerState::default()));
        let worker_state = Arc::clone(&state);
        let wake = Arc::new(Condvar::new());
        let worker_wake = Arc::clone(&wake);
        let (sender, results) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("polymer-blitz-raster".to_owned())
            .spawn(move || {
                loop {
                    let frame = {
                        let mut state = worker_state.lock();
                        worker_wake.wait_while(&mut state, |state| {
                            !state.stopping
                                && (state.awaiting_publication || state.pending.is_none())
                        });
                        if state.stopping {
                            break;
                        }
                        state.pending.take()
                    };
                    let Some(mut frame) = frame else { continue };
                    let raster_chunks = frame
                        .raster_chunks
                        .iter()
                        .map(|chunk| RasterChunkLayout {
                            id: chunk.id,
                            bounds: chunk.bounds,
                            z_order: chunk.z_order,
                            group: chunk.group,
                        })
                        .collect();
                    let revision = frame.revision;
                    let composition_groups = std::mem::take(&mut frame.composition_groups);
                    let updates = rasterizer.rasterize_frame(frame);
                    {
                        let mut state = worker_state.lock();
                        state.awaiting_publication = true;
                    }
                    if sender
                        .send(ShellRasterResult {
                            updates,
                            raster_chunks,
                            composition_groups,
                            revision,
                        })
                        .is_err()
                    {
                        break;
                    }
                    publication_ready();
                }
            })
            .expect("failed to create the Blitz raster worker");
        Self {
            state,
            wake,
            results,
            worker: Some(worker),
        }
    }

    pub(crate) fn submit(&self, frame: ShellFrame) -> Option<u64> {
        let mut state = self.state.lock();
        let superseded = state.pending.replace(frame).map(|frame| frame.revision);
        drop(state);
        self.wake.notify_one();
        superseded
    }

    pub(crate) fn try_result(&self) -> Option<ShellRasterResult> {
        self.results.try_recv().ok()
    }

    /// Releases the raster worker only after the compositor has submitted the
    /// result it produced. This keeps the retained texture immutable between
    /// handoff and sampling while still coalescing any newer DOM frames.
    pub(crate) fn acknowledge_publication(&self) {
        let mut state = self.state.lock();
        if !state.awaiting_publication {
            return;
        }
        state.awaiting_publication = false;
        drop(state);
        self.wake.notify_one();
    }

    pub(crate) fn is_pending(&self) -> bool {
        let state = self.state.lock();
        state.pending.is_some() || state.awaiting_publication
    }
}

impl Drop for ShellRasterWorker {
    fn drop(&mut self) {
        {
            let mut state = self.state.lock();
            state.stopping = true;
            state.pending = None;
        }
        self.wake.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
