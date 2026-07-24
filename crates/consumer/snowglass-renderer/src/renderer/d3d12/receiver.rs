//! Import side of the D3D12 renderer.

use snowglass_consumer_d3d12::{D3d12ConsumerDevice, ImportedD3d12Stream};
use snowglass_ipc::StreamReady;
use snowglass_protocol::AdapterLuid;
use std::ops::Deref;

/// Owns the D3D12 consumer device used to import producer stream handles.
///
/// Swapchain compositors consume the imported resources returned by this
/// type; handle validation and opening stays on the receiving side.
pub struct D3d12Receiver {
    consumer: D3d12ConsumerDevice,
}

impl D3d12Receiver {
    pub fn new_default() -> windows::core::Result<Self> {
        D3d12ConsumerDevice::new_default().map(Self::from_consumer)
    }

    pub fn new(adapter: AdapterLuid) -> windows::core::Result<Self> {
        D3d12ConsumerDevice::new(adapter).map(Self::from_consumer)
    }

    pub fn from_consumer(consumer: D3d12ConsumerDevice) -> Self {
        Self { consumer }
    }

    pub fn import_stream(
        &self,
        stream: &StreamReady,
    ) -> windows::core::Result<ImportedD3d12Stream> {
        self.consumer.import(stream)
    }

    pub fn adapter_luid(&self) -> AdapterLuid {
        self.consumer.adapter_luid()
    }
}

impl Deref for D3d12Receiver {
    type Target = D3d12ConsumerDevice;

    fn deref(&self) -> &Self::Target {
        &self.consumer
    }
}
