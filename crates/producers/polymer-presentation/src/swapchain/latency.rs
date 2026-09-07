use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicU32, Ordering};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, SetEvent};

pub struct FrameLatency {
    event: OwnedHandle,
    maximum: AtomicU32,
}
impl FrameLatency {
    pub fn new() -> windows::core::Result<Self> {
        let event = unsafe { CreateEventW(None, true, false, None)? };
        Ok(Self {
            event: unsafe { OwnedHandle::from_raw_handle(event.0) },
            maximum: AtomicU32::new(1),
        })
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
