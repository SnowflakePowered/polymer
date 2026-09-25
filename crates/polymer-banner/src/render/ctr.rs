use crate::ctr::CtrBanner;
use cgfx_wgpu::{GpuScene, HOME_MENU_ASPECT_RATIO, HomeMenuCamera, Renderer};
use std::sync::Arc;
use std::time::Instant;

/// GPU resources for one 3DS banner placeholder. The cgfx renderer keeps its
/// camera, lights and fog per instance, so every banner owns one.
///
/// The 3DS never displays framebuffer alpha, so materials leave arbitrary
/// values in it. The scene is instead drawn over black and over white at twice
/// the output size, and the difference gives each pixel's real coverage.
pub(super) struct CtrTarget {
    banner: Arc<CtrBanner>,
    renderer: Renderer,
    scene: GpuScene,
    over_black: wgpu::TextureView,
    over_white: wgpu::TextureView,
    depth: wgpu::TextureView,
    viewport: [f32; 4],
    matte: wgpu::RenderPipeline,
    matte_bindings: wgpu::BindGroup,
    pub(super) output: wgpu::Texture,
    output_view: wgpu::TextureView,
    started: Instant,
}

impl CtrTarget {
    const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
    const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24PlusStencil8;
    const SUPERSAMPLING: u32 = 2;
    const FRAMES_PER_SECOND: f32 = 60.0;
    /// The HOME Menu turns a banner's root once every ten seconds.
    const TURN_FRAMES: f32 = 600.0;

    pub(super) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        banner: Arc<CtrBanner>,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        let renderer = Renderer::new(device, Self::COLOR_FORMAT, Some(Self::DEPTH_FORMAT));
        let scene = renderer.upload_scene(device, queue, &banner.scene).ok()?;
        let texture = |label, scale: u32, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: width * scale,
                    height: height * scale,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let scene_usage =
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let view = |texture: wgpu::Texture| texture.create_view(&Default::default());
        let over_black = view(texture(
            "polymer 3ds banner over black",
            Self::SUPERSAMPLING,
            Self::COLOR_FORMAT,
            scene_usage,
        ));
        let over_white = view(texture(
            "polymer 3ds banner over white",
            Self::SUPERSAMPLING,
            Self::COLOR_FORMAT,
            scene_usage,
        ));
        let depth = view(texture(
            "polymer 3ds banner depth",
            Self::SUPERSAMPLING,
            Self::DEPTH_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        ));
        let output = texture(
            "polymer 3ds banner output",
            1,
            Self::COLOR_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let (matte, matte_bindings) = Self::matte(device, &over_black, &over_white);
        let (scaled_width, scaled_height) = (
            (width * Self::SUPERSAMPLING) as f32,
            (height * Self::SUPERSAMPLING) as f32,
        );
        let (fitted_width, fitted_height) = if scaled_width / scaled_height > HOME_MENU_ASPECT_RATIO
        {
            (scaled_height * HOME_MENU_ASPECT_RATIO, scaled_height)
        } else {
            (scaled_width, scaled_width / HOME_MENU_ASPECT_RATIO)
        };
        Some(Self {
            banner,
            renderer,
            scene,
            over_black,
            over_white,
            depth,
            viewport: [
                (scaled_width - fitted_width) * 0.5,
                (scaled_height - fitted_height) * 0.5,
                fitted_width,
                fitted_height,
            ],
            matte,
            matte_bindings,
            output_view: output.create_view(&Default::default()),
            output,
            started: Instant::now(),
        })
    }

    fn matte(
        device: &wgpu::Device,
        over_black: &wgpu::TextureView,
        over_white: &wgpu::TextureView,
    ) -> (wgpu::RenderPipeline, wgpu::BindGroup) {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("polymer 3ds banner matte"),
            source: wgpu::ShaderSource::Wgsl(include_str!("ctr.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("polymer 3ds banner matte"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(Self::COLOR_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("polymer 3ds banner matte"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("polymer 3ds banner matte"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(over_black),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(over_white),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        (pipeline, bindings)
    }

    pub(super) fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let source = &self.banner.scene;
        let frame = self.started.elapsed().as_secs_f32() * Self::FRAMES_PER_SECOND;
        let mut evaluation = source
            .evaluate_composed(0, frame)
            .unwrap_or_else(|| source.rest_evaluation(frame));
        let angle =
            -frame.rem_euclid(Self::TURN_FRAMES) * std::f32::consts::TAU / Self::TURN_FRAMES;
        let camera_world = HomeMenuCamera::default().view_matrix().inverse();
        evaluation.apply_home_menu_presentation(source, angle, camera_world);
        self.renderer
            .apply_evaluation(device, queue, source, &mut self.scene, &evaluation);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("polymer 3ds banner"),
        });
        self.draw_scene(&mut encoder, &self.over_black, wgpu::Color::BLACK);
        self.draw_scene(&mut encoder, &self.over_white, wgpu::Color::WHITE);
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("polymer 3ds banner matte"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.output_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.matte);
            pass.set_bind_group(0, &self.matte_bindings, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
    }

    fn draw_scene(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        background: wgpu::Color,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("polymer 3ds banner scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(background),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0),
                    store: wgpu::StoreOp::Discard,
                }),
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let [x, y, width, height] = self.viewport;
        pass.set_viewport(x, y, width, height, 0.0, 1.0);
        self.renderer.render(&self.scene, &mut pass);
    }
}
