//! Native D3D12/DXGI viewer compositor.
#![allow(unsafe_code)]

use crate::webrtc::WebRtcCompositor;

use std::{
    collections::VecDeque,
    error::Error,
    ffi::c_void,
    mem::{ManuallyDrop, size_of},
    ptr,
    time::Duration,
};

use polymer_consumer_d3d12::D3d12ConsumerDevice;
use polymer_ipc::StreamReady;
use polymer_protocol::{AdapterLuid, FrameReady, RELEASE_ALL_SYNC_VALUE};
use polymer_renderer::renderer::{CompositingPlan, ReceivedSurface, RenderWindow, SwapchainMode};
use raw_window_handle::RawWindowHandle;
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, RECT, WAIT_OBJECT_0},
        Graphics::{
            Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, Fxc::D3DCompile, ID3DBlob},
            Direct3D12::*,
            Dxgi::{Common::*, *},
        },
        System::Threading::{CreateEventW, INFINITE, WaitForSingleObject},
    },
    core::{Interface, PCSTR},
};

use super::D3d12Receiver;
use super::surface::{ImportedSurface, SURFACE_DESCRIPTORS, SurfaceIdentity};
use polymer_consumer_d3d12::ImportedD3d12Stream;

mod pacing;
mod present;
mod resources;
mod setup;
mod snapshot;
mod streams;
use pacing::*;
use resources::*;
pub use resources::create_texture;
pub(super) use resources::{create_srv, transition};
use setup::*;

const FRAME_COUNT: usize = 2;
const DESCRIPTOR_COUNT: u32 = 1024;

/// Liveness bound on waiting for DXGI presentation capacity. Long enough that
/// it never truncates a legitimate wait for the next refresh, short enough that
/// a swapchain which stops signalling cannot stall an event loop.
const FRAME_PERMIT_TIMEOUT_MS: u32 = 250;
const FIRST_CONTENT_DESCRIPTOR: u32 = 0;
const SWAPCHAIN_FORMAT: DXGI_FORMAT = DXGI_FORMAT_B8G8R8A8_UNORM;

#[repr(C)]
#[derive(Clone, Copy)]
struct GpuVertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [u8; 4],
    local: [f32; 2],
    /// Destination width, height, corner radius, and whether sampled RGB is
    /// straight alpha and must be premultiplied before blending.
    clip: [f32; 4],
}

/// A persistently mapped upload heap that one frame's vertex and index data is
/// suballocated from.
///
/// Every draw call used to create its own committed resource. That put a pair
/// of kernel allocations between every quad, which the driver serialises
/// against the same device the guest is rendering on, and it landed as
/// present-time jitter rather than as an obvious cost anywhere.
struct UploadArena {
    buffer: Option<ID3D12Resource>,
    address: u64,
    /// The persistent mapping, held as an integer so the arena keeps the
    /// auto traits the rest of the compositor has.
    mapped: usize,
    capacity: u64,
    used: u64,
}

/// One present, linked back to the guest frame it carried.
#[derive(Clone, Copy, Debug)]
struct PresentedFrame {
    /// DXGI's running present count at the moment this was presented, which is
    /// what frame statistics later report back as displayed.
    present_count: u32,
    /// When the producer captured the frame this present carried, on the same
    /// QPC clock the display's vblank is reported on.
    captured_qpc: u64,
}

/// Measures how long a guest frame takes to reach the glass.
///
/// Every other latency figure in this codebase stops at some internal boundary
/// — the producer stamps a frame, the consumer notes when it arrived — and none
/// of them say anything about the part the user actually sees, which is
/// dominated by waiting for a composition pass and a scanout. DXGI reports
/// which vblank a given present went out on, so pairing that against the
/// capture stamp measures the whole path on one clock.
#[derive(Debug, Default)]
struct DisplayLatency {
    /// Presents issued but not yet known to have reached the display.
    pending: VecDeque<PresentedFrame>,
}

pub struct D3d12Compositor {
    receiver: D3d12Receiver,
    mode: SwapchainMode,
    swapchain: IDXGISwapChain3,
    swapchain_flags: DXGI_SWAP_CHAIN_FLAG,
    present_interval: u32,
    frame_latency_waitable: Option<HANDLE>,
    frame_permit: bool,
    removal_reported: bool,
    extent: (u32, u32),
    backbuffers: Vec<ID3D12Resource>,
    rtv_heap: ID3D12DescriptorHeap,
    rtv_stride: u32,
    srv_heap: ID3D12DescriptorHeap,
    srv_stride: u32,
    root_signature: ID3D12RootSignature,
    content_pipeline: ID3D12PipelineState,
    blend_pipeline: ID3D12PipelineState,
    allocators: Vec<ID3D12CommandAllocator>,
    command_list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_event: HANDLE,
    fence_value: u64,
    frame_values: [u64; FRAME_COUNT],
    frame_garbage: [Vec<ID3D12Resource>; FRAME_COUNT],
    frame_uploads: [UploadArena; FRAME_COUNT],
    surfaces: Vec<ImportedSurface>,
    display_latency: DisplayLatency,
    /// QPC ticks per second, for turning vblank stamps into milliseconds.
    counter_frequency: f64,
}

// D3D12 devices, queues, resources, DXGI swapchains, fences, and Win32 event
// handles are free-threaded. A compositor is never shared: native hosts may
// transfer exclusive ownership to a presentation thread before that thread
// begins using it. `windows::Win32::Foundation::HANDLE` is conservatively not
// `Send` because it stores a raw pointer, so state that otherwise consists of
// thread-safe Windows handles needs this explicit ownership guarantee.
unsafe impl Send for D3d12Compositor {}

/// Open D3D12 frame after game composition and before presentation.
///
/// Viewer-owned renderers may append commands through this narrowly scoped
/// pass. The compositor remains responsible for acquisition, synchronization,
/// submission, and presentation.
pub struct D3d12RenderPass<'a> {
    device: &'a ID3D12Device,
    command_list: &'a ID3D12GraphicsCommandList,
    frame_index: usize,
    extent: (u32, u32),
}

impl D3d12RenderPass<'_> {
    pub const fn frame_index(&self) -> usize {
        self.frame_index
    }

    pub const fn extent(&self) -> (u32, u32) {
        self.extent
    }

    pub const fn format(&self) -> DXGI_FORMAT {
        SWAPCHAIN_FORMAT
    }

    pub fn device(&self) -> &ID3D12Device {
        self.device
    }

    pub fn command_list(&self) -> &ID3D12GraphicsCommandList {
        self.command_list
    }
}

#[derive(Clone, Copy)]
enum SwapchainTarget {
    Hwnd(HWND),
    Composition {
        transparent: bool,
        /// DirectComposition still accepts a DXGI sync interval. The shared
        /// opaque game surface uses one so its queue advances on vblank like
        /// the lightweight HWND viewer, while auxiliary transparent visuals
        /// remain nonblocking.
        sync_interval: u32,
    },
}

impl D3d12Compositor {
    pub fn adapter_luid(&self) -> AdapterLuid {
        self.receiver.adapter_luid()
    }

    pub fn d3d12_device(&self) -> &ID3D12Device {
        self.receiver.device()
    }

    pub const fn target_format(&self) -> DXGI_FORMAT {
        SWAPCHAIN_FORMAT
    }

    pub const fn frames_in_flight(&self) -> usize {
        FRAME_COUNT
    }

    pub const fn swapchain_mode(&self) -> SwapchainMode {
        self.mode
    }

    /// Returns the composition swapchain as content for an
    /// `IDCompositionVisual`.
    pub fn composition_content(&self) -> Result<windows::core::IUnknown, windows::core::Error> {
        self.swapchain.cast()
    }
}

impl Drop for D3d12Compositor {
    fn drop(&mut self) {
        let _ = self.release_imported_streams();
        unsafe {
            let _ = CloseHandle(self.fence_event);
            if let Some(handle) = self.frame_latency_waitable {
                let _ = CloseHandle(handle);
            }
        }
    }
}
