use polymer_protocol::FrameReady;
use std::time::Instant;

/// Whether a composited image stays in use after its frame is displayed.
#[derive(Clone, Copy)]
pub enum Retention {
    /// The displayed image is redrawn from the producer's slot, so that slot
    /// is held until a newer frame replaces it.
    Retained,
    /// The image was copied into a consumer-owned cache, so its slot can be
    /// released as soon as the copy is submitted.
    Cached,
}

#[derive(Clone, Copy)]
struct ReceivedFrame {
    ready: FrameReady,
    /// When the frame arrived, which is when its ready timeline completed.
    received: Instant,
}

/// The newest frame awaiting composite and the frame last composited. Frames
/// only arrive once complete, so a pending frame is always drawable; it stays
/// pending until a composite carrying it is submitted.
#[derive(Default)]
pub struct SurfaceFrames {
    pending: Option<ReceivedFrame>,
    displayed: Option<ReceivedFrame>,
    received: u64,
    released: u64,
}

impl SurfaceFrames {
    /// Superseded frames are reclaimed by the next submitted release watermark.
    pub fn offer(&mut self, ready: FrameReady) {
        if ready.sync_value <= self.received {
            return;
        }
        self.received = ready.sync_value;
        self.pending = Some(ReceivedFrame {
            ready,
            received: Instant::now(),
        });
    }

    /// A newer frame the next composite will carry.
    pub fn pending(&self) -> Option<FrameReady> {
        self.pending.map(|frame| frame.ready)
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn displayed(&self) -> Option<FrameReady> {
        self.displayed.map(|frame| frame.ready)
    }

    /// The image the next composite draws: the pending frame, else the last
    /// displayed one.
    pub fn frame(&self) -> Option<FrameReady> {
        self.pending().or_else(|| self.displayed())
    }

    /// The release watermark a composite carrying the pending frame submits.
    pub fn release_value(&self, retention: Retention) -> Option<u64> {
        let frame = self.pending?;
        // The timeline acknowledges all older frames. Retaining this image
        // must not release it, even when intermediate frames were discarded.
        let value = frame.ready.sync_value - u64::from(matches!(retention, Retention::Retained));
        (value > self.released).then_some(value)
    }

    /// Records a submitted composite. Returns when a newly displayed frame
    /// arrived, for ready → composite latency.
    pub fn commit(&mut self, retention: Retention) -> Option<Instant> {
        if let Some(value) = self.release_value(retention) {
            self.released = value;
        }
        let frame = self.pending.take()?;
        self.displayed = Some(frame);
        Some(frame.received)
    }
}
