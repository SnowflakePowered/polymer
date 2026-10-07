use super::BannerImages;
use super::ctr::CtrTarget;
use super::mesh::{Mesh, Vertex};
use super::webp::WebpTarget;
use super::wii::{WiiRenderer, WiiTarget};
use crate::Banner;
use peniko::ImageData;
use std::collections::HashMap;
use std::time::Instant;

const SAMPLES: u32 = 4;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

type Matrix = [[f32; 4]; 4];

struct Camera {
    yaw: f32,
    /// Turntable speed around the vertical axis, in radians per second.
    spin: f32,
    pitch: f32,
    fov: f32,
}

impl Camera {
    const PS2: Self = Self {
        yaw: -0.4,
        spin: 0.8,
        pitch: 0.15,
        fov: std::f32::consts::PI / 6.0,
    };

    fn uniforms(&self, mesh: &Mesh, aspect: f32, time: f32) -> Uniforms {
        Uniforms {
            view_projection: self.view_projection(mesh, aspect, self.yaw + self.spin * time),
            light_directions: mesh.lighting.directions,
            light_colors: mesh.lighting.colors,
            ambient: mesh.lighting.ambient,
        }
    }

    fn view_projection(&self, mesh: &Mesh, aspect: f32, yaw: f32) -> Matrix {
        let center = [0, 1, 2].map(|axis| (mesh.min[axis] + mesh.max[axis]) * 0.5);
        let radius = (0..3)
            .map(|axis| (mesh.max[axis] - mesh.min[axis]) * 0.5)
            .map(|extent| extent * extent)
            .sum::<f32>()
            .sqrt()
            .max(f32::EPSILON);
        let fov = if aspect < 1.0 {
            2.0 * ((self.fov * 0.5).tan() / aspect).atan()
        } else {
            self.fov
        };
        let distance = radius / (self.fov * 0.5).sin();
        let eye = [
            center[0] + distance * self.pitch.cos() * yaw.sin(),
            center[1] + distance * self.pitch.sin(),
            center[2] + distance * self.pitch.cos() * yaw.cos(),
        ];
        let near = (distance - radius).max(distance * 0.01);
        Self::multiply(
            &Self::perspective(fov, aspect, near, distance + radius),
            &Self::look_at(eye, center),
        )
    }

    fn perspective(fov: f32, aspect: f32, near: f32, far: f32) -> Matrix {
        let focal = 1.0 / (fov * 0.5).tan();
        let depth = far / (near - far);
        [
            [focal / aspect, 0.0, 0.0, 0.0],
            [0.0, focal, 0.0, 0.0],
            [0.0, 0.0, depth, -1.0],
            [0.0, 0.0, near * depth, 0.0],
        ]
    }

    fn look_at(eye: [f32; 3], target: [f32; 3]) -> Matrix {
        let forward = Self::normalize([target[0] - eye[0], target[1] - eye[1], target[2] - eye[2]]);
        let side = Self::normalize(Self::cross(forward, [0.0, 1.0, 0.0]));
        let up = Self::cross(side, forward);
        [
            [side[0], up[0], -forward[0], 0.0],
            [side[1], up[1], -forward[1], 0.0],
            [side[2], up[2], -forward[2], 0.0],
            [
                -Self::dot(side, eye),
                -Self::dot(up, eye),
                Self::dot(forward, eye),
                1.0,
            ],
        ]
    }

    fn multiply(left: &Matrix, right: &Matrix) -> Matrix {
        std::array::from_fn(|column| {
            std::array::from_fn(|row| (0..4).map(|k| left[k][row] * right[column][k]).sum())
        })
    }

    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    fn normalize(value: [f32; 3]) -> [f32; 3] {
        let length = Self::dot(value, value).sqrt().max(f32::EPSILON);
        value.map(|component| component / length)
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_projection: Matrix,
    light_directions: [[f32; 4]; 3],
    light_colors: [[f32; 4]; 3],
    ambient: [f32; 4],
}

struct BannerRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl BannerRenderer {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("polymer banner shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty,
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("polymer banner bindings"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                entry(
                    1,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(
                    2,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("polymer banner layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("polymer banner pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x4],
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: SAMPLES,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR_FORMAT,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("polymer banner sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
        }
    }

    fn texture(
        device: &wgpu::Device,
        label: &str,
        width: u32,
        height: u32,
        samples: u32,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    }

    fn target(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mesh: Mesh,
        width: u32,
        height: u32,
    ) -> BannerTarget {
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("polymer banner uniforms"),
            size: size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("polymer banner vertices"),
            size: (mesh.vertex_count() * size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let icon = Self::texture(
            device,
            "polymer banner texture",
            mesh.texture.width,
            mesh.texture.height,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        queue.write_texture(
            icon.as_image_copy(),
            &mesh.texture.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(mesh.texture.width * 4),
                rows_per_image: None,
            },
            icon.size(),
        );
        let icon_view = icon.create_view(&Default::default());
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("polymer banner bindings"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&icon_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let output = Self::texture(
            device,
            "polymer banner output",
            width,
            height,
            1,
            COLOR_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let multisampled = Self::texture(
            device,
            "polymer banner multisample",
            width,
            height,
            SAMPLES,
            COLOR_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let depth = Self::texture(
            device,
            "polymer banner depth",
            width,
            height,
            SAMPLES,
            DEPTH_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        BannerTarget {
            output_view: output.create_view(&Default::default()),
            multisampled_view: multisampled.create_view(&Default::default()),
            depth_view: depth.create_view(&Default::default()),
            output,
            vertex_buffer,
            bindings,
            pose: Vec::with_capacity(mesh.vertex_count()),
            mesh,
            camera: Camera::PS2,
            aspect: width as f32 / height as f32,
            uniform_buffer,
            started: Instant::now(),
        }
    }

    fn draw(&self, device: &wgpu::Device, queue: &wgpu::Queue, target: &mut BannerTarget) {
        let time = target.started.elapsed().as_secs_f32();
        let uniforms = target.camera.uniforms(&target.mesh, target.aspect, time);
        queue.write_buffer(&target.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
        target.mesh.pose(time, &mut target.pose);
        queue.write_buffer(&target.vertex_buffer, 0, bytemuck::cast_slice(&target.pose));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("polymer banner"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("polymer banner"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.multisampled_view,
                    depth_slice: None,
                    resolve_target: Some(&target.output_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &target.bindings, &[]);
            pass.set_vertex_buffer(0, target.vertex_buffer.slice(..));
            pass.draw(0..target.pose.len() as u32, 0..1);
        }
        queue.submit([encoder.finish()]);
    }
}

/// GPU resources for one placeholder, redrawn in place every frame.
struct BannerTarget {
    mesh: Mesh,
    camera: Camera,
    aspect: f32,
    uniform_buffer: wgpu::Buffer,
    pose: Vec<Vertex>,
    vertex_buffer: wgpu::Buffer,
    bindings: wgpu::BindGroup,
    output: wgpu::Texture,
    output_view: wgpu::TextureView,
    multisampled_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    started: Instant,
}

enum Target {
    Ps2(Box<BannerTarget>),
    Wii(Box<WiiTarget>),
    Webp(Box<WebpTarget>),
    Ctr(Box<CtrTarget>),
}

impl Target {
    fn output(&self) -> &wgpu::Texture {
        match self {
            Self::Ps2(target) => &target.output,
            Self::Wii(target) => &target.output,
            Self::Webp(target) => &target.output,
            Self::Ctr(target) => &target.output,
        }
    }
}

struct Rendered {
    image: ImageData,
    target: Target,
}

/// Rasterizer-side cache of rendered banner placeholders, keyed by the
/// placeholder image's blob id.
#[derive(Default)]
pub struct BannerTextures {
    ps2: Option<BannerRenderer>,
    wii: Option<WiiRenderer>,
    rendered: HashMap<u64, Rendered>,
}

impl BannerTextures {
    /// Draws the banner placeholder `image`. The first time, this returns the
    /// texture that should override it; afterwards the banner is redrawn into
    /// that same texture.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &ImageData,
    ) -> Option<wgpu::Texture> {
        let id = image.data.id();
        if let Some(rendered) = self.rendered.get_mut(&id) {
            match (&mut rendered.target, &self.ps2, &mut self.wii) {
                (Target::Webp(target), _, _) => target.draw(device, queue),
                (Target::Ctr(target), _, _) => target.draw(device, queue),
                (Target::Ps2(target), Some(renderer), _) => renderer.draw(device, queue, target),
                (Target::Wii(target), _, Some(renderer)) => renderer.draw(device, queue, target),
                _ => {}
            }
            return None;
        }
        let (width, height) = (image.width.max(1), image.height.max(1));
        let target = match BannerImages::banner(image)? {
            Banner::Ps2(icon) => {
                let renderer = self.ps2.get_or_insert_with(|| BannerRenderer::new(device));
                let mut target = renderer.target(device, queue, Mesh::new(&icon), width, height);
                renderer.draw(device, queue, &mut target);
                Target::Ps2(Box::new(target))
            }
            Banner::Wii(banner) => {
                let renderer = self
                    .wii
                    .get_or_insert_with(|| WiiRenderer::new(device, queue));
                let mut target = renderer.target(device, queue, banner, width, height);
                renderer.draw(device, queue, &mut target);
                Target::Wii(Box::new(target))
            }
            Banner::Webp(image) => {
                let mut target = WebpTarget::new(device, queue, &image);
                target.draw(device, queue);
                Target::Webp(Box::new(target))
            }
            Banner::Ctr(banner) => {
                let mut target = CtrTarget::new(device, queue, banner, width, height)?;
                target.draw(device, queue);
                Target::Ctr(Box::new(target))
            }
        };
        let texture = target.output().clone();
        self.rendered.insert(
            id,
            Rendered {
                image: image.clone(),
                target,
            },
        );
        Some(texture)
    }

    /// Whether `image` is a rendered banner placeholder, so the chrome
    /// containing it must be rasterized again every frame.
    pub fn is_animated(&self, image: &ImageData) -> bool {
        self.rendered.contains_key(&image.data.id())
    }

    /// Forgets placeholders whose widgets were released, returning the images
    /// whose texture overrides must be removed.
    pub fn release_stale(&mut self) -> Vec<ImageData> {
        let stale = self
            .rendered
            .keys()
            .copied()
            .filter(|id| !BannerImages::contains(*id))
            .collect::<Vec<_>>();
        stale
            .into_iter()
            .filter_map(|id| self.rendered.remove(&id))
            .map(|rendered| rendered.image)
            .collect()
    }
}
