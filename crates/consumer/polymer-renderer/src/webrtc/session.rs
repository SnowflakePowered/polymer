use super::{Codec, EncodeError, EncodedFrame, VideoFrame};
use parking_lot::Mutex;
use std::sync::Arc;

pub trait NativeEncoder: Sized + Send + 'static {
    type Frame: Send + 'static;
    const BACKEND: &'static str;

    fn new_for_frame(
        frame: &Self::Frame,
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: f32,
        preferred: Codec,
    ) -> Result<Self, EncodeError>;

    fn codec(&self) -> Codec;

    fn encode_frame(
        &mut self,
        frame: &mut Self::Frame,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, EncodeError>;

    fn has_in_flight_work(&self) -> bool {
        false
    }
}

pub struct EncoderSession<E> {
    native: Option<E>,
    requested: Option<Codec>,
    failed: bool,
}

impl<E> Default for EncoderSession<E> {
    fn default() -> Self {
        Self {
            native: None,
            requested: None,
            failed: false,
        }
    }
}

impl<E: NativeEncoder> EncoderSession<E> {
    pub fn frame(
        session: Arc<Mutex<Self>>,
        connection: u64,
        width: u32,
        height: u32,
        mut frame: E::Frame,
    ) -> VideoFrame {
        VideoFrame::new(
            connection,
            width,
            height,
            move |width, height, preferred, bitrate, rate| {
                session
                    .lock()
                    .encode(&mut frame, width, height, preferred, bitrate, rate)
            },
        )
    }

    fn encode(
        &mut self,
        frame: &mut E::Frame,
        width: u32,
        height: u32,
        preferred: Codec,
        bitrate: u32,
        rate: f32,
    ) -> Result<EncodedFrame, EncodeError> {
        if self.failed {
            return Err("hardware encoding disabled after GPU failure".into());
        }
        if self.requested != Some(preferred) {
            self.native = None;
            self.requested = Some(preferred);
        }
        let encoder = match &mut self.native {
            Some(encoder) => encoder,
            native => native.insert(E::new_for_frame(
                frame, width, height, bitrate, rate, preferred,
            )?),
        };
        match encoder.encode_frame(frame, width, height) {
            Ok(data) => Ok(EncodedFrame {
                backend: E::BACKEND,
                codec: encoder.codec(),
                data,
            }),
            Err(error) => {
                self.failed = true;
                if let Some(stalled) = self.native.take()
                    && stalled.has_in_flight_work()
                {
                    std::mem::forget(stalled);
                }
                Err(error)
            }
        }
    }
}
