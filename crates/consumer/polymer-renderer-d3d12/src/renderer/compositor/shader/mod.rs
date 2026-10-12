mod probe;

use super::*;
use librashader::presets::ShaderPresetPack;
use librashader::runtime::d3d12::{
    D3D12InputImage, D3D12OutputView, FilterChain, FilterChainOptions, FrameOptions,
};
use librashader::runtime::{FilterChainParameters, Size, Viewport};
use polymer_renderer::renderer::ShaderViewport;

const MAXIMUM_CANVAS: f32 = 4096.0;
const SETTLE_FRAMES: u32 = 120;
const IDLE_FRAME: Duration = Duration::from_millis(100);
const CANVAS_TARGET: u32 = FRAME_COUNT as u32;
const OUTPUT_TARGET: u32 = FRAME_COUNT as u32 + 1;
pub(super) const SHADER_TARGETS: u32 = 2;
pub(super) const SHADER_OUTPUT_DESCRIPTOR: u32 = DESCRIPTOR_COUNT - 1;

/// A librashader filter chain applied to everything a compositor composes.
pub struct ShaderStage {
    chain: FilterChain,
    viewport: ShaderViewport,
    aspect_ratio: f32,
    frame_count: usize,
    fresh: bool,
    settle: u32,
    last_frame: Option<std::time::Instant>,
    frame_delta: Duration,
    canvas: Option<ShaderTarget>,
    output: Option<ShaderTarget>,
}

struct ShaderTarget {
    resource: ID3D12Resource,
    extent: (u32, u32),
}

const _: () = {
    const fn sendable<T: Send>() {}
    sendable::<ShaderStage>();
};

impl ShaderStage {
    /// Compiles a preset for `device`, blocking until its lookup textures are uploaded.
    pub fn build(
        device: &ID3D12Device,
        pack: ShaderPresetPack,
        viewport: ShaderViewport,
        aspect_ratio: f32,
    ) -> Result<Self, String> {
        let chain = unsafe {
            FilterChain::load_from_pack(pack, device, Some(&FilterChainOptions::default()))
        }
        .map_err(|error| error.to_string())?;
        Ok(Self {
            chain,
            viewport,
            aspect_ratio,
            frame_count: 0,
            fresh: true,
            settle: 0,
            last_frame: None,
            frame_delta: Duration::ZERO,
            canvas: None,
            output: None,
        })
    }

    pub fn set_parameter(&self, name: &str, value: f32) {
        self.chain.parameters().set_parameter_value(name, value);
    }

    pub fn set_passes_enabled(&self, count: usize) {
        self.chain.parameters().set_passes_enabled(count);
    }

    /// Whether the chain still needs redraws to converge after its history
    /// was cleared, while no guest frames arrive to drive it.
    pub fn settling(&self) -> bool {
        self.settle > 0
            && self
                .last_frame
                .is_none_or(|last| last.elapsed() >= IDLE_FRAME)
    }

    /// Options for the next frame; presents carrying a guest frame advance
    /// time, as do redraws while the chain settles.
    fn advance(&mut self, carried: bool) -> FrameOptions {
        let clear_history = std::mem::take(&mut self.fresh);
        if clear_history {
            self.settle = SETTLE_FRAMES;
        }
        if carried {
            let now = std::time::Instant::now();
            if let Some(last) = self.last_frame.replace(now) {
                self.frame_delta = now - last;
            }
        }
        if carried || self.settle > 0 {
            self.frame_count += 1;
        }
        self.settle = self.settle.saturating_sub(1);
        let seconds = self.frame_delta.as_secs_f32();
        FrameOptions {
            clear_history,
            aspect_ratio: self.aspect_ratio,
            frametime_delta: self.frame_delta.as_millis() as u32,
            frames_per_second: if seconds > 0.0 { seconds.recip() } else { 1.0 },
            ..Default::default()
        }
    }

    fn target(
        slot: &mut Option<ShaderTarget>,
        device: &ID3D12Device,
        extent: (u32, u32),
    ) -> windows::core::Result<Option<ID3D12Resource>> {
        if slot.as_ref().is_some_and(|target| target.extent == extent) {
            return Ok(None);
        }
        let resource = unsafe {
            ShaderTarget::create(device, extent, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE)?
        };
        *slot = Some(ShaderTarget {
            resource: resource.clone(),
            extent,
        });
        Ok(Some(resource))
    }
}

impl ShaderTarget {
    unsafe fn create(
        device: &ID3D12Device,
        extent: (u32, u32),
        state: D3D12_RESOURCE_STATES,
    ) -> windows::core::Result<ID3D12Resource> {
        let mut resource: Option<ID3D12Resource> = None;
        unsafe {
            device.CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_DEFAULT,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                    Width: u64::from(extent.0),
                    Height: extent.1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: SWAPCHAIN_FORMAT,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                    Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                    Alignment: 0,
                },
                state,
                None,
                &mut resource,
            )?;
        }
        resource.ok_or_else(|| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER)
        })
    }
}

impl D3d12Compositor {
    /// Replaces the filter chain once the GPU has retired every frame using the previous one.
    pub fn set_shader(&mut self, shader: Option<ShaderStage>) -> windows::core::Result<()> {
        self.wait_idle()?;
        self.shader = shader;
        Ok(())
    }

    pub fn shader(&self) -> Option<&ShaderStage> {
        self.shader.as_ref()
    }

    /// Native pixels per composed pixel of the densest stream.
    fn native_scale(&self, plan: &CompositingPlan) -> f32 {
        plan.placements()
            .filter_map(|placement| {
                let surface = self.surfaces.iter().find(|surface| {
                    surface.connection == placement.connection
                        && surface.surface_id == placement.surface_id
                })?;
                let image = polymer_renderer::renderer::image_extent(
                    surface.imported.width,
                    surface.imported.height,
                    surface.quarter_turns,
                );
                let [left, top, right, bottom] = placement.content.destination;
                let [u0, v0, u1, v1] = placement.content.source;
                let horizontal = (u1 - u0) * image.width as f32 / (right - left).max(1.0);
                let vertical = (v1 - v0) * image.height as f32 / (bottom - top).max(1.0);
                Some(horizontal.max(vertical))
            })
            .reduce(f32::max)
            .unwrap_or(1.0)
    }

    fn ensure_shader_targets(
        &mut self,
        stage: &mut ShaderStage,
        canvas: (u32, u32),
        output: (u32, u32),
    ) -> Result<(), Box<dyn Error>> {
        let device = self.receiver.device().clone();
        let stale = stage.canvas.as_ref().map(|target| target.extent) != Some(canvas)
            || stage.output.as_ref().map(|target| target.extent) != Some(output);
        if !stale {
            return Ok(());
        }
        self.wait_idle()?;
        if let Some(resource) = ShaderStage::target(&mut stage.canvas, &device, canvas)? {
            unsafe {
                device.CreateRenderTargetView(
                    &resource,
                    None,
                    rtv_handle(&self.rtv_heap, self.rtv_stride, CANVAS_TARGET),
                );
            }
        }
        if let Some(resource) = ShaderStage::target(&mut stage.output, &device, output)? {
            unsafe {
                device.CreateRenderTargetView(
                    &resource,
                    None,
                    rtv_handle(&self.rtv_heap, self.rtv_stride, OUTPUT_TARGET),
                );
                create_srv(
                    &device,
                    &self.srv_heap,
                    self.srv_stride,
                    SHADER_OUTPUT_DESCRIPTOR,
                    &resource,
                );
            }
        }
        Ok(())
    }

    pub(super) fn draw_shaded(
        &mut self,
        frame_index: usize,
        plan: &CompositingPlan,
        carried: bool,
    ) -> Result<(), Box<dyn Error>> {
        let Some(mut stage) = self.shader.take() else {
            return self.draw_content(frame_index, plan);
        };
        let result = self.draw_through(&mut stage, frame_index, plan, carried);
        self.shader = Some(stage);
        result
    }

    fn draw_through(
        &mut self,
        stage: &mut ShaderStage,
        frame_index: usize,
        plan: &CompositingPlan,
        carried: bool,
    ) -> Result<(), Box<dyn Error>> {
        let Some(content) = plan
            .placements()
            .map(|placement| placement.content.destination)
            .filter(|[left, top, right, bottom]| right > left && bottom > top)
            .reduce(|[l0, t0, r0, b0], [l1, t1, r1, b1]| {
                [l0.min(l1), t0.min(t1), r0.max(r1), b0.max(b1)]
            })
        else {
            return Ok(());
        };
        let (width, height) = (content[2] - content[0], content[3] - content[1]);
        let scale = self
            .native_scale(plan)
            .min(MAXIMUM_CANVAS / width.max(height));
        let canvas = (
            (width * scale).ceil().max(1.0) as u32,
            (height * scale).ceil().max(1.0) as u32,
        );
        let viewport = match stage.viewport {
            ShaderViewport::Content => content,
            ShaderViewport::Element => [0.0, 0.0, self.extent.0 as f32, self.extent.1 as f32],
        };
        let output = (
            (viewport[2] - viewport[0]).round().max(1.0) as u32,
            (viewport[3] - viewport[1]).round().max(1.0) as u32,
        );
        self.ensure_shader_targets(stage, canvas, output)?;
        let canvas_texture = stage
            .canvas
            .as_ref()
            .expect("canvas exists")
            .resource
            .clone();
        let output_texture = stage
            .output
            .as_ref()
            .expect("output exists")
            .resource
            .clone();

        let stitched = CompositingPlan::new(
            plan.clear_color(),
            plan.placements()
                .map(|placement| {
                    let mut placement = *placement;
                    let [left, top, right, bottom] = placement.content.destination;
                    placement.content.destination = [
                        (left - content[0]) * scale,
                        (top - content[1]) * scale,
                        (right - content[0]) * scale,
                        (bottom - content[1]) * scale,
                    ];
                    placement.corner_radius *= scale;
                    placement
                })
                .collect(),
        );
        unsafe {
            transition(
                &self.command_list,
                &canvas_texture,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            );
            let rtv = rtv_handle(&self.rtv_heap, self.rtv_stride, CANVAS_TARGET);
            self.command_list
                .OMSetRenderTargets(1, Some(&rtv), false, None);
            self.command_list
                .ClearRenderTargetView(rtv, &[0.0, 0.0, 0.0, 1.0], None);
        }
        self.target = canvas;
        self.draw_content(frame_index, &stitched)?;
        self.target = self.extent;
        unsafe {
            transition(
                &self.command_list,
                &canvas_texture,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            );
            transition(
                &self.command_list,
                &output_texture,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            );
            let size = Size::new(output.0, output.1);
            let options = stage.advance(carried);
            stage.chain.frame(
                &self.command_list,
                D3D12InputImage::from(canvas_texture.to_ref()),
                &Viewport {
                    x: 0.0,
                    y: 0.0,
                    mvp: None,
                    output: D3D12OutputView::new_from_raw(
                        rtv_handle(&self.rtv_heap, self.rtv_stride, OUTPUT_TARGET),
                        size,
                        SWAPCHAIN_FORMAT,
                    ),
                    size,
                },
                stage.frame_count,
                Some(&options),
            )?;
            transition(
                &self.command_list,
                &output_texture,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            );
            let rtv = rtv_handle(
                &self.rtv_heap,
                self.rtv_stride,
                self.swapchain.GetCurrentBackBufferIndex(),
            );
            self.command_list
                .OMSetRenderTargets(1, Some(&rtv), false, None);
            self.command_list
                .SetGraphicsRootSignature(&self.root_signature);
            self.command_list
                .SetDescriptorHeaps(&[Some(self.srv_heap.clone())]);
            self.command_list
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.command_list.SetPipelineState(&self.content_pipeline);
        }
        let corner = |x: f32, y: f32| {
            (
                x / self.extent.0 as f32 * 2.0 - 1.0,
                1.0 - y / self.extent.1 as f32 * 2.0,
            )
        };
        let (left, top) = corner(viewport[0], viewport[1]);
        let (right, bottom) = corner(viewport[2], viewport[3]);
        let vertices = super::present::quad(
            left,
            top,
            right,
            bottom,
            [0.0, 0.0, 1.0, 1.0],
            false,
            0,
            [255; 4],
            [output.0 as f32, output.1 as f32, 0.0, 0.0],
        );
        self.draw_vertices(frame_index, SHADER_OUTPUT_DESCRIPTOR, &vertices, None)
    }
}
