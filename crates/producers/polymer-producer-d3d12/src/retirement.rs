use super::*;
use std::sync::OnceLock;

pub(super) struct RetiredRing {
    _slots: Vec<D3d12SharedSlot>,
    queue: ID3D12CommandQueue,
    fence: ID3D12Fence,
    value: u64,
    needs_signal: bool,
}
impl RetiredRing {
    fn queue() -> &'static parking_lot::Mutex<Vec<Self>> {
        static RETIRED: OnceLock<parking_lot::Mutex<Vec<RetiredRing>>> = OnceLock::new();
        RETIRED.get_or_init(Default::default)
    }
    pub fn new(capture: &mut D3d12SwapchainCapture) -> Self {
        Self {
            _slots: std::mem::take(&mut capture.slots),
            queue: capture.queue.clone(),
            fence: capture.fence.clone(),
            value: capture.next_fence.saturating_sub(1),
            needs_signal: capture.pending_signal.is_some(),
        }
    }
    pub fn poll() {
        Self::queue()
            .lock()
            .retain_mut(|ring| !ring.ready_to_retire());
    }
    pub fn defer(mut self) {
        if !self.ready_to_retire() {
            Self::queue().lock().push(self);
        }
    }
    pub fn wait(&mut self) -> bool {
        if self.ready_to_retire() {
            return true;
        }
        if self.needs_signal {
            return false;
        }
        unsafe {
            use windows_sys::Win32::System::Threading::{
                CreateEventW, INFINITE, WaitForSingleObject,
            };
            let event = CreateEventW(std::ptr::null(), 0, 0, std::ptr::null());
            if event.is_null() {
                return false;
            }
            if self
                .fence
                .SetEventOnCompletion(self.value, HANDLE(event))
                .is_ok()
            {
                WaitForSingleObject(event, INFINITE);
            }
            let _ = CloseHandle(HANDLE(event));
        }
        self.ready_to_retire()
    }
}
impl RetiredRing {
    fn ready_to_retire(&mut self) -> bool {
        if unsafe { self.fence.GetCompletedValue() } >= self.value {
            return true;
        }
        if self.needs_signal && unsafe { self.queue.Signal(&self.fence, self.value) }.is_ok() {
            self.needs_signal = false;
        }
        (unsafe { self.fence.GetCompletedValue() }) >= self.value
    }
}
