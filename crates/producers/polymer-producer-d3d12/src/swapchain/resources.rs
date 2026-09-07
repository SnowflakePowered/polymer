use polymer_presentation::Extent2D;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows::{
    Win32::{
        Foundation::E_POINTER,
        Graphics::{
            Direct3D12::*,
            Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC},
        },
    },
    core::Error,
};

pub(super) struct SharedFence {
    pub fence: ID3D12Fence,
    handle: OwnedHandle,
}
impl SharedFence {
    pub fn new(device: &ID3D12Device) -> Result<Self, Error> {
        let fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_SHARED)? };
        let handle = unsafe { device.CreateSharedHandle(&fence, None, 0x1000_0000, None)? };
        Ok(Self {
            fence,
            handle: unsafe { OwnedHandle::from_raw_handle(handle.0) },
        })
    }
    pub fn handle(&self) -> i64 {
        self.handle.as_raw_handle() as i64
    }
}

pub(super) struct SharedImage {
    pub resource: ID3D12Resource,
    handle: OwnedHandle,
}
impl SharedImage {
    pub fn new(
        device: &ID3D12Device,
        extent: Extent2D,
        format: DXGI_FORMAT,
    ) -> Result<Self, Error> {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: u64::from(extent.width),
            Height: extent.height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            ..Default::default()
        };
        let mut resource: Option<ID3D12Resource> = None;
        unsafe {
            device.CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_SHARED,
                &desc,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut resource,
            )?;
        }
        let resource = resource.ok_or_else(|| Error::from_hresult(E_POINTER))?;
        let handle = unsafe { device.CreateSharedHandle(&resource, None, 0x1000_0000, None)? };
        let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        Ok(Self { resource, handle })
    }
    pub fn handle(&self) -> i64 {
        self.handle.as_raw_handle() as i64
    }
}
