//! Stable, graphics-API-neutral messages exchanged between Snowglass processes.

#![forbid(unsafe_code)]

pub const PROTOCOL_VERSION: u32 = 8;
pub const MAX_IMAGE_SLOTS: usize = 4;
/// Terminal consumer-fence value used when abandoning an entire image ring.
pub const RELEASE_ALL_SYNC_VALUE: u64 = u64::MAX - 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct Handshake {
    pub protocol_version: u32,
    pub process_id: u32,
    pub preferred_consumer: ConsumerApi,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ConsumerApi {
    Unspecified = 0,
    Vulkan = 1,
    D3d12 = 2,
}

impl TryFrom<u32> for ConsumerApi {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Unspecified),
            1 => Ok(Self::Vulkan),
            2 => Ok(Self::D3d12),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct StreamId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct HandleId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct AdapterLuid {
    pub low_part: u32,
    pub high_part: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ExternalImageKind {
    VulkanOpaqueWin32 = 1,
    D3d11SharedResourceNt = 2,
    D3d12Resource = 3,
    D3d12Heap = 4,
    /// D3D12 shared resource owned and written through OpenGL EXT external objects.
    OpenGlD3d12Resource = 5,
    /// D3D12 shared resource owned by D3D12 and written through Vulkan external memory.
    VulkanD3d12Resource = 6,
    /// D3D11 shared resource opened by D3D12 through its NT handle.
    D3d11D3d12Resource = 7,
}

impl TryFrom<u32> for ExternalImageKind {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::VulkanOpaqueWin32),
            2 => Ok(Self::D3d11SharedResourceNt),
            3 => Ok(Self::D3d12Resource),
            4 => Ok(Self::D3d12Heap),
            5 => Ok(Self::OpenGlD3d12Resource),
            6 => Ok(Self::VulkanD3d12Resource),
            7 => Ok(Self::D3d11D3d12Resource),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ExternalSyncKind {
    VulkanBinarySemaphore = 1,
    VulkanTimelineSemaphore = 2,
    D3d11Fence = 3,
    D3d11KeyedMutex = 4,
    D3d12Fence = 5,
}

impl TryFrom<u32> for ExternalSyncKind {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::VulkanBinarySemaphore),
            2 => Ok(Self::VulkanTimelineSemaphore),
            3 => Ok(Self::D3d11Fence),
            4 => Ok(Self::D3d11KeyedMutex),
            5 => Ok(Self::D3d12Fence),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ExternalImageDesc {
    pub kind: ExternalImageKind,
    pub handle_id: HandleId,
    pub adapter_luid: AdapterLuid,
    pub width: u32,
    pub height: u32,
    /// Backend-neutral format identifier negotiated during the handshake.
    pub format: u32,
    pub allocation_size: u64,
    pub allocation_offset: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct FrameReady {
    pub stream_id: StreamId,
    pub slot: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub capture_x: u32,
    pub capture_y: u32,
    pub capture_width: u32,
    pub capture_height: u32,
    pub frame_number: u64,
    pub sync_value: u64,
    pub timestamp_qpc: u64,
}
