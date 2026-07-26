use ash::vk;
use snowglass_protocol::AdapterLuid;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use windows::Win32::{
    Foundation::{GENERIC_ALL, LUID},
    Graphics::{
        Direct3D::D3D_FEATURE_LEVEL_11_0,
        Direct3D12::{
            D3D12_CPU_PAGE_PROPERTY_UNKNOWN, D3D12_FENCE_FLAG_SHARED, D3D12_HEAP_FLAG_SHARED,
            D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT, D3D12_MEMORY_POOL_UNKNOWN,
            D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS, D3D12_RESOURCE_STATE_COMMON,
            D3D12_TEXTURE_LAYOUT_UNKNOWN, D3D12CreateDevice, ID3D12Device, ID3D12Fence,
            ID3D12Resource,
        },
        Dxgi::{
            Common::{
                DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM,
                DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC,
            },
            CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4,
        },
    },
};

pub(crate) struct SharedResource {
    pub resource: ID3D12Resource,
    pub handle: OwnedHandle,
}

pub(crate) struct SharedFence {
    pub fence: ID3D12Fence,
    pub handle: OwnedHandle,
}

pub(crate) fn create_device(luid: AdapterLuid) -> Result<ID3D12Device, vk::Result> {
    unsafe {
        let factory = CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0))
            .map_err(|_| vk::Result::ERROR_INITIALIZATION_FAILED)?;
        let adapter = factory
            .EnumAdapterByLuid::<IDXGIAdapter1>(LUID {
                LowPart: luid.low_part,
                HighPart: luid.high_part,
            })
            .map_err(|_| vk::Result::ERROR_INITIALIZATION_FAILED)?;
        let mut device = None;
        D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device)
            .map_err(|_| vk::Result::ERROR_INITIALIZATION_FAILED)?;
        device.ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)
    }
}

pub(crate) fn create_resource(
    device: &ID3D12Device,
    extent: vk::Extent2D,
    format: vk::Format,
) -> Result<SharedResource, vk::Result> {
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Width: u64::from(extent.width),
        Height: extent.height,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: dxgi_format(format).ok_or(vk::Result::ERROR_FORMAT_NOT_SUPPORTED)?,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
        ..Default::default()
    };
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
        MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
        CreationNodeMask: 1,
        VisibleNodeMask: 1,
    };
    unsafe {
        let mut resource = None;
        device
            .CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_SHARED,
                &desc,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut resource,
            )
            .map_err(|_| vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)?;
        let resource = resource.ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)?;
        let raw = device
            .CreateSharedHandle(&resource, None, GENERIC_ALL.0, None)
            .map_err(|_| vk::Result::ERROR_INVALID_EXTERNAL_HANDLE)?;
        Ok(SharedResource {
            resource,
            handle: OwnedHandle::from_raw_handle(raw.0),
        })
    }
}

pub(crate) fn create_fence(device: &ID3D12Device) -> Result<SharedFence, vk::Result> {
    unsafe {
        let fence = device
            .CreateFence(0, D3D12_FENCE_FLAG_SHARED)
            .map_err(|_| vk::Result::ERROR_INITIALIZATION_FAILED)?;
        let raw = device
            .CreateSharedHandle(&fence, None, GENERIC_ALL.0, None)
            .map_err(|_| vk::Result::ERROR_INVALID_EXTERNAL_HANDLE)?;
        Ok(SharedFence {
            fence,
            handle: OwnedHandle::from_raw_handle(raw.0),
        })
    }
}

pub(crate) fn dxgi_format(format: vk::Format) -> Option<DXGI_FORMAT> {
    Some(match format {
        vk::Format::R8G8B8A8_UNORM => DXGI_FORMAT_R8G8B8A8_UNORM,
        vk::Format::R8G8B8A8_SRGB => DXGI_FORMAT_R8G8B8A8_UNORM,
        vk::Format::B8G8R8A8_UNORM => DXGI_FORMAT_B8G8R8A8_UNORM,
        vk::Format::B8G8R8A8_SRGB => DXGI_FORMAT_B8G8R8A8_UNORM,
        vk::Format::A2B10G10R10_UNORM_PACK32 => DXGI_FORMAT_R10G10B10A2_UNORM,
        vk::Format::R16G16B16A16_SFLOAT => DXGI_FORMAT_R16G16B16A16_FLOAT,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_vulkan_formats_create_unorm_d3d12_resources() {
        assert_eq!(
            dxgi_format(vk::Format::R8G8B8A8_SRGB),
            Some(DXGI_FORMAT_R8G8B8A8_UNORM)
        );
        assert_eq!(
            dxgi_format(vk::Format::B8G8R8A8_SRGB),
            Some(DXGI_FORMAT_B8G8R8A8_UNORM)
        );
    }
}
