//! Stable, graphics-API-neutral messages and contracts used by Polymer.

#![forbid(unsafe_code)]

mod capture;
pub mod input;

pub use capture::{CaptureError, FrameConsumer, FrameProducer, SurfaceDesc};

pub const PROTOCOL_VERSION: u32 = 18;
/// Fractional bits used by coordinates in [`PresentationPoint`].
pub const PRESENTATION_COORD_FRACTIONAL_BITS: u32 = 16;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct SurfaceId(pub u64);

/// Requests a new client-area extent for a window in the producer process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ResizeWindow {
    pub target_window: i64,
    pub width: u32,
    pub height: u32,
}

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
    D3d12Resource = 3,
    D3d12Heap = 4,
    /// D3D12 shared resource owned and written through OpenGL EXT external objects.
    /// Pixel rows retain OpenGL's bottom-left origin; consumers convert them
    /// to their target coordinate space while presenting.
    OpenGlD3d12Resource = 5,
    /// D3D12 shared resource owned by D3D12 and written through Vulkan external memory.
    VulkanD3d12Resource = 6,
    /// Fence-synchronized D3D11 shared resource, opened through its NT handle
    /// by D3D12 or as D3D11_TEXTURE external memory by Vulkan.
    D3d11D3d12Resource = 7,
    /// D3D11 shared resource painted through a GDI surface DC and opened by
    /// D3D12 through its NT handle.
    GdiD3d12Resource = 8,
}

impl TryFrom<u32> for ExternalImageKind {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::VulkanOpaqueWin32),
            3 => Ok(Self::D3d12Resource),
            4 => Ok(Self::D3d12Heap),
            5 => Ok(Self::OpenGlD3d12Resource),
            6 => Ok(Self::VulkanD3d12Resource),
            7 => Ok(Self::D3d11D3d12Resource),
            8 => Ok(Self::GdiD3d12Resource),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ExternalSyncKind {
    VulkanBinarySemaphore = 1,
    VulkanTimelineSemaphore = 2,
    D3d12Fence = 5,
}

impl TryFrom<u32> for ExternalSyncKind {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::VulkanBinarySemaphore),
            2 => Ok(Self::VulkanTimelineSemaphore),
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
    /// Valid captured pixels packed at the origin of the shared image. These
    /// dimensions determine the image's displayed aspect ratio.
    pub render_width: u32,
    pub render_height: u32,
    /// Axis-aligned bounds of the captured image in the producer's
    /// presentation coordinate space. The exact orientation is described by
    /// `presentation_transform`.
    pub capture_x: u32,
    pub capture_y: u32,
    pub capture_width: u32,
    pub capture_height: u32,
    /// Extent of the presentation coordinate system containing the capture
    /// rectangle. This may differ from both the shared image and target client
    /// extents when the application uses dynamic resolution or a frame graph.
    pub presentation_width: u32,
    pub presentation_height: u32,
    pub presentation_transform: PresentationTransform,
    pub frame_number: u64,
    pub sync_value: u64,
    pub timestamp_qpc: u64,
}

/// Describes whether a captured image can be mapped back to its presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum PresentationMappingKind {
    Unavailable = 0,
    AxisAligned = 1,
    Affine = 2,
}

impl TryFrom<u32> for PresentationMappingKind {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Unavailable),
            1 => Ok(Self::AxisAligned),
            2 => Ok(Self::Affine),
            _ => Err(()),
        }
    }
}

/// A signed 48.16 fixed-point position in target presentation coordinates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct PresentationPoint {
    pub x: i64,
    pub y: i64,
}

impl PresentationPoint {
    pub const fn from_pixels(x: i32, y: i32) -> Self {
        Self {
            x: (x as i64) << PRESENTATION_COORD_FRACTIONAL_BITS,
            y: (y as i64) << PRESENTATION_COORD_FRACTIONAL_BITS,
        }
    }
}

/// Maps the consumer-normalized captured image into the target presentation.
///
/// `upper_left`, `upper_right`, and `lower_left` are the presentation-space
/// positions of the corresponding corners after consumer-side normalization.
/// The fourth corner is implied by the parallelogram. Storage orientation such
/// as `StreamReady::bottom_up` and an explicitly configured display rotation
/// are applied before this transform. This lets a producer report placement
/// and scale without needing to know the consumer's normalization policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct PresentationTransform {
    pub kind: PresentationMappingKind,
    pub upper_left: PresentationPoint,
    pub upper_right: PresentationPoint,
    pub lower_left: PresentationPoint,
}

impl PresentationTransform {
    pub const UNAVAILABLE: Self = Self {
        kind: PresentationMappingKind::Unavailable,
        upper_left: PresentationPoint { x: 0, y: 0 },
        upper_right: PresentationPoint { x: 0, y: 0 },
        lower_left: PresentationPoint { x: 0, y: 0 },
    };

    pub fn axis_aligned(x: u32, y: u32, width: u32, height: u32) -> Self {
        let left = fixed_unsigned(u64::from(x));
        let top = fixed_unsigned(u64::from(y));
        let right = fixed_unsigned(u64::from(x) + u64::from(width));
        let bottom = fixed_unsigned(u64::from(y) + u64::from(height));
        Self {
            kind: PresentationMappingKind::AxisAligned,
            upper_left: PresentationPoint { x: left, y: top },
            upper_right: PresentationPoint { x: right, y: top },
            lower_left: PresentationPoint { x: left, y: bottom },
        }
    }

    pub const fn affine(
        upper_left: PresentationPoint,
        upper_right: PresentationPoint,
        lower_left: PresentationPoint,
    ) -> Self {
        Self {
            kind: PresentationMappingKind::Affine,
            upper_left,
            upper_right,
            lower_left,
        }
    }
}

fn fixed_unsigned(value: u64) -> i64 {
    (value as i64) << PRESENTATION_COORD_FRACTIONAL_BITS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_aligned_transform_uses_target_rectangle_corners() {
        let transform = PresentationTransform::axis_aligned(10, 20, 300, 400);
        assert_eq!(transform.kind, PresentationMappingKind::AxisAligned);
        assert_eq!(transform.upper_left, PresentationPoint::from_pixels(10, 20));
        assert_eq!(
            transform.upper_right,
            PresentationPoint::from_pixels(310, 20)
        );
        assert_eq!(
            transform.lower_left,
            PresentationPoint::from_pixels(10, 420)
        );
    }

    #[test]
    fn affine_transform_preserves_a_quarter_turn() {
        let transform = PresentationTransform::affine(
            PresentationPoint::from_pixels(240, 0),
            PresentationPoint::from_pixels(240, 400),
            PresentationPoint::from_pixels(0, 0),
        );
        assert_eq!(transform.kind, PresentationMappingKind::Affine);
        assert_eq!(
            transform.upper_right,
            PresentationPoint::from_pixels(240, 400)
        );
        assert_eq!(transform.lower_left, PresentationPoint::from_pixels(0, 0));
    }

    #[test]
    fn transport_formats_use_linear_unorm_members() {
        assert_eq!(transport_unorm_dxgi_format(9), 11);
        assert_eq!(transport_unorm_dxgi_format(23), 24);
        assert_eq!(transport_unorm_dxgi_format(27), 28);
        assert_eq!(transport_unorm_dxgi_format(29), 28);
        assert_eq!(transport_unorm_dxgi_format(33), 35);
        assert_eq!(transport_unorm_dxgi_format(48), 49);
        assert_eq!(transport_unorm_dxgi_format(53), 56);
        assert_eq!(transport_unorm_dxgi_format(60), 61);
        assert_eq!(transport_unorm_dxgi_format(70), 71);
        assert_eq!(transport_unorm_dxgi_format(91), 87);
        assert_eq!(transport_unorm_dxgi_format(97), 98);
        assert_eq!(transport_unorm_dxgi_format(99), 98);
        assert_eq!(transport_unorm_dxgi_format(10), 10);
    }
}
