use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::*;
use objc2_core_video::*;
use objc2_video_toolbox::*;
use polymer_renderer::webrtc::Codec;
use std::{
    ffi::c_void,
    io,
    ptr::{self, NonNull},
    sync::mpsc,
    time::Duration,
};

type Output = io::Result<Vec<u8>>;
pub struct Encoder {
    session: CFRetained<VTCompressionSession>,
    _callback: Box<mpsc::Sender<Output>>,
    output: mpsc::Receiver<Output>,
    shape: (u32, u32, Codec),
    sequence: i64,
}
// Session access is serialized by its owner's Mutex. VideoToolbox callbacks
// touch only the independent, heap-stable channel sender, never the session.
unsafe impl Send for Encoder {}
impl Encoder {
    pub fn new(width: u32, height: u32, codec: Codec, bitrate: u32, rate: f32) -> io::Result<Self> {
        let (sender, output) = mpsc::channel();
        let mut callback = Box::new(sender);
        let mut native = ptr::null_mut();
        unsafe {
            let hardware = CFDictionary::from_slices(
                &[kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder],
                &[CFBoolean::new(true)],
            );
            Self::status(
                VTCompressionSession::create(
                    None,
                    width.try_into().map_err(io::Error::other)?,
                    height.try_into().map_err(io::Error::other)?,
                    match codec {
                        Codec::H264 => kCMVideoCodecType_H264,
                        Codec::Hevc => kCMVideoCodecType_HEVC,
                    },
                    Some(hardware.as_opaque()),
                    None,
                    None,
                    Some(Self::completed),
                    (&mut *callback as *mut mpsc::Sender<Output>).cast(),
                    NonNull::from(&mut native),
                ),
                "create hardware encoder",
            )?;
            let session = CFRetained::from_raw(
                NonNull::new(native)
                    .ok_or_else(|| io::Error::other("VideoToolbox returned no session"))?,
            );
            let encoder = Self {
                session,
                _callback: callback,
                output,
                shape: (width, height, codec),
                sequence: 0,
            };
            encoder.property(
                kVTCompressionPropertyKey_ColorPrimaries,
                kCVImageBufferColorPrimaries_ITU_R_709_2.as_ref(),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_TransferFunction,
                kCVImageBufferTransferFunction_sRGB.as_ref(),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_YCbCrMatrix,
                kCVImageBufferYCbCrMatrix_ITU_R_709_2.as_ref(),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_RealTime,
                CFBoolean::new(true).as_ref(),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_AllowFrameReordering,
                CFBoolean::new(false).as_ref(),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_AverageBitRate,
                CFNumber::new_i64(bitrate as i64).as_ref(),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_ExpectedFrameRate,
                CFNumber::new_f32(rate).as_ref(),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                CFNumber::new_i32((rate * 2.).round() as i32).as_ref(),
            )?;
            Self::status(
                encoder.session.prepare_to_encode_frames(),
                "prepare hardware encoder",
            )?;
            Ok(encoder)
        }
    }
    pub fn matches(&self, width: u32, height: u32, codec: Codec) -> bool {
        self.shape == (width, height, codec)
    }
    fn property(&self, key: &CFString, value: &CFType) -> io::Result<()> {
        // VTCompressionSession is a VTSession subtype in the C API.
        unsafe {
            Self::status(
                VTSessionSetProperty(
                    &*((&*self.session as *const VTCompressionSession).cast::<VTSession>()),
                    key,
                    Some(value),
                ),
                "configure hardware encoder",
            )
        }
    }
    fn status(status: i32, operation: &str) -> io::Result<()> {
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "VideoToolbox {operation} failed ({status})"
            )))
        }
    }
    pub fn encode(&mut self, buffer: &CVPixelBuffer, rate: f32) -> io::Result<Vec<u8>> {
        if CVPixelBufferGetWidth(buffer) != self.shape.0 as usize
            || CVPixelBufferGetHeight(buffer) != self.shape.1 as usize
        {
            return Err(io::Error::other("encoder extent mismatch"));
        }
        unsafe {
            let timescale = rate.round().max(1.) as i32;
            Self::status(
                self.session.encode_frame(
                    buffer,
                    CMTime::new(self.sequence, timescale),
                    CMTime::new(1, timescale),
                    None,
                    ptr::null_mut(),
                    ptr::null_mut(),
                ),
                "submit frame",
            )?;
            self.sequence += 1;
        }
        self.output
            .recv_timeout(Duration::from_secs(5))
            .map_err(io::Error::other)?
    }
    unsafe extern "C-unwind" fn completed(
        context: *mut c_void,
        _frame: *mut c_void,
        status: i32,
        _flags: VTEncodeInfoFlags,
        sample: *mut CMSampleBuffer,
    ) {
        // The session owns this sender until invalidation has joined callbacks.
        let sender = unsafe { &*context.cast::<mpsc::Sender<Output>>() };
        let result = Self::status(status, "encode frame").and_then(|()| {
            let sample = unsafe { sample.as_ref() }
                .ok_or_else(|| io::Error::other("hardware encoder dropped frame"))?;
            Self::annex_b(sample)
        });
        let _ = sender.send(result);
    }
    fn annex_b(sample: &CMSampleBuffer) -> io::Result<Vec<u8>> {
        unsafe {
            let format = sample
                .format_description()
                .ok_or_else(|| io::Error::other("missing encoded format"))?;
            let hevc = format.media_sub_type() == kCMVideoCodecType_HEVC;
            let get = if hevc {
                CMVideoFormatDescriptionGetHEVCParameterSetAtIndex
            } else {
                CMVideoFormatDescriptionGetH264ParameterSetAtIndex
            };
            let mut output = Vec::new();
            let mut count = 1;
            let mut index = 0;
            let mut header = 0;
            while index < count {
                let mut bytes = ptr::null();
                let mut length = 0;
                Self::status(
                    get(
                        &format,
                        index,
                        &mut bytes,
                        &mut length,
                        &mut count,
                        &mut header,
                    ),
                    "read codec parameters",
                )?;
                if bytes.is_null() {
                    return Err(io::Error::other("missing codec parameter data"));
                }
                output.extend_from_slice(&[0, 0, 0, 1]);
                output.extend_from_slice(std::slice::from_raw_parts(bytes, length));
                index += 1;
            }
            if !(1..=4).contains(&header) {
                return Err(io::Error::other("invalid codec NAL length"));
            }
            let data = sample
                .data_buffer()
                .ok_or_else(|| io::Error::other("missing encoded bytes"))?;
            let mut bytes = vec![0; data.data_length()];
            Self::status(
                data.copy_data_bytes(
                    0,
                    bytes.len(),
                    NonNull::new(bytes.as_mut_ptr()).unwrap().cast(),
                ),
                "read encoded bytes",
            )?;
            let mut remaining = bytes.as_slice();
            while !remaining.is_empty() {
                let prefix = remaining
                    .get(..header as usize)
                    .ok_or_else(|| io::Error::other("truncated NAL length"))?;
                let length = prefix.iter().fold(0usize, |n, b| (n << 8) | *b as usize);
                remaining = &remaining[header as usize..];
                let nal = remaining
                    .get(..length)
                    .ok_or_else(|| io::Error::other("truncated encoded NAL"))?;
                output.extend_from_slice(&[0, 0, 0, 1]);
                output.extend_from_slice(nal);
                remaining = &remaining[length..];
            }
            Ok(output)
        }
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            self.session.invalidate();
        }
    }
}
