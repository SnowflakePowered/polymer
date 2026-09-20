//! Egui meshes appended to the existing Metal compositor pass.
use crate::{MetalCompositor, MetalRenderPass, MetalVertex};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::*;
use polymer_renderer::egui::EguiPainter;
use std::{collections::HashMap, error::Error};

#[derive(Default)]
pub struct MetalEguiPainter {
    textures: HashMap<egui::TextureId, Texture>,
}
struct Texture {
    native: Retained<ProtocolObject<dyn MTLTexture>>,
    pixels: Vec<u8>,
}
impl EguiPainter for MetalEguiPainter {
    type Compositor = MetalCompositor;
    type RenderPass<'frame> = MetalRenderPass<'frame>;

    fn paint(
        &mut self,
        pass: &mut MetalRenderPass<'_>,
        output: &egui::FullOutput,
        primitives: &[egui::ClippedPrimitive],
    ) -> Result<(), Box<dyn Error>> {
        for (id, delta) in &output.textures_delta.set {
            let pixels: Vec<u8> = match &delta.image {
                egui::ImageData::Color(image) => {
                    image.pixels.iter().flat_map(|p| p.to_array()).collect()
                }
                egui::ImageData::Font(image) => image
                    .srgba_pixels(None)
                    .flat_map(|p| p.to_array())
                    .collect(),
            };
            let [width, height] = delta.image.size();
            let (pixels, width, height) = if let Some([x, y]) = delta.pos {
                let old = self
                    .textures
                    .get(id)
                    .ok_or("egui texture update has no original")?;
                let (w, h) = (old.native.width(), old.native.height());
                if x + width > w || y + height > h {
                    return Err("egui texture update exceeds allocation".into());
                }
                let mut updated = old.pixels.clone();
                for row in 0..height {
                    updated[((row + y) * w + x) * 4..((row + y) * w + x + width) * 4]
                        .copy_from_slice(&pixels[row * width * 4..(row + 1) * width * 4]);
                }
                (updated, w, h)
            } else {
                (pixels, width, height)
            };
            // Immutable per submission: updating an atlas must not overwrite
            // a texture still sampled by an asynchronous command buffer.
            let description = unsafe {
                MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                    MTLPixelFormat::RGBA8Unorm,
                    width,
                    height,
                    false,
                )
            };
            description.setStorageMode(MTLStorageMode::Shared);
            description.setUsage(MTLTextureUsage::ShaderRead);
            let native = pass
                .device
                .newTextureWithDescriptor(&description)
                .ok_or("egui Metal texture allocation failed")?;
            unsafe {
                native.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                    MTLRegion {
                        origin: MTLOrigin { x: 0, y: 0, z: 0 },
                        size: MTLSize {
                            width,
                            height,
                            depth: 1,
                        },
                    },
                    0,
                    std::ptr::NonNull::new(pixels.as_ptr().cast_mut().cast()).unwrap(),
                    width * 4,
                );
            }
            self.textures.insert(*id, Texture { native, pixels });
        }
        let scale = output.pixels_per_point;
        for primitive in primitives {
            let egui::epaint::Primitive::Mesh(mesh) = &primitive.primitive else {
                return Err("custom egui paint callbacks are not supported by Metal".into());
            };
            let texture = self
                .textures
                .get(&mesh.texture_id)
                .ok_or("egui texture is unavailable")?;
            let vertices = mesh
                .vertices
                .iter()
                .map(|v| MetalVertex {
                    position: [v.pos.x * scale, v.pos.y * scale],
                    uv: [v.uv.x, v.uv.y],
                    color: v.color.to_array().map(|c| c as f32 / 255.0),
                    local: [0.0; 2],
                    clip: [0.0; 4],
                })
                .collect::<Vec<_>>();
            let clip = primitive.clip_rect;
            let x = (clip.min.x * scale).max(0.0).min(pass.extent[0] as f32) as usize;
            let y = (clip.min.y * scale).max(0.0).min(pass.extent[1] as f32) as usize;
            let right = (clip.max.x * scale)
                .max(x as f32)
                .min(pass.extent[0] as f32) as usize;
            let bottom = (clip.max.y * scale)
                .max(y as f32)
                .min(pass.extent[1] as f32) as usize;
            pass.draw(
                &vertices,
                &mesh.indices,
                &texture.native,
                MTLScissorRect {
                    x,
                    y,
                    width: right - x,
                    height: bottom - y,
                },
            )?;
        }
        for id in &output.textures_delta.free {
            self.textures.remove(id);
        }
        Ok(())
    }
}
