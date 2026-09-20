use objc2::{
    AnyThread,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
};
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use objc2_io_surface::{IOSurface, IOSurfaceRef};
use objc2_metal::*;
use polymer_presentation::Extent2D;
use std::io;

pub(super) struct Image {
    pub surface: Retained<IOSurface>,
    pub texture: Retained<ProtocolObject<dyn MTLTexture>>,
}
impl Image {
    pub fn create(
        device: &ProtocolObject<dyn MTLDevice>,
        extent: Extent2D,
        format: MTLPixelFormat,
    ) -> io::Result<Self> {
        let bytes = match format {
            MTLPixelFormat::BGRA8Unorm
            | MTLPixelFormat::BGRA8Unorm_sRGB
            | MTLPixelFormat::RGBA8Unorm
            | MTLPixelFormat::RGBA8Unorm_sRGB => 4,
            MTLPixelFormat::RGBA16Float => 8,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "unsupported shared Metal format",
                ));
            }
        };
        let keys = [
            "IOSurfaceWidth",
            "IOSurfaceHeight",
            "IOSurfaceBytesPerElement",
        ]
        .map(NSString::from_str);
        let values = [extent.width, extent.height, bytes].map(NSNumber::new_u32);
        let properties = NSDictionary::from_slices(
            &keys.iter().map(|v| &**v).collect::<Vec<_>>(),
            &values
                .iter()
                .map(|v| &**v as &AnyObject)
                .collect::<Vec<_>>(),
        );
        let surface = IOSurface::initWithProperties(IOSurface::alloc(), &properties)
            .ok_or_else(|| io::Error::other("IOSurface allocation failed"))?;
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                format,
                extent.width as usize,
                extent.height as usize,
                false,
            )
        };
        descriptor.setStorageMode(MTLStorageMode::Shared);
        descriptor.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
        // SAFETY: IOSurface's ObjC and CF representations are toll-free bridged.
        let native = unsafe { &*((&*surface as *const IOSurface).cast::<IOSurfaceRef>()) };
        let texture = device
            .newTextureWithDescriptor_iosurface_plane(&descriptor, native, 0)
            .ok_or_else(|| io::Error::other("Metal IOSurface texture allocation failed"))?;
        Ok(Self { surface, texture })
    }
}
