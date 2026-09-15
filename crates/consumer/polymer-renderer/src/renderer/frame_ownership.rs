use polymer_protocol::FrameReady;

pub(super) struct Pending;
pub(super) struct Prepared {
    retention: Retention,
}
pub(super) struct Displayed;

#[derive(Clone, Copy)]
pub(super) enum Retention {
    Retained,
    Cached,
}

/// A non-copyable lease. FrameReady is only the wire metadata; copying that
/// metadata does not duplicate a lease or permit an ownership transition.
/// Only pending leases can be prepared, and only prepared leases can submit.
pub(super) struct FrameOwnership<State> {
    ready: FrameReady,
    state: State,
}

impl<State> FrameOwnership<State> {
    pub fn ready(&self) -> FrameReady {
        self.ready
    }
}

impl FrameOwnership<Pending> {
    fn prepare(self, retention: Retention) -> FrameOwnership<Prepared> {
        FrameOwnership {
            ready: self.ready,
            state: Prepared { retention },
        }
    }
}

impl FrameOwnership<Prepared> {
    fn release_value(&self) -> u64 {
        // The timeline acknowledges all older frames. Retaining this image
        // must not release it, even when intermediate frames were discarded.
        self.ready.sync_value - u64::from(matches!(self.state.retention, Retention::Retained))
    }

    fn submit(self) -> FrameOwnership<Displayed> {
        FrameOwnership {
            ready: self.ready,
            state: Displayed,
        }
    }

    fn abort(self) -> FrameOwnership<Pending> {
        FrameOwnership {
            ready: self.ready,
            state: Pending,
        }
    }
}

/// The replacement has exactly one owner, including during command recording.
/// It cannot simultaneously exist as both a pending and a prepared lease.
enum Replacement {
    Pending(FrameOwnership<Pending>),
    Prepared(FrameOwnership<Prepared>),
}

impl Replacement {
    fn abort(self) -> FrameOwnership<Pending> {
        match self {
            Self::Pending(frame) => frame,
            Self::Prepared(frame) => frame.abort(),
        }
    }

    fn ready(&self) -> FrameReady {
        match self {
            Self::Pending(frame) => frame.ready(),
            Self::Prepared(frame) => frame.ready(),
        }
    }
}

/// Coordinates the independently displayed lease and its replacement.
/// Backends prepare after checking producer readiness and commit after submit.
#[derive(Default)]
pub(super) struct SurfaceFrames {
    pub release_sender: Option<polymer_runtime::BufferReleaseSender>,
    replacement: Option<Replacement>,
    displayed: Option<FrameOwnership<Displayed>>,
    received: u64,
    released: u64,
}

impl SurfaceFrames {
    pub fn acknowledge_release(&self, value: u64) -> std::io::Result<()> {
        if let (Some(sender), Some(frame)) = (&self.release_sender, self.frame()) {
            sender.release(frame.stream_id, value)?;
        }
        Ok(())
    }
    /// Superseded frames are reclaimed by the next submitted release watermark.
    pub fn offer(&mut self, ready: FrameReady) {
        if ready.sync_value <= self.received {
            return;
        }
        self.received = ready.sync_value;
        self.replacement = Some(Replacement::Pending(FrameOwnership {
            ready,
            state: Pending,
        }));
    }

    /// Roll back any recording that did not reach GPU submission.
    pub fn begin(&mut self) {
        self.replacement = self
            .replacement
            .take()
            .map(|frame| Replacement::Pending(frame.abort()));
    }

    /// Prepare the owned pending lease, never arbitrary copied wire metadata.
    /// Retention is fixed here so release calculation and commit cannot disagree.
    pub fn prepare_pending(&mut self, retention: Retention) {
        self.replacement = self
            .replacement
            .take()
            .map(|replacement| match replacement {
                Replacement::Pending(frame) => Replacement::Prepared(frame.prepare(retention)),
                prepared => prepared,
            });
    }

    pub fn pending(&self) -> Option<FrameReady> {
        match &self.replacement {
            Some(Replacement::Pending(frame)) => Some(frame.ready()),
            _ => None,
        }
    }

    /// Prepared work still needs a retry if recording or swapchain acquisition fails.
    pub fn has_pending(&self) -> bool {
        self.replacement.is_some()
    }

    pub fn displayed(&self) -> Option<FrameReady> {
        self.displayed.as_ref().map(FrameOwnership::ready)
    }

    pub fn prepared(&self) -> Option<&FrameOwnership<Prepared>> {
        match &self.replacement {
            Some(Replacement::Prepared(frame)) => Some(frame),
            _ => None,
        }
    }

    /// Metadata for UI layout and retirement, including an unsubmitted update.
    pub fn latest(&self) -> Option<FrameReady> {
        self.replacement
            .as_ref()
            .map(Replacement::ready)
            .or_else(|| self.displayed())
    }

    /// Metadata for the image used by the current draw, or the retained redraw.
    pub fn frame(&self) -> Option<FrameReady> {
        self.prepared()
            .map(FrameOwnership::ready)
            .or_else(|| self.displayed())
    }

    pub fn release_value(&self) -> Option<u64> {
        let value = self.prepared()?.release_value();
        (value > self.released).then_some(value)
    }

    pub fn commit(&mut self) {
        if let Some(value) = self.release_value() {
            self.released = value;
        }
        match self.replacement.take() {
            Some(Replacement::Prepared(frame)) => self.displayed = Some(frame.submit()),
            pending => self.replacement = pending,
        }
    }
}
