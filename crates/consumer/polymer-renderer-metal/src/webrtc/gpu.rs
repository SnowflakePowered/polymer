//! GPU copies into IOSurface-backed VideoToolbox input. No CPU pixel mapping.
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_video::*;
use objc2_foundation::NSString;
use objc2_metal::*;
use std::{
    io,
    ptr::{self, NonNull},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

pub struct GpuInput {
    cache: CFRetained<CVMetalTextureCache>,
    pipeline: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    active: Arc<Slots>,
}
#[derive(Default)]
struct Slots {
    count: AtomicUsize,
    wake: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
pub struct GpuFrame {
    storage: Arc<FrameStorage>,
}
struct FrameStorage {
    buffer: CFRetained<CVPixelBuffer>,
    textures: Vec<CFRetained<CVMetalTexture>>,
    active: Arc<Slots>,
}
// Submitted GPU work exclusively writes this buffer. The completion callback
// transfers ownership to the encoder worker; no CPU mapping or mutation occurs.
unsafe impl Send for FrameStorage {}
unsafe impl Sync for FrameStorage {}
impl Drop for FrameStorage {
    fn drop(&mut self) {
        self.active.count.fetch_sub(1, Ordering::Release);
        let wake = self.active.wake.lock().clone();
        if let Some(wake) = wake {
            wake();
        }
    }
}
impl GpuFrame {
    pub fn buffer(&self) -> &CVPixelBuffer {
        &self.storage.buffer
    }
    pub fn extent(&self) -> (u32, u32) {
        (
            CVPixelBufferGetWidth(&self.storage.buffer) as u32,
            CVPixelBufferGetHeight(&self.storage.buffer) as u32,
        )
    }
}
impl GpuInput {
    pub fn new(device: Retained<ProtocolObject<dyn MTLDevice>>) -> io::Result<Self> {
        let mut raw = ptr::null_mut();
        unsafe {
            Self::status(CVMetalTextureCache::create(
                None,
                None,
                &device,
                None,
                NonNull::from(&mut raw),
            ))?;
        }
        let cache = unsafe {
            CFRetained::from_raw(
                NonNull::new(raw).ok_or_else(|| io::Error::other("missing Metal texture cache"))?,
            )
        };
        let library = device
            .newLibraryWithSource_options_error(
                &NSString::from_str(include_str!("convert.metal")),
                None,
            )
            .map_err(|e| io::Error::other(e.to_string()))?;
        let function = library
            .newFunctionWithName(&NSString::from_str("convert"))
            .ok_or_else(|| io::Error::other("missing conversion kernel"))?;
        let pipeline = device
            .newComputePipelineStateWithFunction_error(&function)
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self {
            cache,
            pipeline,
            active: Arc::new(Slots::default()),
        })
    }
    pub fn available(&self) -> bool {
        self.active.count.load(Ordering::Acquire) < 2
    }
    pub fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self.active.wake.lock() = Some(wake);
    }
    fn status(status: i32) -> io::Result<()> {
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "CoreVideo GPU input failed ({status})"
            )))
        }
    }
    fn allocate(&self, width: usize, height: usize) -> io::Result<GpuFrame> {
        unsafe {
            let properties = CFDictionary::<CFType, CFType>::empty();
            let attributes = CFDictionary::<CFString, CFType>::from_slices(
                &[
                    kCVPixelBufferIOSurfacePropertiesKey,
                    kCVPixelBufferMetalCompatibilityKey,
                ],
                &[properties.as_ref(), CFBoolean::new(true).as_ref()],
            );
            let mut raw = ptr::null_mut();
            Self::status(CVPixelBufferCreate(
                None,
                width,
                height,
                kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                Some(attributes.as_opaque()),
                NonNull::from(&mut raw),
            ))?;
            let buffer = CFRetained::from_raw(
                NonNull::new(raw).ok_or_else(|| io::Error::other("missing encoder buffer"))?,
            );
            buffer.set_attachment(
                kCVImageBufferColorPrimariesKey,
                kCVImageBufferColorPrimaries_ITU_R_709_2.as_ref(),
                CVAttachmentMode::ShouldPropagate,
            );
            buffer.set_attachment(
                kCVImageBufferTransferFunctionKey,
                kCVImageBufferTransferFunction_sRGB.as_ref(),
                CVAttachmentMode::ShouldPropagate,
            );
            buffer.set_attachment(
                kCVImageBufferYCbCrMatrixKey,
                kCVImageBufferYCbCrMatrix_ITU_R_709_2.as_ref(),
                CVAttachmentMode::ShouldPropagate,
            );
            let mut textures = Vec::new();
            for (plane, format, w, h) in [
                (0, MTLPixelFormat::R8Unorm, width, height),
                (1, MTLPixelFormat::RG8Unorm, width / 2, height / 2),
            ] {
                let usage = objc2_core_foundation::CFNumber::new_i64(
                    (MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite).0 as i64,
                );
                let attributes = CFDictionary::from_slices(&[kCVMetalTextureUsage], &[&*usage]);
                let mut raw = ptr::null_mut();
                Self::status(CVMetalTextureCache::create_texture_from_image(
                    None,
                    &self.cache,
                    &buffer,
                    Some(attributes.as_opaque()),
                    format,
                    w,
                    h,
                    plane,
                    NonNull::from(&mut raw),
                ))?;
                textures.push(CFRetained::from_raw(
                    NonNull::new(raw).ok_or_else(|| io::Error::other("missing encoder texture"))?,
                ));
            }
            self.active.count.fetch_add(1, Ordering::AcqRel);
            Ok(GpuFrame {
                storage: Arc::new(FrameStorage {
                    buffer,
                    textures,
                    active: self.active.clone(),
                }),
            })
        }
    }
    /// Encode a GPU copy before the command buffer signals producer release.
    /// Keep the returned frame alive until GPU completion and encoding finish.
    pub fn capture(
        &self,
        commands: &ProtocolObject<dyn MTLCommandBuffer>,
        source: &ProtocolObject<dyn MTLTexture>,
        quarter_turns: u8,
    ) -> io::Result<Option<GpuFrame>> {
        if !self.available() {
            return Ok(None);
        }
        let (w, h) = (source.width(), source.height());
        let (w, h) = if quarter_turns % 2 == 1 {
            (h, w)
        } else {
            (w, h)
        };
        let (width, height) = (w & !1, h & !1);
        if width == 0 || height == 0 {
            return Ok(None);
        }
        let frame = self.allocate(width, height)?;
        let encoder = commands
            .computeCommandEncoder()
            .ok_or_else(|| io::Error::other("missing GPU conversion encoder"))?;
        encoder.setComputePipelineState(&self.pipeline);
        unsafe {
            encoder.setTexture_atIndex(Some(source), 0);
            for (index, texture) in frame.storage.textures.iter().enumerate() {
                let texture = CVMetalTextureGetTexture(texture)
                    .ok_or_else(|| io::Error::other("missing encoder plane"))?;
                encoder.setTexture_atIndex(Some(&texture), index + 1);
            }
            let srgb = matches!(
                source.pixelFormat(),
                MTLPixelFormat::BGRA8Unorm_sRGB | MTLPixelFormat::RGBA8Unorm_sRGB
            );
            let options = [w as u32, h as u32, quarter_turns as u32, srgb as u32];
            encoder.setBytes_length_atIndex(
                NonNull::from(&options).cast(),
                std::mem::size_of_val(&options),
                0,
            );
        }
        encoder.dispatchThreads_threadsPerThreadgroup(
            MTLSize {
                width: width / 2,
                height: height / 2,
                depth: 1,
            },
            MTLSize {
                width: 8,
                height: 8,
                depth: 1,
            },
        );
        encoder.endEncoding();
        // Keep CoreVideo plane wrappers alive even if the encode job is dropped.
        // The allocation permit is returned only after GPU work and its consumer finish.
        let keep = parking_lot::Mutex::new(Some(frame.storage.clone()));
        let completed =
            block2::RcBlock::new(move |_: NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
                keep.lock().take();
            });
        unsafe {
            commands.addCompletedHandler(block2::RcBlock::as_ptr(&completed));
        }
        Ok(Some(frame))
    }
}
