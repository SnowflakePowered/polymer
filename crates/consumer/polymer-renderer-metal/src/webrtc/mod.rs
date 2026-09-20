//! VideoToolbox hardware encoding fed from the existing Metal presentation path.
pub mod encoder;
pub mod gpu;
use crate::MetalCompositor;
use objc2_metal::MTLCommandBuffer;
use parking_lot::Mutex;
use polymer_renderer::webrtc::{EncodedFrame, Publisher, VideoFrame};
use polymer_renderer::{CompositingPlan, ReceivedSurface, RenderWindow, Renderer};
use polymer_runtime::SurfaceOffer;
use std::{
    collections::HashMap,
    error::Error,
    ops::{Deref, DerefMut},
    sync::Arc,
};

/// The same native swapchain with an optional encoded-frame publication sink.
/// GPU copies detach encoder input from the producer lease before release.
pub struct WebRtcCompositor {
    native: MetalCompositor,
    input: Option<gpu::GpuInput>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
    publisher: Option<Arc<Publisher>>,
    encoders: HashMap<u64, Arc<Mutex<Option<encoder::Encoder>>>>,
    published: HashMap<u64, u64>,
}
impl WebRtcCompositor {
    pub fn new(native: MetalCompositor) -> Self {
        Self {
            native,
            input: None,
            wake: None,
            publisher: None,
            encoders: HashMap::new(),
            published: HashMap::new(),
        }
    }
    pub fn set_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.native.set_wake(wake.clone());
        if let Some(input) = &self.input {
            input.set_wake(wake.clone());
        }
        self.wake = Some(wake);
    }
    pub fn set_publisher(&mut self, publisher: Option<Arc<Publisher>>) {
        self.publisher = publisher;
        self.published.clear();
        self.encoders.clear();
    }
}
impl Deref for WebRtcCompositor {
    type Target = MetalCompositor;
    fn deref(&self) -> &MetalCompositor {
        &self.native
    }
}
impl DerefMut for WebRtcCompositor {
    fn deref_mut(&mut self) -> &mut MetalCompositor {
        &mut self.native
    }
}
impl Renderer for WebRtcCompositor {
    fn import_surface_offer(
        &mut self,
        offer: &SurfaceOffer,
        turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        self.native.import_surface_offer(offer, turns)
    }
    fn disconnect_stream(&mut self, connection: u64) -> Result<(), Box<dyn Error>> {
        self.encoders.remove(&connection);
        self.published.remove(&connection);
        self.native.disconnect_stream(connection)
    }
    fn set_latest(&mut self, connection: u64, frame: polymer_protocol::FrameReady) {
        self.native.set_latest(connection, frame);
    }
    fn received_surfaces(&self) -> Vec<ReceivedSurface> {
        self.native.received_surfaces()
    }
    fn has_pending_frame(&self) -> bool {
        self.native.has_pending_frame()
            || (self.publisher.is_some()
                && self.input.as_ref().is_none_or(|input| input.available())
                && self.native.received_surfaces().iter().any(|surface| {
                    self.published.get(&surface.connection) != Some(&surface.frame.frame_number)
                }))
    }
    fn recreate_swapchain(&mut self, window: &impl RenderWindow) -> Result<bool, Box<dyn Error>> {
        self.native.recreate_swapchain(window)
    }
    fn present(&mut self, plan: &CompositingPlan) -> Result<bool, Box<dyn Error>> {
        let Some(publisher) = self.publisher.clone() else {
            return self.native.present(plan);
        };
        if self.input.is_none() {
            use objc2::Message;
            let input = gpu::GpuInput::new(self.native.device().retain())?;
            if let Some(wake) = &self.wake {
                input.set_wake(wake.clone());
            }
            self.input = Some(input);
        }
        let input = self.input.as_ref().unwrap();
        let published = &self.published;
        let mut scheduled = Vec::new();
        let encoders = &mut self.encoders;
        let presented = self.native.present_with_capture(
            plan,
            |_| Ok(()),
            |commands, frames| {
                for source in frames {
                    if !plan.placements().any(|p| p.connection == source.connection)
                        || published.get(&source.connection) == Some(&source.frame.frame_number)
                    {
                        continue;
                    }
                    let Some(frame) =
                        input.capture(commands, source.texture, source.quarter_turns)?
                    else {
                        continue;
                    };
                    let session = encoders.entry(source.connection).or_default().clone();
                    let (width, height) = frame.extent();
                    let job = VideoFrame::new(
                        source.connection,
                        width,
                        height,
                        move |width, height, codec, bitrate, rate| {
                            let mut session = session.lock();
                            if session
                                .as_ref()
                                .is_none_or(|s| !s.matches(width, height, codec))
                            {
                                *session = Some(encoder::Encoder::new(
                                    width, height, codec, bitrate, rate,
                                )?);
                            }
                            let result = session.as_mut().unwrap().encode(frame.buffer(), rate);
                            if result.is_err() {
                                *session = None;
                            }
                            Ok(EncodedFrame {
                                backend: "VideoToolbox hardware",
                                codec,
                                data: result?,
                            })
                        },
                    );
                    let job = Mutex::new(Some(job));
                    let publisher = publisher.clone();
                    let done = block2::RcBlock::new(
                        move |buffer: std::ptr::NonNull<
                            objc2::runtime::ProtocolObject<dyn objc2_metal::MTLCommandBuffer>,
                        >| {
                            let job = job.lock().take();
                            if unsafe { buffer.as_ref() }.status()
                                == objc2_metal::MTLCommandBufferStatus::Completed
                            {
                                if let Some(job) = job {
                                    publisher.publish(job);
                                }
                            }
                        },
                    );
                    unsafe {
                        commands.addCompletedHandler(block2::RcBlock::as_ptr(&done));
                    }
                    scheduled.push((source.connection, source.frame.frame_number));
                }
                Ok(())
            },
        )?;
        if presented {
            self.published.extend(scheduled);
        }
        Ok(presented)
    }
}
