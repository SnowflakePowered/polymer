//! Stable, graphics-API-neutral messages and contracts used by Polymer.

#![forbid(unsafe_code)]

pub mod configuration;
pub mod input;

pub use configuration::{RenderConfiguration, SurfaceRequest};

pub const PROTOCOL_VERSION: u32 = 25;
pub const MAX_IMAGE_SLOTS: usize = 4;
/// Terminal consumer-fence value used when abandoning an entire image ring.
pub const RELEASE_ALL_SYNC_VALUE: u64 = u64::MAX - 1;

/// Returns a sampleable linear member of a typeless or sRGB DXGI color family.
///
/// Shared transport images carry presentation bytes without applying a color
/// transfer function. They also need a typed format so consumers can create a
/// shader-resource view without reconstructing the producer's view.
pub const fn transport_unorm_dxgi_format(format: u32) -> u32 {
    match format {
        9 => 11,  // R16G16B16A16_TYPELESS -> R16G16B16A16_UNORM
        23 => 24, // R10G10B10A2_TYPELESS -> R10G10B10A2_UNORM
        27 => 28, // R8G8B8A8_TYPELESS -> R8G8B8A8_UNORM
        29 => 28, // R8G8B8A8_UNORM_SRGB -> R8G8B8A8_UNORM
        33 => 35, // R16G16_TYPELESS -> R16G16_UNORM
        48 => 49, // R8G8_TYPELESS -> R8G8_UNORM
        53 => 56, // R16_TYPELESS -> R16_UNORM
        60 => 61, // R8_TYPELESS -> R8_UNORM
        70 => 71, // BC1_TYPELESS -> BC1_UNORM
        72 => 71, // BC1_UNORM_SRGB -> BC1_UNORM
        73 => 74, // BC2_TYPELESS -> BC2_UNORM
        75 => 74, // BC2_UNORM_SRGB -> BC2_UNORM
        76 => 77, // BC3_TYPELESS -> BC3_UNORM
        78 => 77, // BC3_UNORM_SRGB -> BC3_UNORM
        79 => 80, // BC4_TYPELESS -> BC4_UNORM
        82 => 83, // BC5_TYPELESS -> BC5_UNORM
        90 => 87, // B8G8R8A8_TYPELESS -> B8G8R8A8_UNORM
        91 => 87, // B8G8R8A8_UNORM_SRGB -> B8G8R8A8_UNORM
        92 => 88, // B8G8R8X8_TYPELESS -> B8G8R8X8_UNORM
        93 => 88, // B8G8R8X8_UNORM_SRGB -> B8G8R8X8_UNORM
        97 => 98, // BC7_TYPELESS -> BC7_UNORM
        99 => 98, // BC7_UNORM_SRGB -> BC7_UNORM
        _ => format,
    }
}

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
    Metal = 3,
}

impl TryFrom<u32> for ConsumerApi {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Unspecified),
            1 => Ok(Self::Vulkan),
            2 => Ok(Self::D3d12),
            3 => Ok(Self::Metal),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct SurfaceId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct AdapterLuid {
    pub low_part: u32,
    pub high_part: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ExternalImageKind {
    /// Fence-synchronized D3D11 shared resource, opened through its NT handle
    /// by D3D12 or as D3D11_TEXTURE external memory by Vulkan.
    D3d11Resource = 1,
    /// D3D12 shared resource opened through its NT handle.
    D3d12Resource = 2,
    IOSurface = 3,
}

impl TryFrom<u32> for ExternalImageKind {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::D3d11Resource),
            2 => Ok(Self::D3d12Resource),
            3 => Ok(Self::IOSurface),
            _ => Err(()),
        }
    }
}

/// Low bits of a ready-timeline value that carry the published image slot.
pub const READY_SLOT_BITS: u32 = 2;
const _: () = assert!(MAX_IMAGE_SLOTS <= 1 << READY_SLOT_BITS);

/// A frame observed on a producer's ready timeline. Producers signal
/// `(sequence << READY_SLOT_BITS) | slot`, so the newest completed value alone
/// identifies the latest renderable image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameReady {
    pub slot: u32,
    pub frame_number: u64,
    pub sync_value: u64,
}

impl FrameReady {
    pub const fn ready_value(sequence: u64, slot: u32) -> u64 {
        (sequence << READY_SLOT_BITS) | slot as u64
    }

    pub const fn sequence(value: u64) -> u64 {
        value >> READY_SLOT_BITS
    }

    pub const fn observed(sync_value: u64) -> Self {
        Self {
            slot: (sync_value & ((1 << READY_SLOT_BITS) - 1)) as u32,
            frame_number: Self::sequence(sync_value),
            sync_value,
        }
    }
}

mod hooks;
pub use hooks::{GraphicsHook, Hooks, InputHook};
