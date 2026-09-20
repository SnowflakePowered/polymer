use anyrender::PaintScene;
use anyrender_vello::VelloScenePainter;
use objc2::{Message, rc::Retained, runtime::ProtocolObject};
use objc2_metal::{MTLCommandBuffer, MTLCommandQueue, MTLDevice, MTLScissorRect};
use polymer_composition::{CompositionScene, RasterChunkId};
use polymer_renderer::{CompositingPlan, Extent};
use polymer_renderer_metal::{MetalCompositor, MetalVertex};
use std::{collections::HashMap, error::Error, sync::Arc};
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, wgpu};

pub(crate) struct Rasterizer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    native_queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pending: Option<(u64, Vec<(RasterChunkId, RasterLayer)>)>,
    pub wake: Option<Arc<dyn Fn() + Send + Sync>>,
    pub native: Retained<ProtocolObject<dyn MTLDevice>>,
}
pub(crate) struct RasterLayer {
    pub renderer: MetalCompositor,
    texture: wgpu::Texture,
    scene: anyrender::Scene,
    extent: Extent,
}
impl Rasterizer {
    pub async fn new() -> Result<Self, Box<dyn Error>> {
        let mut options = wgpu::InstanceDescriptor::new_without_display_handle();
        options.backends = wgpu::Backends::METAL;
        let instance = wgpu::Instance::new(options);
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await?;
        let renderer = Renderer::new(
            &device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )?;
        // The native renderers use the same physical device as Vello's textures.
        let native = unsafe {
            device
                .as_hal::<wgpu_hal::api::Metal>()
                .ok_or("Vello did not select Metal")?
                .raw_device()
                .clone()
        };
        let native_queue = unsafe {
            queue
                .as_hal::<wgpu_hal::api::Metal>()
                .ok_or("missing Metal queue")?
                .as_raw()
                .retain()
        };
        Ok(Self {
            native_queue,
            pending: None,
            wake: None,
            device,
            queue,
            renderer,
            native,
        })
    }
    pub fn waiting(&self) -> bool {
        self.pending.as_ref().is_some_and(|(_, updates)| {
            updates
                .iter()
                .any(|(_, layer)| layer.renderer.gpu_pending())
        })
    }
    pub fn render(
        &mut self,
        scene: &CompositionScene,
        layers: &mut HashMap<RasterChunkId, RasterLayer>,
    ) -> Result<bool, Box<dyn Error>> {
        if let Some((revision, updates)) = &self.pending {
            if updates
                .iter()
                .any(|(_, layer)| layer.renderer.gpu_pending())
            {
                return Ok(false);
            }
            for (_, layer) in updates {
                layer.renderer.check_submission()?;
            }
            let matches = *revision == scene.revision;
            let (_, updates) = self.pending.take().unwrap();
            if matches {
                for (id, layer) in updates {
                    if let Some(old) = layers.insert(id, layer) {
                        old.renderer.layer().removeFromSuperlayer();
                    }
                }
            }
        }
        let mut updates = Vec::new();
        for chunk in &scene.raster_chunks {
            let extent = Extent::new(
                chunk.bounds.width().ceil().max(1.0) as u32,
                chunk.bounds.height().ceil().max(1.0) as u32,
            );
            let unchanged = layers
                .get(&chunk.id)
                .is_some_and(|layer| layer.extent == extent && layer.scene == chunk.scene);
            if unchanged {
                continue;
            }
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Polymer shell fragment"),
                size: wgpu::Extent3d {
                    width: extent.width,
                    height: extent.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let mut encoded = vello::Scene::new();
            VelloScenePainter::new(&mut encoded)
                .append_scene(chunk.scene.clone(), kurbo::Affine::IDENTITY);
            self.renderer.render_to_texture(
                &self.device,
                &self.queue,
                &encoded,
                &texture.create_view(&Default::default()),
                &RenderParams {
                    base_color: vello::peniko::Color::TRANSPARENT,
                    width: extent.width,
                    height: extent.height,
                    antialiasing_method: AaConfig::Area,
                },
            )?;
            // Native draws follow Vello on its exact command queue, with no CPU wait.
            let mut renderer = MetalCompositor::with_queue(
                self.native.clone(),
                self.native_queue.clone(),
                extent,
            )?;
            if let Some(wake) = &self.wake {
                renderer.set_wake(wake.clone());
            }
            let mut layer = RasterLayer {
                renderer,
                texture,
                scene: chunk.scene.clone(),
                extent,
            };
            if !layer.present()? {
                return Ok(false);
            }
            updates.push((chunk.id, layer));
        }
        if !updates.is_empty() {
            self.pending = Some((scene.revision, updates));
            return Ok(false);
        }
        layers.retain(|id, layer| {
            let keep = scene.raster_chunks.iter().any(|chunk| chunk.id == *id);
            if !keep {
                layer.renderer.layer().removeFromSuperlayer();
            }
            keep
        });
        Ok(true)
    }
}
impl RasterLayer {
    fn present(&mut self) -> Result<bool, Box<dyn Error>> {
        // HAL guard keeps the wgpu-owned texture alive throughout native submission.
        let texture = unsafe { self.texture.as_hal::<wgpu_hal::api::Metal>() }
            .ok_or("missing native raster texture")?;
        let width = self.extent.width as f32;
        let height = self.extent.height as f32;
        let vertices =
            [[0., 0.], [width, 0.], [width, height], [0., height]].map(|position| MetalVertex {
                position,
                uv: [position[0] / width, position[1] / height],
                color: [1.; 4],
                local: [0.; 2],
                clip: [0.; 4],
            });
        let retained = self.texture.clone();
        self.renderer.present_with_capture(
            &CompositingPlan::new([0.; 4], vec![]),
            |pass| {
                pass.draw(
                    &vertices,
                    &[0, 1, 2, 0, 2, 3],
                    texture.raw_handle(),
                    MTLScissorRect {
                        x: 0,
                        y: 0,
                        width: width as usize,
                        height: height as usize,
                    },
                )?;
                Ok(())
            },
            move |commands, _| {
                // wgpu must not recycle the allocation while native sampling is queued.
                let keep = retained;
                let done = block2::RcBlock::new(
                    move |_: std::ptr::NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
                        let _ = &keep;
                    },
                );
                unsafe {
                    commands.addCompletedHandler(block2::RcBlock::as_ptr(&done));
                }
                Ok(())
            },
        )
    }
}
