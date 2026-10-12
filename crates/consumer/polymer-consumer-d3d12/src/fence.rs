use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{HANDLE, WAIT_OBJECT_0},
    Graphics::Direct3D12::ID3D12Fence,
    System::Threading::{CreateEventW, INFINITE, WaitForSingleObject},
};

pub struct FenceWaiter {
    event: OwnedHandle,
}

impl FenceWaiter {
    pub fn new() -> windows::core::Result<Self> {
        unsafe {
            let event = CreateEventW(None, false, false, None)?;
            Ok(Self {
                event: OwnedHandle::from_raw_handle(event.0),
            })
        }
    }

    pub fn wait(
        &self,
        fence: &ID3D12Fence,
        value: u64,
        timeout_ms: u32,
    ) -> windows::core::Result<bool> {
        let event = HANDLE(self.event.as_raw_handle());
        let deadline = (timeout_ms != INFINITE)
            .then(|| Instant::now() + Duration::from_millis(u64::from(timeout_ms)));
        loop {
            unsafe {
                if fence.GetCompletedValue() >= value {
                    return Ok(true);
                }
                let remaining = deadline.map_or(INFINITE, |deadline| {
                    let left = deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis();
                    u32::try_from(left).unwrap_or(INFINITE - 1)
                });
                fence.SetEventOnCompletion(value, event)?;
                if WaitForSingleObject(event, remaining) != WAIT_OBJECT_0 {
                    return Ok(fence.GetCompletedValue() >= value);
                }
            }
        }
    }
}
