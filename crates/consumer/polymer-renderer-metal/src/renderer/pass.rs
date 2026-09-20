use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::NSString;
use objc2_metal::*;
use std::{io, ptr::NonNull};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MetalVertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
    pub color: [f32; 4],
    pub local: [f32; 2],
    pub clip: [f32; 4],
}
pub struct MetalRenderPass<'a> {
    pub device: &'a ProtocolObject<dyn MTLDevice>,
    pub encoder: &'a ProtocolObject<dyn MTLRenderCommandEncoder>,
    pub extent: [u32; 2],
}
impl MetalRenderPass<'_> {
    pub fn draw(
        &self,
        vertices: &[MetalVertex],
        indices: &[u32],
        texture: &ProtocolObject<dyn MTLTexture>,
        scissor: MTLScissorRect,
    ) -> io::Result<()> {
        if vertices.is_empty() || indices.is_empty() || scissor.width == 0 || scissor.height == 0 {
            return Ok(());
        }
        unsafe {
            let vertices = self
                .device
                .newBufferWithBytes_length_options(
                    NonNull::new(vertices.as_ptr().cast_mut().cast()).unwrap(),
                    std::mem::size_of_val(vertices),
                    MTLResourceOptions::StorageModeShared,
                )
                .ok_or_else(|| io::Error::other("Metal vertex allocation failed"))?;
            let index = self
                .device
                .newBufferWithBytes_length_options(
                    NonNull::new(indices.as_ptr().cast_mut().cast()).unwrap(),
                    std::mem::size_of_val(indices),
                    MTLResourceOptions::StorageModeShared,
                )
                .ok_or_else(|| io::Error::other("Metal index allocation failed"))?;
            let extent = [self.extent[0] as f32, self.extent[1] as f32];
            self.encoder
                .setVertexBuffer_offset_atIndex(Some(&vertices), 0, 0);
            self.encoder
                .setVertexBytes_length_atIndex(NonNull::from(&extent).cast(), 8, 1);
            self.encoder.setFragmentTexture_atIndex(Some(texture), 0);
            self.encoder.setScissorRect(scissor);
            self.encoder
                .drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset(
                    MTLPrimitiveType::Triangle,
                    indices.len(),
                    MTLIndexType::UInt32,
                    &index,
                    0,
                );
        }
        Ok(())
    }
}
pub(super) struct Pipeline;
impl Pipeline {
    pub fn create(
        device: &ProtocolObject<dyn MTLDevice>,
    ) -> io::Result<Retained<ProtocolObject<dyn MTLRenderPipelineState>>> {
        let library = device
            .newLibraryWithSource_options_error(
                &NSString::from_str(include_str!("shader.metal")),
                None,
            )
            .map_err(|e| io::Error::other(e.to_string()))?;
        let description = MTLRenderPipelineDescriptor::new();
        description.setVertexFunction(
            library
                .newFunctionWithName(&NSString::from_str("vertex_main"))
                .as_deref(),
        );
        description.setFragmentFunction(
            library
                .newFunctionWithName(&NSString::from_str("fragment_main"))
                .as_deref(),
        );
        // Attachment zero is within Metal's fixed color attachment array.
        let color = unsafe { description.colorAttachments().objectAtIndexedSubscript(0) };
        color.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        color.setBlendingEnabled(true);
        color.setSourceRGBBlendFactor(MTLBlendFactor::One);
        color.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        color.setSourceAlphaBlendFactor(MTLBlendFactor::One);
        color.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        device
            .newRenderPipelineStateWithDescriptor_error(&description)
            .map_err(|e| io::Error::other(e.to_string()))
    }
}
