use super::{AcquireDeadline, ControllerError, SwapchainBackend, SwapchainController};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForSingleObject,
};

/// An auto-reset event that backend fences set on release or completion.
pub struct ProgressEvent(OwnedHandle);
impl ProgressEvent {
    pub fn new() -> windows::core::Result<Self> {
        let event = unsafe { CreateEventW(None, false, false, None)? };
        Ok(Self(unsafe { OwnedHandle::from_raw_handle(event.0) }))
    }
    pub fn handle(&self) -> HANDLE {
        HANDLE(self.0.as_raw_handle())
    }
    pub fn wait(&self, timeout: Duration) {
        let milliseconds = u32::try_from(timeout.as_millis()).map_or(INFINITE - 1, |ms| ms.max(1));
        unsafe { WaitForSingleObject(self.handle(), milliseconds) };
    }
}

/// The application-visible frame latency waitable. A release fence sets it
/// directly once the controller's pacing target is released.
pub struct FrameLatency {
    event: OwnedHandle,
    maximum: AtomicU32,
    armed: AtomicU64,
}
impl FrameLatency {
    pub fn new() -> windows::core::Result<Self> {
        let event = unsafe { CreateEventW(None, true, false, None)? };
        Ok(Self {
            event: unsafe { OwnedHandle::from_raw_handle(event.0) },
            maximum: AtomicU32::new(1),
            armed: AtomicU64::new(0),
        })
    }
    pub fn handle(&self) -> HANDLE {
        HANDLE(self.event.as_raw_handle())
    }
    /// Sets the waitable once `controller` may take another frame, and
    /// otherwise arms it on the release that will allow one. Returns `false`
    /// when the controller has failed; the waitable is then set so a waiting
    /// application calls Present and observes the failure.
    pub fn refresh<B: SwapchainBackend>(&self, controller: &mut SwapchainController<B>) -> bool {
        let maximum = self.maximum();
        self.signal(false);
        let ready = match controller.wait_for_pacing(maximum, AcquireDeadline::Nonblocking) {
            Ok(()) => true,
            Err(ControllerError::WouldBlock) => {
                let armed = controller
                    .pacing_release_target(maximum)
                    .or_else(|| controller.pending_release())
                    .filter(|&target| self.armed.swap(target, Ordering::AcqRel) != target)
                    .is_none_or(|target| {
                        controller
                            .backend()
                            .arm_progress(Some(target), None, self.handle())
                            .is_ok()
                    });
                if armed {
                    return true;
                }
                false
            }
            Err(_) => false,
        };
        self.signal(true);
        ready
    }
    pub fn set_maximum(&self, maximum: u32) {
        self.maximum.store(maximum, Ordering::Release);
    }
    pub fn maximum(&self) -> u32 {
        self.maximum.load(Ordering::Acquire)
    }
    pub fn signal(&self, ready: bool) {
        let event = HANDLE(self.event.as_raw_handle());
        unsafe {
            let _ = if ready {
                SetEvent(event)
            } else {
                ResetEvent(event)
            };
        }
    }
    pub fn duplicate(&self) -> HANDLE {
        polymer_platform_windows::duplicate_handle_into_process(
            self.event.as_raw_handle() as isize,
            polymer_platform_windows::ProcessId(std::process::id()),
        )
        .map_or(HANDLE::default(), |handle| HANDLE(handle as *mut _))
    }
}
