use crate::{MipSource, PLACEHOLDERS, Upload};
use peniko::ImageData;
use std::collections::HashMap;
use std::sync::{Arc, Weak};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

struct Pipelines {
    layout: wgpu::BindGroupLayout,
    premultiply: wgpu::RenderPipeline,
    reduce: wgpu::RenderPipeline,
    resolve: wgpu::RenderPipeline,
    linear: wgpu::Sampler,
    nearest: wgpu::Sampler,
}

impl Pipelines {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("polymer mip shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mip.wgsl").into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("polymer mip bindings"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(
                    1,
                    wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                ),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("polymer mip layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry_point| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry_point),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry_point),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let sampler = |filter: wgpu::FilterMode, mipmap_filter| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("polymer mip sampler"),
                mag_filter: filter,
                min_filter: filter,
                mipmap_filter,
                ..Default::default()
            })
        };
        Self {
            premultiply: pipeline("premultiply"),
            reduce: pipeline("reduce"),
            resolve: pipeline("resolve"),
            linear: sampler(wgpu::FilterMode::Linear, wgpu::MipmapFilterMode::Linear),
            nearest: sampler(wgpu::FilterMode::Nearest, wgpu::MipmapFilterMode::Nearest),
            layout,
        }
    }

    fn texture(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        mip_level_count: u32,
        usage: wgpu::TextureUsages,
    ) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("polymer mip texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage,
            view_formats: &[],
        })
    }

    fn level(texture: &wgpu::Texture, level: u32) -> wgpu::TextureView {
        texture.create_view(&wgpu::TextureViewDescriptor {
            base_mip_level: level,
            mip_level_count: Some(1),
            ..Default::default()
        })
    }

    fn pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        (source, sampler): (&wgpu::TextureView, &wgpu::Sampler),
        target: &wgpu::TextureView,
    ) {
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("polymer mip bindings"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("polymer mip pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
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
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Builds the premultiplied mip chain of `upload` and keeps only its
    /// levels from `upload.level` down.
    fn chain(&self, device: &wgpu::Device, queue: &wgpu::Queue, upload: Upload) -> wgpu::Texture {
        let (width, height) = (upload.pixels.width, upload.pixels.height);
        let levels = u32::BITS - width.max(height).leading_zeros();
        let kept = upload.level.min(levels - 1);
        let staging = Self::texture(
            device,
            width,
            height,
            1,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        queue.write_texture(
            staging.as_image_copy(),
            upload.pixels.data.data(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            staging.size(),
        );
        drop(upload);
        let full = Self::texture(
            device,
            width,
            height,
            levels,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC,
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("polymer mip chain"),
        });
        self.pass(
            device,
            &mut encoder,
            &self.premultiply,
            (&staging.create_view(&Default::default()), &self.nearest),
            &Self::level(&full, 0),
        );
        for level in 1..levels {
            self.pass(
                device,
                &mut encoder,
                &self.reduce,
                (&Self::level(&full, level - 1), &self.nearest),
                &Self::level(&full, level),
            );
        }
        if kept == 0 {
            queue.submit([encoder.finish()]);
            return full;
        }
        let size = |level: u32| ((width >> level).max(1), (height >> level).max(1));
        let (kept_width, kept_height) = size(kept);
        let chain = Self::texture(
            device,
            kept_width,
            kept_height,
            levels - kept,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        for level in kept..levels {
            let (level_width, level_height) = size(level);
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    mip_level: level,
                    ..full.as_image_copy()
                },
                wgpu::TexelCopyTextureInfo {
                    mip_level: level - kept,
                    ..chain.as_image_copy()
                },
                wgpu::Extent3d {
                    width: level_width,
                    height: level_height,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit([encoder.finish()]);
        chain
    }
}

struct Chain {
    source: Weak<MipSource>,
    texture: wgpu::Texture,
    revision: u64,
}

struct Resolved {
    image: ImageData,
    texture: wgpu::Texture,
    revision: Option<u64>,
    last_used: u64,
}

/// Rasterizer-side mip chains, and the placeholder textures resampled from
/// them at each placeholder's size.
#[derive(Default)]
pub struct MipTextures {
    pipelines: Option<Pipelines>,
    chains: HashMap<u64, Chain>,
    resolved: HashMap<u64, Resolved>,
    generation: u64,
    revisions: u64,
    frame: u64,
}

impl MipTextures {
    /// Starts a raster frame painted in `generation`, returning the
    /// placeholder images whose overrides must be removed because a newer
    /// generation evicted every chain.
    pub fn begin_frame(&mut self, generation: u64) -> Vec<ImageData> {
        self.frame += 1;
        if generation == self.generation {
            return Vec::new();
        }
        self.generation = generation;
        self.chains.clear();
        self.resolved
            .drain()
            .map(|(_, resolved)| resolved.image)
            .collect()
    }

    /// Resolves the placeholder `image`, returning the texture that should
    /// override it whenever its contents changed.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &ImageData,
    ) -> Option<wgpu::Texture> {
        if !image.data.data().is_empty() {
            return None;
        }
        let id = image.data.id();
        let placement = PLACEHOLDERS
            .get(&id)
            .map(|placement| (placement.source.clone(), placement.nearest));
        let pipelines = self.pipelines.get_or_insert_with(|| Pipelines::new(device));
        let chain = placement.as_ref().and_then(|(source, _)| {
            if let Some(upload) = source.take() {
                self.revisions += 1;
                self.chains.insert(
                    source.id,
                    Chain {
                        source: Arc::downgrade(source),
                        texture: pipelines.chain(device, queue, upload),
                        revision: self.revisions,
                    },
                );
            }
            self.chains.get(&source.id)
        });
        let mut created = false;
        let resolved = self.resolved.entry(id).or_insert_with(|| {
            created = true;
            Resolved {
                image: image.clone(),
                texture: Pipelines::texture(
                    device,
                    image.width.max(1),
                    image.height.max(1),
                    1,
                    wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                ),
                revision: None,
                last_used: 0,
            }
        });
        resolved.last_used = self.frame;
        let Some((chain, (_, nearest))) = chain
            .zip(placement)
            .filter(|(chain, _)| resolved.revision != Some(chain.revision))
        else {
            return created.then(|| resolved.texture.clone());
        };
        let sampler = if nearest {
            &pipelines.nearest
        } else {
            &pipelines.linear
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("polymer mip resolve"),
        });
        pipelines.pass(
            device,
            &mut encoder,
            &pipelines.resolve,
            (&chain.texture.create_view(&Default::default()), sampler),
            &resolved.texture.create_view(&Default::default()),
        );
        queue.submit([encoder.finish()]);
        resolved.revision = Some(chain.revision);
        Some(resolved.texture.clone())
    }

    /// Forgets released placeholders and chains, returning the images whose
    /// texture overrides must be removed.
    pub fn release_stale(&mut self) -> Vec<ImageData> {
        self.chains
            .retain(|_, chain| chain.source.strong_count() > 0);
        let stale = self
            .resolved
            .iter()
            .filter(|(id, resolved)| {
                resolved.last_used != self.frame && !PLACEHOLDERS.contains_key(*id)
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        stale
            .into_iter()
            .filter_map(|id| self.resolved.remove(&id))
            .map(|resolved| resolved.image)
            .collect()
    }
}
