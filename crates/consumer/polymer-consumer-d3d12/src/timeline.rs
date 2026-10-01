use super::*;
use windows::Win32::System::Threading::{INFINITE, SetEvent, WaitForMultipleObjects};

/// The producer's ready timeline, opened independently of any image import so
/// a consumer can learn of new frames from the fence alone.
pub struct ProducerTimeline {
    fence: ID3D12Fence,
    completed: HANDLE,
    closed: HANDLE,
}

// SAFETY: the fence is free-threaded and the events are only signalled or
// waited on, which Win32 permits from any thread.
unsafe impl Send for ProducerTimeline {}
unsafe impl Sync for ProducerTimeline {}

impl ProducerTimeline {
    /// Opens the stream's ready fence and takes ownership of its handle. Image
    /// importers never use the ready timeline.
    pub fn open(stream: &StreamReady) -> windows::core::Result<Self> {
        unsafe {
            let factory = CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let adapter = factory.EnumAdapterByLuid::<IDXGIAdapter1>(LUID {
                LowPart: stream.adapter_luid.low_part,
                HighPart: stream.adapter_luid.high_part,
            })?;
            let mut device: Option<ID3D12Device> = None;
            D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device)?;
            let device = device.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?;
            Ok(Self {
                fence: open_fence(&device, stream.timeline_semaphore_handle)?,
                completed: CreateEventW(None, false, false, None)?,
                closed: CreateEventW(None, true, false, None)?,
            })
        }
    }

    /// Blocks until the timeline passes `value` and returns its newest
    /// completed value, or `None` once closed or the device is removed.
    pub fn wait_beyond(&self, value: u64) -> Option<u64> {
        loop {
            let completed = unsafe { self.fence.GetCompletedValue() };
            if completed == u64::MAX {
                return None;
            }
            if completed > value {
                return Some(completed);
            }
            unsafe { self.fence.SetEventOnCompletion(value + 1, self.completed) }.ok()?;
            let signalled =
                unsafe { WaitForMultipleObjects(&[self.completed, self.closed], false, INFINITE) };
            if signalled != WAIT_OBJECT_0 {
                return None;
            }
        }
    }

    pub fn close(&self) {
        let _ = unsafe { SetEvent(self.closed) };
    }
}

impl Drop for ProducerTimeline {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.completed);
            let _ = CloseHandle(self.closed);
        }
    }
}
