#![cfg(target_os = "macos")]
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained};
use objc2_core_media::*;
use objc2_core_video::*;
use objc2_metal::*;
use objc2_video_toolbox::*;
use polymer_renderer::webrtc::Codec;
use polymer_renderer_metal::webrtc::encoder::Encoder;
use polymer_renderer_metal::webrtc::gpu::GpuInput;
use std::{
    ffi::c_void,
    ptr::{self, NonNull},
    sync::mpsc,
    time::Duration,
};

#[test]
#[ignore = "requires VideoToolbox hardware H.264 and HEVC encoders"]
fn hardware_frames_decode_with_the_original_colors_and_order() {
    let device = MTLCreateSystemDefaultDevice().unwrap();
    let queue = device.newCommandQueue().unwrap();
    let input = GpuInput::new(device.clone()).unwrap();
    let descriptor = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::BGRA8Unorm,
            64,
            48,
            false,
        )
    };
    descriptor.setStorageMode(MTLStorageMode::Private);
    descriptor.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
    let texture = device.newTextureWithDescriptor(&descriptor).unwrap();
    for codec in [Codec::H264, Codec::Hevc] {
        let mut encoder = Encoder::new(64, 48, codec, 2_000_000, 60.).unwrap();
        let mut frames = Vec::new();
        let colors = [
            [190, 45, 70, 255],
            [60, 170, 90, 255],
            [35, 80, 200, 255],
            [210, 170, 40, 255],
        ];
        for color in colors {
            let commands = queue.commandBuffer().unwrap();
            let pass = MTLRenderPassDescriptor::new();
            let attachment = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
            attachment.setTexture(Some(&texture));
            attachment.setLoadAction(MTLLoadAction::Clear);
            attachment.setStoreAction(MTLStoreAction::Store);
            attachment.setClearColor(MTLClearColor {
                red: color[0] as f64 / 255.,
                green: color[1] as f64 / 255.,
                blue: color[2] as f64 / 255.,
                alpha: 1.,
            });
            commands
                .renderCommandEncoderWithDescriptor(&pass)
                .unwrap()
                .endEncoding();
            let frame = input.capture(&commands, &texture, 0).unwrap().unwrap();
            commands.commit();
            // This hardware test joins the GPU before asking the native decoder
            // to verify pixels; production uses the completion callback instead.
            commands.waitUntilCompleted();
            assert_eq!(commands.status(), MTLCommandBufferStatus::Completed);
            frames.push(encoder.encode(frame.buffer(), 60.).unwrap());
        }
        let decoded = Decoder::decode(codec, &frames);
        assert_eq!(decoded.len(), colors.len());
        for (pixel, color) in decoded.into_iter().zip(colors) {
            for channel in 0..3 {
                assert!(
                    (pixel[channel] as i16 - color[channel] as i16).abs() < 12,
                    "{codec:?}: decoded {pixel:?}, expected {color:?}"
                );
            }
            assert_eq!(pixel[3], 255);
        }
    }
}

struct Decoder {
    session: CFRetained<VTDecompressionSession>,
    _sender: Box<mpsc::Sender<(i32, Option<[u8; 4]>)>>,
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            self.session.invalidate();
        }
    }
}
impl Decoder {
    fn units(bytes: &[u8]) -> Vec<&[u8]> {
        let mut starts = bytes
            .windows(4)
            .enumerate()
            .filter_map(|(i, b)| (b == [0, 0, 0, 1]).then_some(i))
            .collect::<Vec<_>>();
        starts.push(bytes.len());
        starts
            .windows(2)
            .map(|pair| &bytes[pair[0] + 4..pair[1]])
            .collect()
    }
    fn parameter(codec: Codec, nal: &[u8]) -> bool {
        match codec {
            Codec::H264 => matches!(nal[0] & 31, 7 | 8),
            Codec::Hevc => matches!((nal[0] >> 1) & 63, 32..=34),
        }
    }
    fn decode(codec: Codec, frames: &[Vec<u8>]) -> Vec<[u8; 4]> {
        unsafe {
            let params = Self::units(&frames[0])
                .into_iter()
                .filter(|n| Self::parameter(codec, n))
                .collect::<Vec<_>>();
            let mut pointers = params
                .iter()
                .map(|n| NonNull::new(n.as_ptr().cast_mut()).unwrap())
                .collect::<Vec<_>>();
            let mut sizes = params.iter().map(|n| n.len()).collect::<Vec<_>>();
            let mut raw = ptr::null();
            let status = match codec {
                Codec::H264 => CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    None,
                    params.len(),
                    NonNull::new(pointers.as_mut_ptr()).unwrap(),
                    NonNull::new(sizes.as_mut_ptr()).unwrap(),
                    4,
                    NonNull::from(&mut raw),
                ),
                Codec::Hevc => CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    None,
                    params.len(),
                    NonNull::new(pointers.as_mut_ptr()).unwrap(),
                    NonNull::new(sizes.as_mut_ptr()).unwrap(),
                    4,
                    None,
                    NonNull::from(&mut raw),
                ),
            };
            assert_eq!(status, 0);
            let format = CFRetained::from_raw(NonNull::new(raw.cast_mut()).unwrap());
            let (sender, output) = mpsc::channel::<(i32, Option<[u8; 4]>)>();
            let mut sender = Box::new(sender);
            let callback = VTDecompressionOutputCallbackRecord {
                decompressionOutputCallback: Some(Self::received),
                decompressionOutputRefCon: (&mut *sender as *mut mpsc::Sender<_>).cast(),
            };
            let attributes = CFDictionary::from_slices(
                &[kCVPixelBufferPixelFormatTypeKey],
                &[&*CFNumber::new_i32(kCVPixelFormatType_32BGRA as i32)],
            );
            let mut raw = ptr::null_mut();
            assert_eq!(
                VTDecompressionSession::create(
                    None,
                    &format,
                    None,
                    Some(attributes.as_opaque()),
                    &callback,
                    NonNull::from(&mut raw)
                ),
                0
            );
            let decoder = Self {
                session: CFRetained::from_raw(NonNull::new(raw).unwrap()),
                _sender: sender,
            };
            let mut decoded = Vec::new();
            for frame in frames {
                let mut avcc = Vec::new();
                for nal in Self::units(frame)
                    .into_iter()
                    .filter(|n| !Self::parameter(codec, n))
                {
                    avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                    avcc.extend_from_slice(nal);
                }
                let sample = Self::sample(&avcc, &format);
                assert_eq!(
                    decoder.session.decode_frame(
                        &sample,
                        VTDecodeFrameFlags::empty(),
                        ptr::null_mut(),
                        ptr::null_mut()
                    ),
                    0
                );
                assert_eq!(decoder.session.wait_for_asynchronous_frames(), 0);
                let (status, pixel) = output.recv_timeout(Duration::from_secs(5)).unwrap();
                assert_eq!(status, 0);
                decoded.push(pixel.unwrap());
            }
            decoded
        }
    }
    unsafe fn sample(bytes: &[u8], format: &CMFormatDescription) -> CFRetained<CMSampleBuffer> {
        unsafe {
            let mut raw = ptr::null_mut();
            assert_eq!(
                CMBlockBuffer::create_with_memory_block(
                    None,
                    ptr::null_mut(),
                    bytes.len(),
                    None,
                    ptr::null(),
                    0,
                    bytes.len(),
                    0,
                    NonNull::from(&mut raw)
                ),
                0
            );
            let block = CFRetained::from_raw(NonNull::new(raw).unwrap());
            assert_eq!(
                CMBlockBuffer::replace_data_bytes(
                    NonNull::new(bytes.as_ptr().cast_mut()).unwrap().cast(),
                    &block,
                    0,
                    bytes.len()
                ),
                0
            );
            let mut raw = ptr::null_mut();
            assert_eq!(
                CMSampleBuffer::create_ready(
                    None,
                    Some(&block),
                    Some(format),
                    1,
                    0,
                    ptr::null(),
                    1,
                    &bytes.len(),
                    NonNull::from(&mut raw)
                ),
                0
            );
            CFRetained::from_raw(NonNull::new(raw).unwrap())
        }
    }
    unsafe extern "C-unwind" fn received(
        context: *mut c_void,
        _source: *mut c_void,
        status: i32,
        _flags: VTDecodeInfoFlags,
        image: *mut CVImageBuffer,
        _time: CMTime,
        _duration: CMTime,
    ) {
        unsafe {
            let sender = &*context.cast::<mpsc::Sender<(i32, Option<[u8; 4]>)>>();
            let Some(image) = image.as_ref() else {
                let _ = sender.send((status, None));
                return;
            };
            let lock = CVPixelBufferLockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly);
            if lock != 0 {
                let _ = sender.send((lock, None));
                return;
            }
            let bytes = CVPixelBufferGetBaseAddress(image).cast::<u8>();
            let offset = (CVPixelBufferGetHeight(image) / 2) * CVPixelBufferGetBytesPerRow(image)
                + (CVPixelBufferGetWidth(image) / 2) * 4;
            let pixel = std::slice::from_raw_parts(bytes.add(offset), 4);
            let rgba = [pixel[2], pixel[1], pixel[0], pixel[3]];
            CVPixelBufferUnlockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly);
            let _ = sender.send((status, Some(rgba)));
        }
    }
}
