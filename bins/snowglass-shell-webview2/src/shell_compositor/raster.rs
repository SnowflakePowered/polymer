use super::{
    ShellDeviceContext,
    chrome::{ChromeResource, ChromeTexture},
};
use crate::shell_dom::{
    RasterChunkId, RasterChunkLayout, ShellCompositionGroup, ShellFrame, TrackedSurfaceLayout,
};
use anyrender::PaintScene as _;
use anyrender_vello::VelloScenePainter;
use parking_lot::Mutex;
use snowglass_protocol::AdapterLuid;
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    sync::{Arc, mpsc},
    thread,
};
use vello::wgpu;
use vello::{AaConfig, AaSupport, RenderParams, Renderer as VelloRenderer, RendererOptions};
use windows::core::{IUnknown, Interface};

/// Vello GPU state owned by the shell compositor.
pub(crate) struct ShellRasterizer {
    renderer: VelloRenderer,
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_luid: AdapterLuid,
    targets: HashMap<RasterChunkId, ChromeTexture>,
    /// The complete scene currently represented by each retained band texture.
    /// Comparing encodings is cheap relative to another Vello dispatch and
    /// avoids unsafe partial updates across alpha and effect boundaries.
    rendered_scenes: HashMap<RasterChunkId, vello::Scene>,
    /// Backend-neutral, layer-local scene represented by each retained texture.
    /// Compositor-only property changes therefore compare equal without Vello.
    recorded_scenes: HashMap<RasterChunkId, anyrender::Scene>,
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
            targets: HashMap::new(),
            rendered_scenes: HashMap::new(),
            recorded_scenes: HashMap::new(),
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

    pub(crate) fn device_context(&self) -> Result<ShellDeviceContext, Box<dyn Error>> {
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
            if target_is_current_size
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
                        tracing::error!(
                            ?id,
                            "snowglass: shell chrome could not make chunk: {error}"
                        );
                        continue;
                    }
                };
                self.targets.insert(id, target);
            }
            let target = self
                .targets
                .get_mut(&id)
                .expect("chunk target was inserted");
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
                tracing::error!(?id, "snowglass: rasterizing shell chunk failed: {error}");
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
        self.targets.retain(|id, _| live.contains(id));
        self.rendered_scenes.retain(|id, _| live.contains(id));
        self.recorded_scenes.retain(|id, _| live.contains(id));
        if std::env::var_os("SNOWGLASS_GAME_LAYER_TIMING").is_some() {
            tracing::error!(
                "snowglass: shell chrome rasterized {}x{} in {:.2}ms over {}/{} chunk(s)",
                frame.width,
                frame.height,
                rasterized.elapsed().as_secs_f64() * 1000.0,
                updates.len(),
                chunk_count,
            );
            tracing::error!(
                "snowglass: shell chunk raster timings {}",
                timings
                    .iter()
                    .map(|(id, elapsed)| format!("{id:?}:{:.2}ms", elapsed.as_secs_f64() * 1000.0))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            if !reused.is_empty() {
                tracing::error!(
                    "snowglass: shell reused unchanged chunk(s) {}",
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
    pub(crate) surface_layout: Vec<TrackedSurfaceLayout>,
}

#[derive(Default)]
struct ShellRasterWorkerState {
    pending: Option<ShellFrame>,
    busy: bool,
    /// One raster result is waiting for the presentation thread to submit all
    /// of its raster chunks and matching tracked-surface layout. The rasterizer
    /// retains and updates its chunk textures, so it must not begin the next
    /// generation until that submission has sampled the current one.
    awaiting_publication: bool,
    ready: usize,
    stopping: bool,
}

/// Latest-only Vello worker. DOM/layout stays on the UI thread, but shader
/// dispatch, command encoding, and local chunk raster no longer delay
/// capture draining or a guest Present.
pub(crate) struct ShellRasterWorker {
    state: Arc<Mutex<ShellRasterWorkerState>>,
    results: mpsc::Receiver<ShellRasterResult>,
    worker: Option<thread::JoinHandle<()>>,
}

impl ShellRasterWorker {
    pub(crate) fn spawn(mut rasterizer: ShellRasterizer) -> Self {
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
                        if state.awaiting_publication {
                            None
                        } else {
                            state.pending.take()
                        }
                    };
                    let Some(mut frame) = frame else {
                        thread::park();
                        continue;
                    };
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
                    let surface_layout = std::mem::take(&mut frame.surface_layout);
                    let composition_groups = std::mem::take(&mut frame.composition_groups);
                    let updates = rasterizer.rasterize_frame(frame);
                    {
                        let mut state = worker_state.lock();
                        state.ready += 1;
                        state.awaiting_publication = true;
                    }
                    if sender
                        .send(ShellRasterResult {
                            updates,
                            raster_chunks,
                            composition_groups,
                            surface_layout,
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

    pub(crate) fn submit(&self, frame: ShellFrame) {
        let mut state = self.state.lock();
        state.pending = Some(frame);
        state.busy = true;
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
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
        state.ready = state.ready.saturating_sub(1);
        state.busy = state.pending.is_some();
        drop(state);
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
    }

    pub(crate) fn is_pending(&self) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_self_contained_d3d12_shader_compiler() {
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
}
