use super::*;
use polymer_presentation::{
    producer::CaptureOperation,
    scheduling::{DetachedSlotReservation, SlotLease},
};
use windows::Win32::Graphics::Direct3D11::ID3D11Fence;

pub(super) struct PaintLease {
    surface: IDXGISurface1,
    dc: HDC,
    dirty: RECT,
    thread: std::thread::ThreadId,
    context: ID3D11DeviceContext4,
    fence: ID3D11Fence,
    value: u64,
    released: bool,
    #[cfg(test)]
    pub(super) release_failures: usize,
}
impl PaintLease {
    fn release(&mut self) -> windows::core::Result<()> {
        #[cfg(test)]
        if self.release_failures > 0 {
            self.release_failures -= 1;
            return Err(windows::core::Error::from_hresult(HRESULT(
                0x80004005u32 as i32,
            )));
        }
        if !self.released {
            unsafe { GdiFlush() };
            unsafe { self.surface.ReleaseDC(Some(&raw const self.dirty)) }?;
            self.released = true;
        }
        Ok(())
    }
}
impl SlotLease for PaintLease {
    type Error = windows::core::Error;
    fn finish(mut self) -> Result<(), Self::Error> {
        self.release()?;
        unsafe { self.context.Signal(&self.fence, self.value) }
    }
}
impl Drop for PaintLease {
    fn drop(&mut self) {
        // The owning capture cannot be preempted while a DC is outstanding.
        // Failed publication/submission still returns native DC ownership.
        let _ = self.release();
    }
}

pub(super) type PaintReservation = DetachedSlotReservation<PaintLease>;

impl Capture {
    /// Retry an ended paint on its owning GUI thread before configuration or
    /// new allocation can release the surface holding its outstanding DC.
    pub(super) fn recover_paint(&mut self) -> windows::core::Result<bool> {
        let Some(paint) = self.pending_release.as_mut() else {
            return Ok(true);
        };
        if paint.lease().thread != std::thread::current().id() {
            return Ok(false);
        }
        paint.lease_mut().release()?;
        self.submitted_value = paint.lease().value;
        self.pending_signal = Some(self.submitted_value);
        self.pending_release.take();
        Ok(true)
    }
}
pub(super) struct BeginPaint {
    pub dirty: [i32; 4],
}
impl CaptureOperation for BeginPaint {
    type Backend = Capture;
    type Output = isize;
    type Error = windows::core::Error;
    unsafe fn submit(self, capture: &mut Capture) -> Result<Option<isize>, Self::Error> {
        if !capture.recover_paint()? {
            return Ok(None);
        }
        if capture.active_paint.is_some() {
            return Ok(None);
        }
        let fence = capture.ring.fence().expect("GDI producer fence");
        if let Some(value) = capture.pending_signal {
            unsafe { capture.context.Signal(fence, value) }?;
            capture.pending_signal = None;
        }
        let producer_ready = unsafe { fence.GetCompletedValue() } >= capture.submitted_value;
        let consumer_ready = capture.sync_value == 0
            || capture
                .ring
                .consumer_fence()
                .is_some_and(|fence| unsafe { fence.GetCompletedValue() } >= capture.sync_value);
        if !producer_ready || !consumer_ready {
            return Ok(None);
        }
        let surface: IDXGISurface1 = capture.ring.slots()[0].texture.cast()?;
        let dc = unsafe { surface.GetDC(!capture.initialized) }?;
        let [left, top, right, bottom] = self.dirty;
        let lease = PaintLease {
            surface,
            dc,
            dirty: RECT {
                left,
                top,
                right,
                bottom,
            },
            thread: std::thread::current().id(),
            context: capture.context.clone(),
            fence: fence.clone(),
            value: capture.submitted_value.wrapping_add(1).max(1),
            released: false,
            #[cfg(test)]
            release_failures: 0,
        };
        if !capture.initialized {
            unsafe {
                PatBlt(
                    dc,
                    0,
                    0,
                    capture.width as i32,
                    capture.height as i32,
                    BLACKNESS,
                )
            };
            capture.initialized = true;
        }
        if right > left && bottom > top {
            unsafe { IntersectClipRect(dc, left, top, right, bottom) };
        }
        let mut lease = Some(lease);
        capture.active_paint = capture
            .scheduler
            .try_acquire(1, |_| lease.take())
            .map(|reservation| reservation.detach());
        Ok(Some(dc.0 as isize))
    }
}

pub(super) struct EndPaint {
    pub hwnd: isize,
    pub dc: isize,
}
impl CaptureOperation for EndPaint {
    type Backend = Capture;
    type Output = ();
    type Error = windows::core::Error;
    unsafe fn submit(self, capture: &mut Capture) -> Result<Option<()>, Self::Error> {
        let Some(paint) = capture.active_paint.as_ref() else {
            return Ok(None);
        };
        if capture.hwnd != self.hwnd
            || paint.lease().dc.0 as isize != self.dc
            || paint.lease().thread != std::thread::current().id()
        {
            return Ok(None);
        }
        let mut paint = capture.active_paint.take().expect("matched active paint");
        if let Err(error) = paint.lease_mut().release() {
            capture.pending_release = Some(paint);
            return Err(error);
        }
        let value = paint.lease().value;
        capture.submitted_value = value;
        capture.pending_signal = Some(value);
        paint.commit(&mut capture.scheduler)?;
        capture.pending_signal = None;
        capture.sync_value = value;
        capture.last_present = Instant::now();
        let extent = polymer_presentation::Extent2D {
            width: capture.width,
            height: capture.height,
        };
        let message = polymer_presentation::producer::CapturedFrame {
            stream_id: StreamId(self.hwnd as u64),
            slot: 0,
            sync_value: value,
            timestamp_qpc: performance_counter().unwrap_or(0),
            extent,
        }
        .present(polymer_presentation::producer::FramePlacement {
            extent,
            bounds: extent.rect(),
            transform: PresentationTransform::axis_aligned(0, 0, extent.width, extent.height),
        });
        capture.ipc.publish_frame(message);
        Ok(Some(()))
    }
}
