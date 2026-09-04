use polymer_protocol::transport_unorm_dxgi_format;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use windows::{
    Win32::{
        Foundation::GENERIC_ALL,
        Graphics::{
            Direct3D11::{
                D3D11_FENCE_FLAG_SHARED, D3D11_RESOURCE_MISC_GDI_COMPATIBLE,
                D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
                D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC, ID3D11Device,
                ID3D11Device5, ID3D11Fence, ID3D11Texture2D,
            },
            Dxgi::{IDXGIKeyedMutex, IDXGIResource1},
        },
    },
    core::{Error, HRESULT, Interface, PCWSTR},
};

/// How the shared ring synchronizes slot handoff with its consumer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum D3d11RingMode {
    /// Keyed-mutex NT-handle sharing with a shared producer fence, for
    /// consumers that acquire through Vulkan's DXGI interop.
    KeyedMutex,
    /// Keyed-mutex-only sharing for D3D12 consumers on devices without
    /// shared-fence support. The consumer acquires through D3D11On12.
    KeyedMutexNoFence,
    /// Fence-only NT-handle sharing that D3D12 opens directly. Slot reuse is
    /// gated on a shared consumer-release fence instead of a keyed mutex.
    Fence,
}

impl D3d11RingMode {
    pub fn uses_keyed_mutex(self) -> bool {
        matches!(self, Self::KeyedMutex | Self::KeyedMutexNoFence)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct D3d11ImageRingDesc {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub slots: u32,
}

pub struct D3d11ImageSlot {
    pub texture: ID3D11Texture2D,
    pub keyed_mutex: Option<IDXGIKeyedMutex>,
    pub handle: OwnedHandle,
}

impl D3d11ImageSlot {
    pub fn raw_handle(&self) -> RawHandle {
        self.handle.as_raw_handle()
    }
}

pub struct D3d11ImageRing {
    desc: D3d11ImageRingDesc,
    mode: D3d11RingMode,
    slots: Vec<D3d11ImageSlot>,
    fence: Option<ID3D11Fence>,
    fence_handle: Option<OwnedHandle>,
    consumer_fence: Option<ID3D11Fence>,
    consumer_fence_handle: Option<OwnedHandle>,
}

impl D3d11ImageRing {
    pub unsafe fn new(
        device: &ID3D11Device,
        source: &D3D11_TEXTURE2D_DESC,
        slot_count: u32,
        mode: D3d11RingMode,
    ) -> windows::core::Result<Self> {
        unsafe { Self::new_inner(device, source, slot_count, mode, false) }
    }

    /// Creates a fence-synchronized ring whose textures can be rendered by GDI.
    pub unsafe fn new_gdi(
        device: &ID3D11Device,
        source: &D3D11_TEXTURE2D_DESC,
        slot_count: u32,
    ) -> windows::core::Result<Self> {
        unsafe { Self::new_inner(device, source, slot_count, D3d11RingMode::Fence, true) }
    }

    unsafe fn new_inner(
        device: &ID3D11Device,
        source: &D3D11_TEXTURE2D_DESC,
        slot_count: u32,
        mode: D3d11RingMode,
        gdi_compatible: bool,
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
        shared_desc.MiscFlags = if mode.uses_keyed_mutex() {
            (D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX).0 as u32
        } else {
            let mut flags = D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED;
            if gdi_compatible {
                flags |= D3D11_RESOURCE_MISC_GDI_COMPATIBLE;
            }
            flags.0 as u32
        };

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
            let keyed_mutex = if mode.uses_keyed_mutex() {
                Some(
                    texture
                        .cast::<IDXGIKeyedMutex>()
                        .map_err(|error| contextual(error, "IDXGIKeyedMutex query"))?,
                )
            } else {
                None
            };
            slots.push(D3d11ImageSlot {
                texture,
                keyed_mutex,
                handle: unsafe { OwnedHandle::from_raw_handle(handle.0) },
            });
        }

        let (fence, fence_handle, consumer_fence, consumer_fence_handle) = match mode {
            D3d11RingMode::KeyedMutexNoFence => (None, None, None, None),
            D3d11RingMode::KeyedMutex => {
                let device5: ID3D11Device5 = device
                    .cast()
                    .map_err(|error| contextual(error, "ID3D11Device5 query"))?;
                let (fence, handle) = create_shared_fence(&device5)?;
                (Some(fence), Some(handle), None, None)
            }
            D3d11RingMode::Fence => {
                let device5: ID3D11Device5 = device
                    .cast()
                    .map_err(|error| contextual(error, "ID3D11Device5 query"))?;
                let (fence, handle) = create_shared_fence(&device5)?;
                let (consumer_fence, consumer_handle) = create_shared_fence(&device5)?;
                (
                    Some(fence),
                    Some(handle),
                    Some(consumer_fence),
                    Some(consumer_handle),
                )
            }
        };

        Ok(Self {
            desc,
            mode,
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

    pub fn mode(&self) -> D3d11RingMode {
        self.mode
    }

    pub fn slots(&self) -> &[D3d11ImageSlot] {
        &self.slots
    }

    pub fn fence(&self) -> Option<&ID3D11Fence> {
        self.fence.as_ref()
    }

    pub fn fence_handle(&self) -> Option<&OwnedHandle> {
        self.fence_handle.as_ref()
    }

    pub fn raw_fence_handle(&self) -> Option<RawHandle> {
        self.fence_handle.as_ref().map(AsRawHandle::as_raw_handle)
    }

    pub fn consumer_fence(&self) -> Option<&ID3D11Fence> {
        self.consumer_fence.as_ref()
    }

    pub fn raw_consumer_fence_handle(&self) -> Option<RawHandle> {
        self.consumer_fence_handle
            .as_ref()
            .map(AsRawHandle::as_raw_handle)
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
