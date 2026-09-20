//! Native Metal import of the shared virtual swapchain's stream resources.
#![cfg(target_os = "macos")]
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_io_surface::{IOSurface, IOSurfaceRef};
use objc2_metal::*;
use polymer_ipc::{StreamReady, xpc::MetalResources};
use polymer_protocol::{ExternalImageKind, ExternalSyncKind, FrameReady};
use std::io;

pub struct ImportedMetalStream {
    pub stream: StreamReady,
    textures: Vec<Retained<ProtocolObject<dyn MTLTexture>>>,
    ready: Retained<ProtocolObject<dyn MTLSharedEvent>>,
    released: Retained<ProtocolObject<dyn MTLSharedEvent>>,
}
impl ImportedMetalStream {
    pub fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        stream: StreamReady,
        resources: MetalResources,
    ) -> io::Result<Self> {
        if stream.image_kind != ExternalImageKind::IOSurface
            || stream.sync_kind != ExternalSyncKind::MetalSharedEvent
            || stream.slot_count as usize != resources.surfaces.len()
            || resources.surfaces.is_empty()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid Metal stream",
            ));
        }
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat(stream.format as usize),
                stream.width as usize,
                stream.height as usize,
                false,
            )
        };
        descriptor.setStorageMode(MTLStorageMode::Shared);
        descriptor.setUsage(MTLTextureUsage::ShaderRead);
        let textures = resources
            .surfaces
            .iter()
            .map(|surface| {
                if surface.width() != stream.width as isize
                    || surface.height() != stream.height as isize
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "IOSurface extent mismatch",
                    ));
                }
                // SAFETY: IOSurface is toll-free bridged to IOSurfaceRef.
                let native = unsafe { &*((&**surface as *const IOSurface).cast::<IOSurfaceRef>()) };
                device
                    .newTextureWithDescriptor_iosurface_plane(&descriptor, native, 0)
                    .ok_or_else(|| io::Error::other("Metal texture import failed"))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let ready = device
            .newSharedEventWithHandle(&resources.ready)
            .ok_or_else(|| io::Error::other("producer event import failed"))?;
        let released = device
            .newSharedEventWithHandle(&resources.released)
            .ok_or_else(|| io::Error::other("consumer event import failed"))?;
        Ok(Self {
            stream,
            textures,
            ready,
            released,
        })
    }
    pub fn texture(&self, frame: FrameReady) -> io::Result<&ProtocolObject<dyn MTLTexture>> {
        self.textures
            .get(frame.slot as usize)
            .map(|value| &**value)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "Metal frame slot out of bounds")
            })
    }
    pub fn ready(&self, value: u64) -> bool {
        self.ready.signaledValue() >= value
    }
    pub fn wait(&self, commands: &ProtocolObject<dyn MTLCommandBuffer>, value: u64) {
        commands.encodeWaitForEvent_value(ProtocolObject::from_ref(&*self.ready), value);
    }
    pub fn release(&self, commands: &ProtocolObject<dyn MTLCommandBuffer>, value: u64) {
        commands.encodeSignalEvent_value(ProtocolObject::from_ref(&*self.released), value);
    }
}
