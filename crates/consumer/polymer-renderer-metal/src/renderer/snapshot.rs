use super::*;

impl MetalCompositor {
    /// Read back a retained producer image without releasing its display lease.
    pub fn snapshot(
        &self,
        connection: u64,
    ) -> Result<Option<polymer_runtime::RgbaScreenshot>, Box<dyn Error>> {
        let Some(surface) = self.surfaces.get(&connection) else {
            return Ok(None);
        };
        let Some(frame) = surface.frames.frame() else {
            return Ok(None);
        };
        if !surface.imported.ready(frame.sync_value) {
            return Ok(None);
        }
        let texture = surface.imported.texture(frame)?;
        let bgra = match texture.pixelFormat() {
            MTLPixelFormat::BGRA8Unorm | MTLPixelFormat::BGRA8Unorm_sRGB => true,
            MTLPixelFormat::RGBA8Unorm | MTLPixelFormat::RGBA8Unorm_sRGB => false,
            _ => return Err("snapshot requires an RGBA8 or BGRA8 Metal surface".into()),
        };
        let width = texture.width();
        let height = texture.height();
        let stride = (width * 4).div_ceil(256) * 256;
        let buffer = self
            .device
            .newBufferWithLength_options(stride * height, MTLResourceOptions::StorageModeShared)
            .ok_or("snapshot buffer allocation failed")?;
        let commands = self
            .queue
            .commandBuffer()
            .ok_or("snapshot command buffer allocation failed")?;
        let blit = commands
            .blitCommandEncoder()
            .ok_or("snapshot blit encoder allocation failed")?;
        // The destination covers the complete texture with aligned rows and lives
        // through GPU completion; the retained frame cannot be recycled here.
        unsafe {
            blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
            texture, 0, 0, MTLOrigin { x:0,y:0,z:0 }, MTLSize { width,height,depth:1 }, &buffer, 0, stride, stride*height,
        );
        }
        blit.endEncoding();
        commands.commit();
        commands.waitUntilCompleted();
        if commands.status() == MTLCommandBufferStatus::Error {
            return Err("Metal snapshot failed".into());
        }
        let bytes = unsafe {
            std::slice::from_raw_parts(buffer.contents().as_ptr().cast::<u8>(), stride * height)
        };
        let turns = surface.quarter_turns % 4;
        let (out_width, out_height) = if turns % 2 == 1 {
            (height, width)
        } else {
            (width, height)
        };
        let mut pixels = vec![0; out_width * out_height * 4];
        for y in 0..out_height {
            for x in 0..out_width {
                // Match the UV rotation used by the presentation pass.
                let (sx, sy) = match turns {
                    0 => (x, y),
                    1 => (width - 1 - y, x),
                    2 => (width - 1 - x, height - 1 - y),
                    _ => (y, height - 1 - x),
                };
                let src = &bytes[sy * stride + sx * 4..][..4];
                let dst = &mut pixels[(y * out_width + x) * 4..][..4];
                dst.copy_from_slice(src);
                if bgra {
                    dst.swap(0, 2);
                }
            }
        }
        Ok(Some(polymer_runtime::RgbaScreenshot {
            width: out_width as u32,
            height: out_height as u32,
            pixels,
        }))
    }
}
