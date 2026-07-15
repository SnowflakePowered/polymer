//! Stable, graphics-API-neutral messages exchanged between Snowglass processes.

#![forbid(unsafe_code)]

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_IMAGE_SLOTS: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct Handshake {
    pub protocol_version: u32,
    pub process_id: u32,
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
    pub _reserved: u32,
    pub frame_number: u64,
    pub sync_value: u64,
    pub timestamp_qpc: u64,
}
