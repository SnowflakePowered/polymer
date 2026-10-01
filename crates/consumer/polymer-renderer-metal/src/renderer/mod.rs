mod pass;
mod snapshot;
mod submission;
use objc2::{MainThreadMarker, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::NSView;
use objc2_metal::*;
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
pub use pass::{MetalRenderPass, MetalVertex};
use polymer_consumer_metal::ImportedMetalStream;
use polymer_protocol::{FrameReady, RELEASE_ALL_SYNC_VALUE};
use polymer_renderer::Extent;
use polymer_renderer::renderer::{
    CompositingPlan, ReceivedSurface, RenderWindow, Renderer,
    frame_ownership::{Retention, SurfaceFrames},
};
use raw_window_handle::RawWindowHandle;
use std::{collections::BTreeMap, error::Error, sync::Arc};
use submission::Submissions;

pub struct MetalFrame<'a> {
    pub connection: u64,
    pub frame: FrameReady,
    pub texture: &'a ProtocolObject<dyn MTLTexture>,
    pub quarter_turns: u8,
}

struct Surface {
    imported: ImportedMetalStream,
    frames: SurfaceFrames,
    quarter_turns: u8,
}
pub struct MetalCompositor {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    layer: Retained<CAMetalLayer>,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    surfaces: BTreeMap<u64, Surface>,
    extent: Extent,
    submissions: Arc<Submissions>,
}
impl MetalCompositor {
    pub fn new(window: &impl RenderWindow) -> Result<Self, Box<dyn Error>> {
        let _main =
            MainThreadMarker::new().ok_or("Metal view must be created on the AppKit thread")?;
        let RawWindowHandle::AppKit(handle) = window.window_handle()?.as_raw() else {
            return Err("Metal requires an AppKit view".into());
        };
        let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
        let device = MTLCreateSystemDefaultDevice().ok_or("Metal device unavailable")?;
        let value = Self::for_layer(device, window.render_extent())?;
        view.setWantsLayer(true);
        view.setLayer(Some(value.layer()));
        Ok(value)
    }

    /// A renderer-owned presentation layer for a retained native composition tree.
    pub fn for_layer(
        device: Retained<ProtocolObject<dyn MTLDevice>>,
        extent: Extent,
    ) -> Result<Self, Box<dyn Error>> {
        let queue = device.newCommandQueue().ok_or("Metal queue unavailable")?;
        Self::with_queue(device, queue, extent)
    }
    /// Share a native queue with Vello so submission order carries dependencies.
    pub fn with_queue(
        device: Retained<ProtocolObject<dyn MTLDevice>>,
        queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
        extent: Extent,
    ) -> Result<Self, Box<dyn Error>> {
        let layer = CAMetalLayer::new();
        layer.setDevice(Some(&device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setFramebufferOnly(true);
        layer.setMaximumDrawableCount(3);
        layer.setOpaque(false);
        let pipeline = pass::Pipeline::create(&device)?;
        let mut value = Self {
            device,
            queue,
            layer,
            pipeline,
            surfaces: BTreeMap::new(),
            extent,
            submissions: Arc::new(Submissions::default()),
        };
        value.resize_layer(extent);
        Ok(value)
    }
    pub fn set_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self.submissions.wake.lock() = Some(wake);
    }
    pub fn check_submission(&self) -> Result<(), Box<dyn Error>> {
        self.submissions.available()?;
        Ok(())
    }
    pub fn gpu_pending(&self) -> bool {
        self.submissions.gpu_pending()
    }
    pub fn layer(&self) -> &CAMetalLayer {
        &self.layer
    }
    pub fn resize_layer(&mut self, extent: Extent) {
        self.extent = extent;
        self.layer.setDrawableSize(objc2_foundation::NSSize::new(
            extent.width as f64,
            extent.height as f64,
        ));
    }
    pub fn device(&self) -> &ProtocolObject<dyn MTLDevice> {
        &self.device
    }
    pub fn present_with(
        &mut self,
        plan: &CompositingPlan,
        paint: impl FnOnce(&mut MetalRenderPass<'_>) -> Result<(), Box<dyn Error>>,
    ) -> Result<bool, Box<dyn Error>> {
        self.present_with_capture(plan, paint, |_, _| Ok(()))
    }
    pub fn present_with_capture(
        &mut self,
        plan: &CompositingPlan,
        paint: impl FnOnce(&mut MetalRenderPass<'_>) -> Result<(), Box<dyn Error>>,
        capture: impl FnOnce(
            &ProtocolObject<dyn MTLCommandBuffer>,
            &[MetalFrame<'_>],
        ) -> Result<(), Box<dyn Error>>,
    ) -> Result<bool, Box<dyn Error>> {
        if !self.submissions.can_submit()? {
            return Ok(false);
        }
        // Presentation may stop while the window is occluded. Continue consuming
        // guest frames on the same queue so old leases can still be released.
        let drawable =
            if self.submissions.available()? && self.extent.width != 0 && self.extent.height != 0 {
                self.layer.nextDrawable()
            } else {
                None
            };
        let commands = self
            .queue
            .commandBuffer()
            .ok_or("Metal command buffer unavailable")?;
        if let Some(drawable) = &drawable {
            self.paint_drawable(plan, &commands, drawable, paint)?;
        }
        let frames = self
            .surfaces
            .iter()
            .filter_map(|(&connection, s)| s.frames.frame().map(|frame| (connection, s, frame)))
            .map(|(connection, s, frame)| {
                Ok(MetalFrame {
                    connection,
                    frame,
                    texture: s.imported.texture(frame)?,
                    quarter_turns: s.quarter_turns,
                })
            })
            .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
        capture(&commands, &frames)?;
        for surface in self.surfaces.values() {
            if let Some(value) = surface.frames.release_value(Retention::Retained) {
                surface.imported.release(&commands, value);
            }
        }
        self.submissions.track(&commands, drawable.as_deref());
        if let Some(drawable) = &drawable {
            commands.presentDrawable(ProtocolObject::from_ref(&**drawable));
        }
        commands.commit();
        // Queue order protects producer textures; submission advances local ownership.
        for (&connection, surface) in &mut self.surfaces {
            if let Some(ready) = surface.frames.commit(Retention::Retained) {
                polymer_statistics::capture_latency(
                    connection,
                    surface.imported.stream.surface_id.0,
                    ready.elapsed(),
                );
            }
        }
        Ok(true)
    }
    fn paint_drawable(
        &self,
        plan: &CompositingPlan,
        commands: &ProtocolObject<dyn MTLCommandBuffer>,
        drawable: &ProtocolObject<dyn objc2_quartz_core::CAMetalDrawable>,
        paint: impl FnOnce(&mut MetalRenderPass<'_>) -> Result<(), Box<dyn Error>>,
    ) -> Result<(), Box<dyn Error>> {
        let description = MTLRenderPassDescriptor::new();
        // Attachment zero is within Metal's fixed color attachment array.
        let color = unsafe { description.colorAttachments().objectAtIndexedSubscript(0) };
        color.setTexture(Some(&drawable.texture()));
        color.setLoadAction(MTLLoadAction::Clear);
        color.setStoreAction(MTLStoreAction::Store);
        let clear = plan.clear_color();
        color.setClearColor(MTLClearColor {
            red: clear[0] as f64,
            green: clear[1] as f64,
            blue: clear[2] as f64,
            alpha: clear[3] as f64,
        });
        let encoder = commands
            .renderCommandEncoderWithDescriptor(&description)
            .ok_or("Metal render encoder unavailable")?;
        encoder.setRenderPipelineState(&self.pipeline);
        let mut pass = MetalRenderPass {
            device: &self.device,
            encoder: &encoder,
            extent: [self.extent.width, self.extent.height],
        };
        for placement in plan.placements() {
            let Some(surface) = self.surfaces.get(&placement.connection) else {
                continue;
            };
            let Some(frame) = surface.frames.frame() else {
                continue;
            };
            let [left, top, right, bottom] = placement.content.destination;
            let [u0, v0, u1, v1] = placement.content.source;
            let positions = [[left, top], [right, top], [right, bottom], [left, bottom]];
            let uv = [[u0, v0], [u1, v0], [u1, v1], [u0, v1]];
            let vertices = std::array::from_fn::<_, 4, _>(|index| MetalVertex {
                position: positions[index],
                uv: uv[(index + surface.quarter_turns as usize) % 4],
                color: [placement.content.opacity; 4],
                local: [positions[index][0] - left, positions[index][1] - top],
                clip: [right - left, bottom - top, placement.corner_radius, 0.0],
            });
            pass.draw(
                &vertices,
                &[0, 1, 2, 0, 2, 3],
                surface.imported.texture(frame)?,
                MTLScissorRect {
                    x: 0,
                    y: 0,
                    width: self.extent.width as usize,
                    height: self.extent.height as usize,
                },
            )?;
        }
        paint(&mut pass)?;
        encoder.endEncoding();
        Ok(())
    }
}
impl Renderer for MetalCompositor {
    fn import_surface_offer(
        &mut self,
        offer: &polymer_runtime::SurfaceOffer,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        let resources = offer
            .metal_resources()
            .ok_or("missing native Metal resources")?;
        let imported = ImportedMetalStream::new(&self.device, *offer.stream(), resources)?;
        let frames = SurfaceFrames::default();
        self.surfaces.insert(
            offer.connection_id(),
            Surface {
                imported,
                frames,
                quarter_turns,
            },
        );
        Ok(())
    }
    fn disconnect_stream(&mut self, connection: u64) -> Result<(), Box<dyn Error>> {
        if let Some(surface) = self.surfaces.remove(&connection) {
            let commands = self
                .queue
                .commandBuffer()
                .ok_or("Metal command buffer unavailable")?;
            surface.imported.release(&commands, RELEASE_ALL_SYNC_VALUE);
            commands.commit();
            // The queue retains encoded resources until the release completes.
        }
        Ok(())
    }
    fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        if let Some(surface) = self.surfaces.get_mut(&connection) {
            surface.frames.offer(frame);
        }
    }
    fn received_surfaces(&self) -> Vec<ReceivedSurface> {
        self.surfaces
            .iter()
            .filter_map(|(&connection, surface)| {
                Some(ReceivedSurface {
                    connection,
                    surface_id: surface.imported.stream.surface_id.0,
                    target_window: surface.imported.stream.source_window,
                    image_extent: polymer_renderer::renderer::image_extent(
                        surface.imported.stream.width,
                        surface.imported.stream.height,
                        surface.quarter_turns,
                    ),
                    quarter_turns: surface.quarter_turns,
                    flip_y: false,
                    frame: surface.frames.frame()?,
                })
            })
            .collect()
    }
    fn has_pending_frame(&self) -> bool {
        self.surfaces.values().any(|s| s.frames.has_pending())
    }
    fn recreate_swapchain(&mut self, window: &impl RenderWindow) -> Result<bool, Box<dyn Error>> {
        self.extent = window.render_extent();
        self.layer.setDrawableSize(objc2_foundation::NSSize::new(
            self.extent.width as f64,
            self.extent.height as f64,
        ));
        Ok(true)
    }
    fn present(&mut self, plan: &CompositingPlan) -> Result<bool, Box<dyn Error>> {
        self.present_with(plan, |_| Ok(()))
    }
}
impl Drop for MetalCompositor {
    fn drop(&mut self) {
        for key in self.surfaces.keys().copied().collect::<Vec<_>>() {
            let _ = self.disconnect_stream(key);
        }
    }
}
