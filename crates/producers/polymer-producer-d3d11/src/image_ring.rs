use polymer_protocol::transport_unorm_dxgi_format;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use windows::{
    Win32::{
        Foundation::GENERIC_ALL,
        Graphics::{
            Direct3D11::{
                D3D11_FENCE_FLAG_SHARED, D3D11_RESOURCE_MISC_SHARED,
                D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC, ID3D11Device,
                ID3D11Device5, ID3D11Fence, ID3D11Texture2D,
            },
            Dxgi::IDXGIResource1,
        },
    },
    core::{Error, HRESULT, Interface, PCWSTR},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct D3d11ImageRingDesc {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub slots: u32,
}

pub struct D3d11ImageSlot {
    pub texture: ID3D11Texture2D,
    pub handle: OwnedHandle,
}

impl D3d11ImageSlot {
    pub fn raw_handle(&self) -> RawHandle {
        self.handle.as_raw_handle()
    }
}

pub struct D3d11ImageRing {
    desc: D3d11ImageRingDesc,
    slots: Vec<D3d11ImageSlot>,
    fence: ID3D11Fence,
    fence_handle: OwnedHandle,
    consumer_fence: ID3D11Fence,
    consumer_fence_handle: OwnedHandle,
}

impl D3d11ImageRing {
    #[allow(clippy::missing_safety_doc)]
    pub unsafe fn new(
        device: &ID3D11Device,
        source: &D3D11_TEXTURE2D_DESC,
        slot_count: u32,
    ) -> windows::core::Result<Self> {
        if slot_count == 0 {
            return Err(Error::from_hresult(HRESULT(0x8000_4003u32 as i32)));
        }
        let transport_format = transport_unorm_dxgi_format(source.Format.0 as u32);
        let desc = D3d11ImageRingDesc {
            width: source.Width,
            height: source.Height,
            format: transport_format,
            slots: slot_count,
        };
        let mut shared_desc = *source;
        shared_desc.Format =
            windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT(transport_format as i32);
        shared_desc.MipLevels = 1;
        shared_desc.ArraySize = 1;
        shared_desc.SampleDesc.Count = 1;
        shared_desc.SampleDesc.Quality = 0;
        shared_desc.CPUAccessFlags = 0;
        shared_desc.MiscFlags =
            (D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED).0 as u32;

        let mut slots = Vec::with_capacity(slot_count as usize);
        for _ in 0..slot_count {
            let mut texture = None;
            unsafe { device.CreateTexture2D(&shared_desc, None, Some(&mut texture)) }
                .map_err(|error| contextual(error, "ID3D11Device::CreateTexture2D"))?;
            let Some(texture) = texture else {
                return Err(Error::from_hresult(HRESULT(0x8000_4003u32 as i32)));
            };
            let resource: IDXGIResource1 = texture
                .cast()
                .map_err(|error| contextual(error, "IDXGIResource1 query"))?;
            let handle =
                unsafe { resource.CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null()) }
                    .map_err(|error| contextual(error, "IDXGIResource1::CreateSharedHandle"))?;
            slots.push(D3d11ImageSlot {
                texture,
                handle: unsafe { OwnedHandle::from_raw_handle(handle.0) },
            });
        }

        let device5: ID3D11Device5 = device
            .cast()
            .map_err(|error| contextual(error, "ID3D11Device5 query"))?;
        let (fence, fence_handle) = create_shared_fence(&device5)?;
        let (consumer_fence, consumer_fence_handle) = create_shared_fence(&device5)?;

        Ok(Self {
            desc,
            slots,
            fence,
            fence_handle,
            consumer_fence,
            consumer_fence_handle,
        })
    }

    pub fn desc(&self) -> D3d11ImageRingDesc {
        self.desc
    }

    pub fn slots(&self) -> &[D3d11ImageSlot] {
        &self.slots
    }

    pub fn fence(&self) -> &ID3D11Fence {
        &self.fence
    }

    pub fn fence_handle(&self) -> &OwnedHandle {
        &self.fence_handle
    }

    pub fn raw_fence_handle(&self) -> RawHandle {
        self.fence_handle.as_raw_handle()
    }

    pub fn consumer_fence(&self) -> &ID3D11Fence {
        &self.consumer_fence
    }

    pub fn raw_consumer_fence_handle(&self) -> RawHandle {
        self.consumer_fence_handle.as_raw_handle()
    }
}

fn create_shared_fence(
    device5: &ID3D11Device5,
) -> windows::core::Result<(ID3D11Fence, OwnedHandle)> {
    let mut fence: Option<ID3D11Fence> = None;
    unsafe { device5.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut fence) }
        .map_err(|error| contextual(error, "ID3D11Device5::CreateFence"))?;
    let Some(fence) = fence else {
        return Err(Error::from_hresult(HRESULT(0x8000_4003u32 as i32)));
    };
    let fence_handle = unsafe { fence.CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null()) }
        .map_err(|error| contextual(error, "ID3D11Fence::CreateSharedHandle"))?;
    Ok((fence, unsafe {
        OwnedHandle::from_raw_handle(fence_handle.0)
    }))
}

fn contextual(error: Error, operation: &str) -> Error {
    Error::new(error.code(), format!("{operation}: {error}"))
}
