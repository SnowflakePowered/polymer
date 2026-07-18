use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use windows::{
    Win32::{
        Foundation::GENERIC_ALL,
        Graphics::{
            Direct3D11::{
                D3D11_FENCE_FLAG_SHARED, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
                D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC, ID3D11Device,
                ID3D11Device5, ID3D11Fence, ID3D11Texture2D,
            },
            Dxgi::{IDXGIKeyedMutex, IDXGIResource1},
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
    pub keyed_mutex: IDXGIKeyedMutex,
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
}

impl D3d11ImageRing {
    pub unsafe fn new(
        device: &ID3D11Device,
        source: &D3D11_TEXTURE2D_DESC,
        slot_count: u32,
    ) -> windows::core::Result<Self> {
        if slot_count == 0 {
            return Err(Error::from_hresult(HRESULT(0x8000_4003u32 as i32)));
        }
        let desc = D3d11ImageRingDesc {
            width: source.Width,
            height: source.Height,
            format: source.Format.0 as u32,
            slots: slot_count,
        };
        let mut shared_desc = *source;
        shared_desc.MipLevels = 1;
        shared_desc.ArraySize = 1;
        shared_desc.SampleDesc.Count = 1;
        shared_desc.SampleDesc.Quality = 0;
        shared_desc.CPUAccessFlags = 0;
        shared_desc.MiscFlags =
            (D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX).0 as u32;

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
            let keyed_mutex: IDXGIKeyedMutex = texture
                .cast()
                .map_err(|error| contextual(error, "IDXGIKeyedMutex query"))?;
            slots.push(D3d11ImageSlot {
                texture,
                keyed_mutex,
                handle: unsafe { OwnedHandle::from_raw_handle(handle.0) },
            });
        }

        let device5: ID3D11Device5 = device
            .cast()
            .map_err(|error| contextual(error, "ID3D11Device5 query"))?;
        let mut fence: Option<ID3D11Fence> = None;
        unsafe { device5.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut fence) }
            .map_err(|error| contextual(error, "ID3D11Device5::CreateFence"))?;
        let Some(fence) = fence else {
            return Err(Error::from_hresult(HRESULT(0x8000_4003u32 as i32)));
        };
        let fence_handle = unsafe { fence.CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null()) }
            .map_err(|error| contextual(error, "ID3D11Fence::CreateSharedHandle"))?;

        Ok(Self {
            desc,
            slots,
            fence,
            fence_handle: unsafe { OwnedHandle::from_raw_handle(fence_handle.0) },
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
}

fn contextual(error: Error, operation: &str) -> Error {
    Error::new(error.code(), format!("{operation}: {error}"))
}
