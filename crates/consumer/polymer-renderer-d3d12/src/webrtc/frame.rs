use windows::Win32::Graphics::{
    Direct3D12::{D3D12_RESOURCE_STATES, ID3D12Device, ID3D12Fence, ID3D12Resource},
    Dxgi::Common::DXGI_FORMAT,
};
pub(crate) struct D3d12Frame {
    pub(crate) device: ID3D12Device,
    pub(crate) resource: ID3D12Resource,
    pub(crate) state: D3D12_RESOURCE_STATES,
    pub(crate) format: DXGI_FORMAT,
    pub(crate) ready_fence: ID3D12Fence,
    pub(crate) ready_value: u64,
    pub(crate) completion_fence: ID3D12Fence,
    pub(crate) completion_value: u64,
    pub(crate) completion_submitted: bool,
}

impl D3d12Frame {
    pub(crate) fn mark_completion_submitted(&mut self) {
        self.completion_submitted = true;
    }
}

impl Drop for D3d12Frame {
    fn drop(&mut self) {
        if !self.completion_submitted {
            // No GPU consumer took ownership (for example the bounded channel
            // was full), so release the compositor cache immediately.
            let _ = unsafe { self.completion_fence.Signal(self.completion_value) };
        }
    }
}
