use super::*;

impl D3d12Compositor {
    /// Returns whether this compositor can begin another frame without
    /// waiting for DXGI presentation capacity. All receiver swapchains use a
    /// one-frame latency waitable object so the display clock owns pacing.
    pub fn try_begin_frame(&mut self) -> bool {
        self.begin_frame_within(Duration::ZERO)
    }

    /// Blocks until DXGI has presentation capacity, so the caller can gather
    /// the frame it is about to composite *after* the wait instead of before
    /// it.
    ///
    /// Blocking inside `Present` at the end of a frame costs a whole refresh
    /// interval of staleness: everything the frame was built from was sampled
    /// before the wait rather than after it. Waiting here first is what makes
    /// the composited image one vblank old instead of two.
    pub fn begin_frame(&mut self) {
        // The timeout is a liveness guard, not pacing: an occluded or otherwise
        // stalled swapchain must not wedge the caller's event loop. Falling
        // through on a timeout only means `Present` blocks instead.
        self.begin_frame_within(Duration::from_millis(u64::from(FRAME_PERMIT_TIMEOUT_MS)));
    }

    /// Waits up to `budget` for presentation capacity, reporting whether it
    /// arrived.
    ///
    /// A caller already paced by something else — a host waking on the
    /// DirectComposition clock, say — wants a budget rather than either
    /// extreme. DXGI releases the slot for the frame the compositor is retiring
    /// slightly *after* that clock ticks, so polling misses it by a hair and
    /// defers the present a whole interval, while waiting without a bound would
    /// hand the display's pacing control over an event loop that has other work
    /// to do.
    ///
    /// A permit already held is kept rather than waited for again. The waitable
    /// object is a semaphore released once per completed present, so taking a
    /// permit without presenting would leave the next wait with nothing to
    /// consume.
    pub fn begin_frame_within(&mut self, budget: Duration) -> bool {
        if self.frame_permit {
            return true;
        }
        let Some(handle) = self.frame_latency_waitable else {
            return true;
        };
        // Round up, so a sub-millisecond budget still waits instead of quietly
        // degrading into a poll. `WaitForSingleObject` reserves `INFINITE`.
        let milliseconds = u32::try_from(budget.as_nanos().div_ceil(1_000_000))
            .unwrap_or(INFINITE - 1)
            .min(INFINITE - 1);
        // SAFETY: the handle belongs to this live swapchain and is only closed
        // when the compositor is dropped.
        self.frame_permit = unsafe { WaitForSingleObject(handle, milliseconds) } == WAIT_OBJECT_0;
        self.frame_permit
    }

    /// Emits measured guest-capture-to-vblank latency when DXGI can attribute
    /// a present to an exact display refresh.
    pub(super) fn poll_display_latency(&mut self) {
        // SAFETY: the swapchain is live for the compositor's lifetime.
        //
        // Statistics are unavailable on some configurations and go disjoint
        // across mode changes, both reported as an error. Neither is worth
        // surfacing: this is diagnostics, and the next frame usually recovers.
        let mut statistics = DXGI_FRAME_STATISTICS::default();
        if unsafe { self.swapchain.GetFrameStatistics(&mut statistics) }.is_ok() {
            // `SyncQPCTime` timestamps the vblank named by `SyncRefreshCount`.
            // It only dates the present when that present went out on the very
            // same vblank; otherwise the present's own vblank was never
            // timestamped, and inferring one would require a refresh period
            // this does not have. Skipping those keeps every sample measured
            // rather than modelled.
            if statistics.PresentRefreshCount == statistics.SyncRefreshCount {
                self.display_latency.observe(
                    statistics.PresentCount,
                    statistics.SyncQPCTime as u64,
                    self.counter_frequency,
                );
            }
        }
    }

    pub(crate) fn resize(&mut self, window: &impl RenderWindow) -> Result<bool, Box<dyn Error>> {
        let size = window.render_extent();
        if size.width == 0 || size.height == 0 {
            return Ok(false);
        }
        self.ensure_device()?;
        self.wait_idle()?;
        self.backbuffers.clear();
        unsafe {
            self.swapchain.ResizeBuffers(
                FRAME_COUNT as u32,
                size.width,
                size.height,
                SWAPCHAIN_FORMAT,
                self.swapchain_flags,
            )?;
            self.backbuffers = create_backbuffers(
                self.receiver.device(),
                &self.swapchain,
                &self.rtv_heap,
                self.rtv_stride,
            )?;
        }
        self.frame_permit = false;
        self.extent = (size.width, size.height);
        Ok(true)
    }

    /// Fails once the device is removed, logging why on the first failure.
    pub(super) fn ensure_device(&mut self) -> Result<(), Box<dyn Error>> {
        if unsafe { self.receiver.device().GetDeviceRemovedReason() }.is_ok() {
            return Ok(());
        }
        if !self.removal_reported {
            self.removal_reported = true;
            tracing::error!(
                "polymer: D3D12 compositor device removed: {}",
                self.receiver.removal_report()
            );
        }
        Err("D3D12 device removed".into())
    }

    pub(super) fn wait_frame(&self, index: usize) -> windows::core::Result<()> {
        self.wait_value(self.frame_values[index])
    }

    pub(super) fn wait_idle(&mut self) -> windows::core::Result<()> {
        unsafe {
            self.fence_value += 1;
            self.receiver
                .queue()
                .Signal(&self.fence, self.fence_value)?;
        }
        self.wait_value(self.fence_value)
    }

    pub(super) fn wait_value(&self, value: u64) -> windows::core::Result<()> {
        unsafe {
            if value != 0 && self.fence.GetCompletedValue() < value {
                self.fence.SetEventOnCompletion(value, self.fence_event)?;
                WaitForSingleObject(self.fence_event, INFINITE);
            }
        }
        Ok(())
    }
}

impl DisplayLatency {
    /// Bounds how many unconfirmed presents are tracked. Frame statistics can
    /// go unavailable for stretches, and this is diagnostics: dropping the
    /// oldest is right, growing without limit is not.
    pub(super) const PENDING_LIMIT: usize = 64;

    pub(super) fn record(&mut self, present_count: u32, captured_qpc: u64) {
        if self.pending.len() >= Self::PENDING_LIMIT {
            self.pending.pop_front();
        }
        self.pending.push_back(PresentedFrame {
            present_count,
            captured_qpc,
        });
    }

    /// Attributes a vblank to the present it displayed.
    ///
    /// `displayed_qpc` is only meaningful for the present DXGI names, so
    /// everything older is discarded rather than guessed at: an earlier present
    /// went out on an earlier vblank whose time was never sampled.
    pub(super) fn observe(&mut self, present_count: u32, displayed_qpc: u64, frequency: f64) {
        while self
            .pending
            .front()
            .is_some_and(|frame| frame.present_count < present_count)
        {
            self.pending.pop_front();
        }
        let Some(frame) = self
            .pending
            .front()
            .filter(|frame| frame.present_count == present_count)
            .copied()
        else {
            return;
        };
        self.pending.pop_front();
        let Some(ticks) = displayed_qpc.checked_sub(frame.captured_qpc) else {
            return;
        };
        polymer_statistics::display_latency(
            "d3d12",
            Duration::from_secs_f64(ticks as f64 / frequency),
        );
    }
}

/// QPC ticks per second, the clock producers stamp captures on and DXGI
/// reports vblanks on.
pub(super) fn counter_frequency() -> f64 {
    let mut frequency = 0i64;
    // SAFETY: the out-parameter is a live local. A failure or a zero would
    // divide, so the result is floored either way.
    let queried = unsafe {
        windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut frequency)
    };
    if queried == 0 {
        return 1.0;
    }
    (frequency as f64).max(1.0)
}
