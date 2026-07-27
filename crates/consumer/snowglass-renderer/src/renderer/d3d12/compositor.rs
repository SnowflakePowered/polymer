//! Native D3D12/DXGI viewer compositor.
#![allow(unsafe_code)]

use std::{
    collections::{HashMap, VecDeque},
    error::Error,
    ffi::c_void,
    mem::{ManuallyDrop, size_of},
    ptr,
    time::{Duration, Instant},
};

use crate::renderer::{
    CompositingPlan, PlanLayer, ReceivedSurface, RenderWindow, SwapchainMode, empty_interface,
    webrtc::WebRtcCompositor,
};
use egui::{ClippedPrimitive, TextureId, epaint::Primitive};
use raw_window_handle::RawWindowHandle;
use snowglass_consumer_d3d12::{D3d12ConsumerDevice, ImportedD3d12Stream};
use snowglass_ipc::StreamReady;
use snowglass_protocol::{AdapterLuid, FrameReady, RELEASE_ALL_SYNC_VALUE};
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

const FRAME_COUNT: usize = 2;
const DESCRIPTOR_COUNT: u32 = 1024;

const KEYED_MUTEX_TIMEOUT_MS: u32 = 100;
/// Liveness bound on waiting for DXGI presentation capacity. Long enough that
/// it never truncates a legitimate wait for the next refresh, short enough that
/// a swapchain which stops signalling cannot stall an event loop.
const FRAME_PERMIT_TIMEOUT_MS: u32 = 250;
const FIRST_CONTENT_DESCRIPTOR: u32 = 0;
const FIRST_EGUI_DESCRIPTOR: u32 = 64;
const SWAPCHAIN_FORMAT: DXGI_FORMAT = DXGI_FORMAT_B8G8R8A8_UNORM;

#[repr(C)]
#[derive(Clone, Copy)]
struct GpuVertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [u8; 4],
}

struct EguiTexture {
    resource: ID3D12Resource,
    pixels: Vec<u8>,
    size: [usize; 2],
    descriptor: u32,
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

impl UploadArena {
    const ALIGNMENT: u64 = 16;
    const INITIAL_CAPACITY: u64 = 256 * 1024;

    const fn new() -> Self {
        Self {
            buffer: None,
            address: 0,
            mapped: 0,
            capacity: 0,
            used: 0,
        }
    }

    /// Hands the whole arena back at the start of a frame. Safe only once that
    /// frame's fence has been waited on, since the GPU reads these bytes.
    const fn reset(&mut self) {
        self.used = 0;
    }

    /// Where `length` bytes starting from `used` land, or `None` when they do
    /// not fit and the arena has to grow first.
    const fn placement(used: u64, capacity: u64, length: u64) -> Option<u64> {
        // Rounding up is itself fallible: `used` is bounded by `capacity` in
        // practice, but an unchecked round would panic rather than report the
        // miss it is supposed to report.
        let Some(offset) = used.checked_next_multiple_of(Self::ALIGNMENT) else {
            return None;
        };
        match offset.checked_add(length) {
            Some(end) if end <= capacity => Some(offset),
            _ => None,
        }
    }

    /// The capacity a grown arena needs to hold `length` bytes.
    const fn grown_capacity(capacity: u64, length: u64) -> u64 {
        // Geometric, so a steady-state frame stops reallocating after the
        // first few passes rather than growing once per draw call.
        let doubled = capacity.saturating_mul(2);
        let floor = if doubled > Self::INITIAL_CAPACITY {
            doubled
        } else {
            Self::INITIAL_CAPACITY
        };
        let needed = length.next_multiple_of(Self::ALIGNMENT);
        if floor > needed { floor } else { needed }
    }

    /// Copies `bytes` into the arena and returns the GPU address they live at.
    fn write(
        &mut self,
        device: &ID3D12Device,
        bytes: &[u8],
        garbage: &mut Vec<ID3D12Resource>,
    ) -> windows::core::Result<u64> {
        let length = (bytes.len() as u64).max(1);
        let mut offset = Self::placement(self.used, self.capacity, length);
        if self.buffer.is_none() || offset.is_none() {
            // The retired buffer stays alive in this frame's garbage, so views
            // already recorded against it remain valid until the frame's fence
            // passes.
            self.replace(device, Self::grown_capacity(self.capacity, length), garbage)?;
            offset = Self::placement(0, self.capacity, length);
        }
        let offset = offset.ok_or_else(|| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_OUTOFMEMORY)
        })?;
        // SAFETY: the buffer is mapped for its whole lifetime and the range
        // [offset, offset + bytes.len()) was just checked to fit in it.
        unsafe {
            ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                (self.mapped as *mut u8).add(offset as usize),
                bytes.len(),
            );
        }
        self.used = offset + length;
        Ok(self.address + offset)
    }

    fn replace(
        &mut self,
        device: &ID3D12Device,
        capacity: u64,
        garbage: &mut Vec<ID3D12Resource>,
    ) -> windows::core::Result<()> {
        // SAFETY: the resource is created and mapped here and stays owned by
        // this arena until it is retired into the frame's garbage.
        let (buffer, address, mapped) = unsafe {
            let buffer = create_upload_buffer(device, capacity)?;
            let mut mapped = ptr::null_mut();
            // An upload heap may stay mapped for its whole life; the empty read
            // range says the CPU never reads back what it wrote.
            buffer.Map(
                0,
                Some(&D3D12_RANGE { Begin: 0, End: 0 }),
                Some(&mut mapped),
            )?;
            let address = buffer.GetGPUVirtualAddress();
            (buffer, address, mapped)
        };
        self.address = address;
        self.mapped = mapped as usize;
        self.capacity = capacity;
        self.used = 0;
        if let Some(previous) = self.buffer.replace(buffer) {
            garbage.push(previous);
        }
        Ok(())
    }
}

struct ImportedSurface {
    connection: u64,
    surface_id: u64,
    target_window: i64,
    descriptor: u32,
    imported: ImportedD3d12Stream,
    flip_y: bool,
    quarter_turns: u8,
    cache: ID3D12Resource,
    cache_state: D3D12_RESOURCE_STATES,
    latest: Option<FrameReady>,
    display_frame: Option<FrameReady>,
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
    samples: u32,
    total_ms: f64,
    worst_ms: f64,
    report: Option<DisplayLatencyReport>,
    started: Option<Instant>,
}

/// Guest-capture-to-scanout latency over one sampling window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DisplayLatencyReport {
    pub average_ms: f64,
    pub worst_ms: f64,
    /// Presents the display confirmed in this window. Far below the present
    /// rate means most presents could not be attributed and the average is
    /// drawn from a thin sample.
    pub confirmed: u32,
}

impl DisplayLatency {
    /// Bounds how many unconfirmed presents are tracked. Frame statistics can
    /// go unavailable for stretches, and this is diagnostics: dropping the
    /// oldest is right, growing without limit is not.
    const PENDING_LIMIT: usize = 64;

    fn record(&mut self, present_count: u32, captured_qpc: u64) {
        if self.pending.len() >= Self::PENDING_LIMIT {
            self.pending.pop_front();
        }
        self.pending.push_back(PresentedFrame {
            present_count,
            captured_qpc,
        });
    }

    /// Attributes a vblank to the present it displayed.
    ///
    /// `displayed_qpc` is only meaningful for the present DXGI names, so
    /// everything older is discarded rather than guessed at: an earlier present
    /// went out on an earlier vblank whose time was never sampled.
    fn observe(&mut self, present_count: u32, displayed_qpc: u64, frequency: f64) {
        while self
            .pending
            .front()
            .is_some_and(|frame| frame.present_count < present_count)
        {
            self.pending.pop_front();
        }
        let Some(frame) = self
            .pending
            .front()
            .filter(|frame| frame.present_count == present_count)
            .copied()
        else {
            return;
        };
        self.pending.pop_front();
        let Some(ticks) = displayed_qpc.checked_sub(frame.captured_qpc) else {
            return;
        };
        let milliseconds = ticks as f64 * 1000.0 / frequency;
        self.samples += 1;
        self.total_ms += milliseconds;
        self.worst_ms = self.worst_ms.max(milliseconds);
    }

    fn sample(&mut self, now: Instant) -> Option<DisplayLatencyReport> {
        let started = *self.started.get_or_insert(now);
        if now.saturating_duration_since(started) < crate::debug::STATISTICS_SAMPLE_INTERVAL {
            return self.report;
        }
        if self.samples > 0 {
            self.report = Some(DisplayLatencyReport {
                average_ms: self.total_ms / f64::from(self.samples),
                worst_ms: self.worst_ms,
                confirmed: self.samples,
            });
        }
        self.samples = 0;
        self.total_ms = 0.0;
        self.worst_ms = 0.0;
        self.started = Some(now);
        self.report
    }
}

/// Shell-rasterised pixels the compositor holds so a plan can draw them
/// between the streams.
struct ChromeLayer {
    resource: ID3D12Resource,
    descriptor: u32,
    /// The handle this was opened from, so an unchanged band is not reopened.
    handle: isize,
}

pub struct D3d12Compositor {
    receiver: D3d12Receiver,
    mode: SwapchainMode,
    swapchain: IDXGISwapChain3,
    swapchain_flags: DXGI_SWAP_CHAIN_FLAG,
    present_interval: u32,
    frame_latency_waitable: Option<HANDLE>,
    frame_permit: bool,
    extent: (u32, u32),
    backbuffers: Vec<ID3D12Resource>,
    rtv_heap: ID3D12DescriptorHeap,
    rtv_stride: u32,
    srv_heap: ID3D12DescriptorHeap,
    srv_stride: u32,
    root_signature: ID3D12RootSignature,
    content_pipeline: ID3D12PipelineState,
    egui_pipeline: ID3D12PipelineState,
    allocators: Vec<ID3D12CommandAllocator>,
    command_list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_event: HANDLE,
    fence_value: u64,
    frame_values: [u64; FRAME_COUNT],
    frame_garbage: [Vec<ID3D12Resource>; FRAME_COUNT],
    frame_descriptors: [Vec<u32>; FRAME_COUNT],
    frame_uploads: [UploadArena; FRAME_COUNT],
    surfaces: Vec<ImportedSurface>,
    // Standalone copy submission used to bound keyed-mutex holds; the main
    // command list executes too late to release the mutex promptly.
    mutex_copy: Option<(ID3D12CommandAllocator, ID3D12GraphicsCommandList)>,
    textures: HashMap<TextureId, EguiTexture>,
    chrome: HashMap<u64, ChromeLayer>,
    /// Chrome handed over since the last frame. Opening a shared texture takes
    /// a descriptor slot, and a slot cannot be reused while an in-flight frame
    /// still refers to it, so the work waits for the start of a frame.
    chrome_opens: Vec<(u64, isize)>,
    chrome_removals: Vec<u64>,
    free_descriptors: Vec<u32>,
    display_latency: DisplayLatency,
    /// QPC ticks per second, for turning vblank stamps into milliseconds.
    counter_frequency: f64,
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

fn swapchain_target(window: &impl RenderWindow) -> Result<SwapchainTarget, Box<dyn Error>> {
    match window.window_handle()?.as_raw() {
        RawWindowHandle::Win32(handle) => Ok(SwapchainTarget::Hwnd(HWND(
            handle.hwnd.get() as *mut c_void
        ))),
        _ => Err("D3D12 composition requires a Win32 window".into()),
    }
}

impl D3d12Compositor {
    pub fn new_default(window: &impl RenderWindow) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new_default()?,
            swapchain_target(window)?,
            SwapchainMode::Shared,
        )
    }

    pub fn new_default_with_webrtc(
        window: &impl RenderWindow,
    ) -> Result<(Self, WebRtcCompositor), Box<dyn Error>> {
        let compositor = Self::new_default(window)?;
        let webrtc = WebRtcCompositor::new_d3d12(&compositor)?;
        Ok((compositor, webrtc))
    }

    /// Creates a compositor whose swapchain can be attached to an
    /// `IDCompositionVisual` with [`Self::composition_content`].
    pub fn new_default_for_composition(window: &impl RenderWindow) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new_default()?,
            SwapchainTarget::Composition {
                transparent: false,
                sync_interval: 0,
            },
            SwapchainMode::Shared,
        )
    }

    /// Creates a transparent composition swapchain suitable for an overlay
    /// visual. Its backbuffer alpha is interpreted as premultiplied alpha by
    /// DirectComposition.
    pub fn new_default_for_transparent_composition(
        window: &impl RenderWindow,
    ) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new_default()?,
            SwapchainTarget::Composition {
                transparent: true,
                sync_interval: 0,
            },
            SwapchainMode::Shared,
        )
    }

    pub fn new_default_for_composition_with_webrtc(
        window: &impl RenderWindow,
    ) -> Result<(Self, WebRtcCompositor), Box<dyn Error>> {
        let compositor = Self::new_default_for_composition(window)?;
        let webrtc = WebRtcCompositor::new_d3d12(&compositor)?;
        Ok((compositor, webrtc))
    }

    pub fn new(window: &impl RenderWindow, adapter: AdapterLuid) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new(adapter)?,
            swapchain_target(window)?,
            SwapchainMode::Shared,
        )
    }

    pub fn new_with_webrtc(
        window: &impl RenderWindow,
        adapter: AdapterLuid,
    ) -> Result<(Self, WebRtcCompositor), Box<dyn Error>> {
        let compositor = Self::new(window, adapter)?;
        let webrtc = WebRtcCompositor::new_d3d12(&compositor)?;
        Ok((compositor, webrtc))
    }

    /// Creates a swapchain compositor dedicated to one received connection.
    pub fn new_default_for_stream(
        window: &impl RenderWindow,
        connection: u64,
    ) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new_default()?,
            swapchain_target(window)?,
            SwapchainMode::Dedicated(connection),
        )
    }

    /// Creates a composition swapchain dedicated to one received connection.
    pub fn new_default_for_composition_stream(
        window: &impl RenderWindow,
        connection: u64,
    ) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new_default()?,
            SwapchainTarget::Composition {
                transparent: false,
                sync_interval: 0,
            },
            SwapchainMode::Dedicated(connection),
        )
    }

    /// Creates an adapter-specific swapchain dedicated to one connection.
    pub fn new_for_stream(
        window: &impl RenderWindow,
        adapter: AdapterLuid,
        connection: u64,
    ) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new(adapter)?,
            swapchain_target(window)?,
            SwapchainMode::Dedicated(connection),
        )
    }

    /// Creates an adapter-specific composition swapchain dedicated to one
    /// received connection.
    pub fn new_for_composition_stream(
        window: &impl RenderWindow,
        adapter: AdapterLuid,
        connection: u64,
    ) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new(adapter)?,
            SwapchainTarget::Composition {
                transparent: false,
                sync_interval: 0,
            },
            SwapchainMode::Dedicated(connection),
        )
    }

    /// Creates one adapter-specific composition swapchain that composites every
    /// imported stream. A single visual gives the display one plane to promote
    /// instead of asking it to promote one per stream.
    pub fn new_for_shared_composition(
        window: &impl RenderWindow,
        adapter: AdapterLuid,
    ) -> Result<Self, Box<dyn Error>> {
        Self::with_consumer(
            window,
            D3d12ConsumerDevice::new(adapter)?,
            SwapchainTarget::Composition {
                transparent: false,
                sync_interval: 1,
            },
            SwapchainMode::Shared,
        )
    }

    pub fn new_for_composition_stream_with_webrtc(
        window: &impl RenderWindow,
        adapter: AdapterLuid,
        connection: u64,
    ) -> Result<(Self, WebRtcCompositor), Box<dyn Error>> {
        let compositor = Self::new_for_composition_stream(window, adapter, connection)?;
        let webrtc = WebRtcCompositor::new_d3d12(&compositor)?;
        Ok((compositor, webrtc))
    }

    fn with_consumer(
        window: &impl RenderWindow,
        consumer: D3d12ConsumerDevice,
        target: SwapchainTarget,
        mode: SwapchainMode,
    ) -> Result<Self, Box<dyn Error>> {
        let size = window.render_extent();
        let extent = (size.width.max(1), size.height.max(1));

        // SAFETY: all COM objects and the event are owned by this compositor and GPU work is
        // drained before resources referenced by a submitted command list are replaced.
        unsafe {
            let present_interval = match target {
                SwapchainTarget::Hwnd(_) => 1,
                SwapchainTarget::Composition { sync_interval, .. } => sync_interval,
            };
            let swapchain_flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;
            let description = DXGI_SWAP_CHAIN_DESC1 {
                Width: extent.0,
                Height: extent.1,
                Format: SWAPCHAIN_FORMAT,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: FRAME_COUNT as u32,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: match target {
                    SwapchainTarget::Hwnd(_) => DXGI_SWAP_EFFECT_FLIP_DISCARD,
                    // DXGI requires the sequential flip model for swapchains
                    // supplied as DirectComposition visual content.
                    SwapchainTarget::Composition { .. } => DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                },
                AlphaMode: match target {
                    SwapchainTarget::Composition {
                        transparent: true, ..
                    } => DXGI_ALPHA_MODE_PREMULTIPLIED,
                    _ => DXGI_ALPHA_MODE_IGNORE,
                },
                Flags: swapchain_flags.0 as u32,
            };
            let swapchain1 = match target {
                SwapchainTarget::Hwnd(hwnd) => {
                    let swapchain = consumer.factory().CreateSwapChainForHwnd(
                        consumer.queue(),
                        hwnd,
                        &description,
                        None,
                        None,
                    )?;
                    consumer
                        .factory()
                        .MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER)?;
                    swapchain
                }
                SwapchainTarget::Composition { .. } => consumer
                    .factory()
                    .CreateSwapChainForComposition(consumer.queue(), &description, None)?,
            };
            let swapchain: IDXGISwapChain3 = swapchain1.cast()?;
            let swapchain2: IDXGISwapChain2 = swapchain.cast()?;
            swapchain2.SetMaximumFrameLatency(1)?;
            let handle = swapchain2.GetFrameLatencyWaitableObject();
            if handle.is_invalid() {
                return Err("DXGI did not create a frame-latency waitable object".into());
            }
            let frame_latency_waitable = Some(handle);

            let rtv_heap = consumer
                .device()
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                    NumDescriptors: FRAME_COUNT as u32,
                    ..Default::default()
                })?;
            let srv_heap = consumer
                .device()
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                    NumDescriptors: DESCRIPTOR_COUNT,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    ..Default::default()
                })?;
            let rtv_stride = consumer
                .device()
                .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV);
            let srv_stride = consumer
                .device()
                .GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);
            let backbuffers =
                create_backbuffers(consumer.device(), &swapchain, &rtv_heap, rtv_stride)?;
            let root_signature = create_root_signature(consumer.device())?;
            let content_pipeline = create_pipeline(consumer.device(), &root_signature, false)?;
            let egui_pipeline = create_pipeline(consumer.device(), &root_signature, true)?;
            let mut allocators = Vec::with_capacity(FRAME_COUNT);
            for _ in 0..FRAME_COUNT {
                allocators.push(
                    consumer
                        .device()
                        .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?,
                );
            }
            let command_list: ID3D12GraphicsCommandList = consumer.device().CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                &allocators[0],
                Some(&egui_pipeline),
            )?;
            command_list.Close()?;
            let fence = consumer.device().CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let fence_event = CreateEventW(None, false, false, None)?;

            Ok(Self {
                receiver: D3d12Receiver::from_consumer(consumer),
                mode,
                swapchain,
                swapchain_flags,
                present_interval,
                frame_latency_waitable,
                frame_permit: false,
                extent,
                backbuffers,
                rtv_heap,
                rtv_stride,
                srv_heap,
                srv_stride,
                root_signature,
                content_pipeline,
                egui_pipeline,
                allocators,
                command_list,
                fence,
                fence_event,
                fence_value: 0,
                frame_values: [0; FRAME_COUNT],
                frame_garbage: std::array::from_fn(|_| Vec::new()),
                frame_descriptors: std::array::from_fn(|_| Vec::new()),
                frame_uploads: std::array::from_fn(|_| UploadArena::new()),
                surfaces: Vec::new(),
                mutex_copy: None,
                textures: HashMap::new(),
                chrome: HashMap::new(),
                chrome_opens: Vec::new(),
                chrome_removals: Vec::new(),
                free_descriptors: (FIRST_EGUI_DESCRIPTOR..DESCRIPTOR_COUNT).rev().collect(),
                display_latency: DisplayLatency::default(),
                counter_frequency: counter_frequency(),
            })
        }
    }

    pub fn adapter_luid(&self) -> AdapterLuid {
        self.receiver.adapter_luid()
    }

    pub(crate) fn d3d12_device(&self) -> &ID3D12Device {
        self.receiver.device()
    }

    pub const fn swapchain_mode(&self) -> SwapchainMode {
        self.mode
    }

    /// Returns the composition swapchain as content for an
    /// `IDCompositionVisual`.
    pub fn composition_content(&self) -> Result<windows::core::IUnknown, windows::core::Error> {
        self.swapchain.cast()
    }

    /// Returns whether this compositor can begin another frame without
    /// waiting for DXGI presentation capacity. All receiver swapchains use a
    /// one-frame latency waitable object so the display clock owns pacing.
    pub fn try_begin_frame(&mut self) -> bool {
        self.begin_frame_within(Duration::ZERO)
    }

    /// Blocks until DXGI has presentation capacity, so the caller can gather
    /// the frame it is about to composite *after* the wait instead of before
    /// it.
    ///
    /// Blocking inside `Present` at the end of a frame costs a whole refresh
    /// interval of staleness: everything the frame was built from was sampled
    /// before the wait rather than after it. Waiting here first is what makes
    /// the composited image one vblank old instead of two.
    pub fn begin_frame(&mut self) {
        // The timeout is a liveness guard, not pacing: an occluded or otherwise
        // stalled swapchain must not wedge the caller's event loop. Falling
        // through on a timeout only means `Present` blocks instead.
        self.begin_frame_within(Duration::from_millis(u64::from(FRAME_PERMIT_TIMEOUT_MS)));
    }

    /// Waits up to `budget` for presentation capacity, reporting whether it
    /// arrived.
    ///
    /// A caller already paced by something else — a host waking on the
    /// DirectComposition clock, say — wants a budget rather than either
    /// extreme. DXGI releases the slot for the frame the compositor is retiring
    /// slightly *after* that clock ticks, so polling misses it by a hair and
    /// defers the present a whole interval, while waiting without a bound would
    /// hand the display's pacing control over an event loop that has other work
    /// to do.
    ///
    /// A permit already held is kept rather than waited for again. The waitable
    /// object is a semaphore released once per completed present, so taking a
    /// permit without presenting would leave the next wait with nothing to
    /// consume.
    pub fn begin_frame_within(&mut self, budget: Duration) -> bool {
        if self.frame_permit {
            return true;
        }
        let Some(handle) = self.frame_latency_waitable else {
            return true;
        };
        // Round up, so a sub-millisecond budget still waits instead of quietly
        // degrading into a poll. `WaitForSingleObject` reserves `INFINITE`.
        let milliseconds = u32::try_from(budget.as_nanos().div_ceil(1_000_000))
            .unwrap_or(INFINITE - 1)
            .min(INFINITE - 1);
        // SAFETY: the handle belongs to this live swapchain and is only closed
        // when the compositor is dropped.
        self.frame_permit = unsafe { WaitForSingleObject(handle, milliseconds) } == WAIT_OBJECT_0;
        self.frame_permit
    }

    /// Reports how long guest frames are taking to reach the display.
    ///
    /// Call once per frame. DXGI names the vblank a present went out on, which
    /// is paired here against the moment the producer captured that frame, so
    /// the result spans the whole path — capture, transport, the wait for a
    /// composition pass, and the scanout — on one clock. Panel response is not
    /// included; nothing on this side can see it.
    ///
    /// `None` until enough presents have been confirmed, and on backends or
    /// swapchains where DXGI declines to report statistics at all.
    pub fn display_latency(&mut self) -> Option<DisplayLatencyReport> {
        // SAFETY: the swapchain is live for the compositor's lifetime.
        //
        // Statistics are unavailable on some configurations and go disjoint
        // across mode changes, both reported as an error. Neither is worth
        // surfacing: this is diagnostics, and the next frame usually recovers.
        let mut statistics = DXGI_FRAME_STATISTICS::default();
        if unsafe { self.swapchain.GetFrameStatistics(&mut statistics) }.is_ok() {
            // `SyncQPCTime` timestamps the vblank named by `SyncRefreshCount`.
            // It only dates the present when that present went out on the very
            // same vblank; otherwise the present's own vblank was never
            // timestamped, and inferring one would require a refresh period
            // this does not have. Skipping those keeps every sample measured
            // rather than modelled.
            if statistics.PresentRefreshCount == statistics.SyncRefreshCount {
                self.display_latency.observe(
                    statistics.PresentCount,
                    statistics.SyncQPCTime as u64,
                    self.counter_frequency,
                );
            }
        }
        self.display_latency.sample(Instant::now())
    }

    pub(crate) fn import_stream(
        &mut self,
        connection: u64,
        stream: &StreamReady,
        quarter_turns: u8,
    ) -> Result<(), Box<dyn Error>> {
        if !self.mode.accepts(connection) {
            return Err(
                format!("connection {connection} does not belong to {:?}", self.mode).into(),
            );
        }
        self.disconnect_stream(connection)?;
        let imported = self.receiver.import_stream(stream)?;
        if imported.uses_keyed_mutex() && self.mutex_copy.is_none() {
            unsafe {
                let allocator: ID3D12CommandAllocator = self
                    .receiver
                    .device()
                    .CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
                let list: ID3D12GraphicsCommandList = self.receiver.device().CreateCommandList(
                    0,
                    D3D12_COMMAND_LIST_TYPE_DIRECT,
                    &allocator,
                    None,
                )?;
                list.Close()?;
                self.mutex_copy = Some((allocator, list));
            }
        }
        // The private texture decouples viewer redraws from producer slot reuse.
        let cache = unsafe {
            create_texture(
                self.receiver.device(),
                imported.width,
                imported.height,
                DXGI_FORMAT(imported.format as i32),
                D3D12_RESOURCE_STATE_COMMON,
            )?
        };
        unsafe {
            create_srv(
                self.receiver.device(),
                &self.srv_heap,
                self.srv_stride,
                FIRST_CONTENT_DESCRIPTOR + self.surfaces.len() as u32,
                &cache,
            )
        };
        self.surfaces.push(ImportedSurface {
            connection,
            surface_id: stream.surface_id.0,
            target_window: stream.source_window,
            descriptor: FIRST_CONTENT_DESCRIPTOR + self.surfaces.len() as u32,
            imported,
            flip_y: stream.bottom_up,
            quarter_turns: quarter_turns % 4,
            cache,
            cache_state: D3D12_RESOURCE_STATE_COMMON,
            latest: None,
            display_frame: None,
        });
        self.surfaces
            .sort_by_key(|surface| crate::surface_sort_key(surface.surface_id));
        self.refresh_surface_descriptors()?;
        Ok(())
    }

    pub(crate) fn set_latest(&mut self, connection: u64, frame: FrameReady) {
        let Some(surface) = self
            .surfaces
            .iter_mut()
            .find(|surface| surface.connection == connection)
        else {
            return;
        };
        if let Some(previous) = surface.latest.replace(frame)
            && previous.slot != frame.slot
        {
            surface.imported.recycle_slot(previous.slot);
        }
    }

    pub fn received_surfaces(&self) -> Vec<ReceivedSurface> {
        self.surfaces
            .iter()
            .filter_map(|surface| {
                let frame = surface.latest.or(surface.display_frame)?;
                let (width, height) = crate::fit_inside(
                    frame.render_width.max(1),
                    frame.render_height.max(1),
                    surface.imported.width,
                    surface.imported.height,
                );
                let image_extent = if surface.quarter_turns % 2 == 1 {
                    crate::Extent::new(height, width)
                } else {
                    crate::Extent::new(width, height)
                };
                Some(ReceivedSurface {
                    connection: surface.connection,
                    surface_id: surface.surface_id,
                    target_window: surface.target_window,
                    image_extent,
                    quarter_turns: surface.quarter_turns,
                    flip_y: surface.flip_y,
                    frame,
                })
            })
            .collect()
    }

    pub(crate) fn has_pending_frame(&self) -> bool {
        self.surfaces.iter().any(|surface| surface.latest.is_some())
    }

    pub(crate) fn disconnect_stream(&mut self, connection: u64) -> Result<(), Box<dyn Error>> {
        if let Some(index) = self
            .surfaces
            .iter()
            .position(|surface| surface.connection == connection)
        {
            let surface = self.surfaces.remove(index);
            surface
                .imported
                .signal_release(self.receiver.queue(), RELEASE_ALL_SYNC_VALUE)?;
            self.wait_idle()?;
            self.refresh_surface_descriptors()?;
        }
        Ok(())
    }

    fn release_imported_streams(&mut self) -> windows::core::Result<()> {
        for surface in &self.surfaces {
            surface
                .imported
                .signal_release(self.receiver.queue(), RELEASE_ALL_SYNC_VALUE)?;
        }
        self.wait_idle()?;
        self.surfaces.clear();
        Ok(())
    }

    fn refresh_surface_descriptors(&mut self) -> windows::core::Result<()> {
        for (index, surface) in self.surfaces.iter_mut().enumerate() {
            surface.descriptor = FIRST_CONTENT_DESCRIPTOR + index as u32;
            unsafe {
                create_srv(
                    self.receiver.device(),
                    &self.srv_heap,
                    self.srv_stride,
                    surface.descriptor,
                    &surface.cache,
                )
            };
        }
        Ok(())
    }

    pub(crate) fn resize(&mut self, window: &impl RenderWindow) -> Result<bool, Box<dyn Error>> {
        let size = window.render_extent();
        if size.width == 0 || size.height == 0 {
            return Ok(false);
        }
        self.wait_idle()?;
        self.backbuffers.clear();
        unsafe {
            self.swapchain.ResizeBuffers(
                FRAME_COUNT as u32,
                size.width,
                size.height,
                SWAPCHAIN_FORMAT,
                self.swapchain_flags,
            )?;
            self.backbuffers = create_backbuffers(
                self.receiver.device(),
                &self.swapchain,
                &self.rtv_heap,
                self.rtv_stride,
            )?;
        }
        self.frame_permit = false;
        self.extent = (size.width, size.height);
        Ok(true)
    }

    pub(crate) fn draw(
        &mut self,
        output: &egui::FullOutput,
        primitives: &[ClippedPrimitive],
        plan: &CompositingPlan,
    ) -> Result<bool, Box<dyn Error>> {
        self.draw_with_webrtc(output, primitives, plan, None)
    }

    pub fn draw_with_webrtc(
        &mut self,
        output: &egui::FullOutput,
        primitives: &[ClippedPrimitive],
        plan: &CompositingPlan,
        mut webrtc: Option<&mut WebRtcCompositor>,
    ) -> Result<bool, Box<dyn Error>> {
        if webrtc
            .as_deref()
            .is_some_and(|webrtc| !webrtc.d3d12_matches(self.adapter_luid()))
        {
            return Err("WebRTC compositor belongs to a different D3D12 adapter".into());
        }
        let frame_index = unsafe { self.swapchain.GetCurrentBackBufferIndex() } as usize;
        self.wait_frame(frame_index)?;
        self.frame_garbage[frame_index].clear();
        // The fence above is what makes reusing these bytes safe: the GPU has
        // finished every draw that read from this frame's arena.
        self.frame_uploads[frame_index].reset();
        self.free_descriptors
            .append(&mut self.frame_descriptors[frame_index]);
        let allocator = &self.allocators[frame_index];
        unsafe {
            allocator.Reset()?;
            self.command_list
                .Reset(allocator, Some(&self.egui_pipeline))?;
        }

        self.upload_texture_deltas(frame_index, &output.textures_delta.set)?;
        self.upload_chrome(frame_index)?;

        let mut release_values = Vec::new();
        let mut video_captures = Vec::new();
        // Only publish video when every previously published frame has
        // resolved its completion value, and never more per draw than the
        // encoder channel can buffer. Out-of-order completion signals would
        // rewind the fence (dropped frames host-signal immediately while an
        // in-flight frame signals its older value later), and a rewound
        // fence leaves the compositor queue waiting forever.
        let video_publish_ready = webrtc.as_deref().is_some_and(WebRtcCompositor::d3d12_ready);
        let mut video_publish_budget = if video_publish_ready {
            webrtc.as_deref().map_or(0, WebRtcCompositor::available)
        } else {
            0
        };
        // The oldest guest frame this present carries. A pass composing several
        // streams is only as fresh as the stalest one on it, so latency is
        // measured against that rather than the most recent.
        let mut oldest_capture_qpc = None::<u64>;
        for index in 0..self.surfaces.len() {
            let mut surface = self.surfaces.remove(index);
            let mut captured = None;
            let pending = surface.latest.and_then(|frame| {
                surface
                    .imported
                    .resource(frame.slot)
                    .map(|source| (frame, source.clone(), surface.imported.uses_keyed_mutex()))
            });
            if let Some((frame, source, keyed_mutex)) = pending {
                if let Some(webrtc) = webrtc.as_deref() {
                    webrtc.wait_d3d12_stream(self.receiver.queue(), surface.connection)?;
                }
                if keyed_mutex {
                    let acquired = surface
                        .imported
                        .acquire_slot(frame.slot, KEYED_MUTEX_TIMEOUT_MS)
                        .is_ok();
                    if acquired {
                        let copied = self.copy_to_cache_sync(
                            &source,
                            &surface.cache,
                            &mut surface.cache_state,
                        );
                        let _ = surface.imported.release_slot(frame.slot);
                        copied?;
                        surface.display_frame = Some(frame);
                        surface.latest = None;
                        captured = Some(frame);
                    }
                } else {
                    surface
                        .imported
                        .wait_on_queue(self.receiver.queue(), frame.sync_value)?;
                    unsafe {
                        transition(
                            &self.command_list,
                            &source,
                            D3D12_RESOURCE_STATE_COMMON,
                            D3D12_RESOURCE_STATE_COPY_SOURCE,
                        );
                        transition(
                            &self.command_list,
                            &surface.cache,
                            surface.cache_state,
                            D3D12_RESOURCE_STATE_COPY_DEST,
                        );
                        self.command_list.CopyResource(&surface.cache, &source);
                        transition(
                            &self.command_list,
                            &source,
                            D3D12_RESOURCE_STATE_COPY_SOURCE,
                            D3D12_RESOURCE_STATE_COMMON,
                        );
                        transition(
                            &self.command_list,
                            &surface.cache,
                            D3D12_RESOURCE_STATE_COPY_DEST,
                            D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                        );
                    }
                    surface.cache_state = D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE;
                    surface.display_frame = Some(frame);
                    surface.latest = None;
                    captured = Some(frame);
                    release_values.push((surface.connection, frame.sync_value));
                }
            }
            if video_publish_budget > 0
                && captured.is_some()
                && webrtc
                    .as_deref()
                    .is_some_and(|webrtc| webrtc.accepts_d3d12_stream(surface.connection))
            {
                video_publish_budget -= 1;
                video_captures.push((
                    surface.connection,
                    surface.cache.clone(),
                    surface.imported.width,
                    surface.imported.height,
                    DXGI_FORMAT(surface.imported.format as i32),
                ));
            }
            if let Some(frame) = captured {
                oldest_capture_qpc = Some(
                    oldest_capture_qpc.map_or(frame.timestamp_qpc, |oldest: u64| {
                        oldest.min(frame.timestamp_qpc)
                    }),
                );
            }
            self.surfaces.insert(index, surface);
        }

        let backbuffer = self.backbuffers[frame_index].clone();
        let clear_color = plan.clear_color();
        unsafe {
            transition(
                &self.command_list,
                &backbuffer,
                D3D12_RESOURCE_STATE_PRESENT,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            );
            let rtv = rtv_handle(&self.rtv_heap, self.rtv_stride, frame_index as u32);
            self.command_list
                .OMSetRenderTargets(1, Some(&rtv), false, None);
            self.command_list
                .ClearRenderTargetView(rtv, &clear_color, None);
            self.command_list
                .SetGraphicsRootSignature(&self.root_signature);
            self.command_list
                .SetDescriptorHeaps(&[Some(self.srv_heap.clone())]);
            self.command_list
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        }

        if self
            .surfaces
            .iter()
            .any(|surface| surface.display_frame.is_some())
        {
            self.draw_content(frame_index, plan)?;
        }
        self.draw_egui(frame_index, output.pixels_per_point, primitives)?;

        unsafe {
            transition(
                &self.command_list,
                &backbuffer,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PRESENT,
            );
            self.command_list.Close()?;
            let list: ID3D12CommandList = self.command_list.cast()?;
            self.receiver.queue().ExecuteCommandLists(&[Some(list)]);
            for (connection, value) in release_values {
                if let Some(surface) = self
                    .surfaces
                    .iter()
                    .find(|surface| surface.connection == connection)
                {
                    surface
                        .imported
                        .signal_release(self.receiver.queue(), value)?;
                }
            }
            self.fence_value += 1;
            self.receiver
                .queue()
                .Signal(&self.fence, self.fence_value)?;
            self.frame_values[frame_index] = self.fence_value;
            for (connection, resource, width, height, format) in video_captures {
                if let Some(webrtc) = webrtc.as_deref_mut() {
                    webrtc.present_d3d12(
                        connection,
                        self.receiver.device().clone(),
                        resource,
                        D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
                        format,
                        width,
                        height,
                        self.fence.clone(),
                        self.fence_value,
                    );
                }
            }
            self.swapchain
                .Present(self.present_interval, DXGI_PRESENT(0))
                .ok()?;
            self.frame_permit = false;
            // Only presents that actually carried a new guest frame are worth
            // attributing; a pass that redrew unchanged content would report
            // the age of a frame the display had already shown.
            if let Some(captured_qpc) = oldest_capture_qpc {
                self.display_latency
                    .record(self.swapchain.GetLastPresentCount()?, captured_qpc);
            }
        }
        for texture_id in &output.textures_delta.free {
            if let Some(texture) = self.textures.remove(texture_id) {
                self.frame_descriptors[frame_index].push(texture.descriptor);
                self.frame_garbage[frame_index].push(texture.resource);
            }
        }
        Ok(true)
    }

    /// Composites a plan with no interface drawn over it.
    ///
    /// Hosts that paint their own chrome contribute no egui pass, so this
    /// hands the compositor an empty one rather than manufacturing a real
    /// context to satisfy the signature. Building a fresh `egui::Context` per
    /// frame rasterised the entire default font atlas and then re-uploaded it
    /// as a texture delta on every present, for a UI whose contents were
    /// always empty.
    pub fn draw_game_with_webrtc(
        &mut self,
        plan: &CompositingPlan,
        webrtc: Option<&mut WebRtcCompositor>,
    ) -> Result<bool, Box<dyn Error>> {
        self.draw_with_webrtc(&empty_interface(), &[], plan, webrtc)
    }

    fn draw_content(
        &mut self,
        frame_index: usize,
        plan: &CompositingPlan,
    ) -> Result<(), Box<dyn Error>> {
        // Painter order follows the plan, not the order streams happened to be
        // imported in, so a shell that stacks surfaces gets the z-order it
        // asked for. Chrome is interleaved rather than drawn first, because a
        // stream can have chrome both below and above it.
        let corners = |rect: snowglass_input::mouse::layout::Rect| {
            let (x, y, w, h) = (rect.x, rect.y, rect.width as i32, rect.height as i32);
            (
                x as f32 / self.extent.0 as f32 * 2.0 - 1.0,
                1.0 - y as f32 / self.extent.1 as f32 * 2.0,
                (x + w) as f32 / self.extent.0 as f32 * 2.0 - 1.0,
                1.0 - (y + h) as f32 / self.extent.1 as f32 * 2.0,
            )
        };
        let draws = plan
            .layers()
            .iter()
            .filter_map(|layer| match layer {
                PlanLayer::Surface(placement) => {
                    let surface = self.surfaces.iter().find(|surface| {
                        surface.connection == placement.connection
                            && surface.surface_id == placement.surface_id
                    })?;
                    let frame = surface.display_frame?;
                    let (crop_w, crop_h) = crate::fit_inside(
                        frame.render_width.max(1),
                        frame.render_height.max(1),
                        surface.imported.width,
                        surface.imported.height,
                    );
                    let (l, t, r, b) = corners(placement.rectangle);
                    let u = crop_w as f32 / surface.imported.width as f32;
                    let v = crop_h as f32 / surface.imported.height as f32;
                    Some((
                        false,
                        surface.descriptor,
                        quad(
                            l,
                            t,
                            r,
                            b,
                            u,
                            v,
                            surface.flip_y,
                            surface.quarter_turns,
                            [255; 4],
                        ),
                    ))
                }
                PlanLayer::Chrome { id, rectangle } => {
                    let layer = self.chrome.get(id)?;
                    let (l, t, r, b) = corners(*rectangle);
                    // Chrome is mostly transparent by design — a band covering
                    // only a bezel is empty everywhere else — so it composites
                    // rather than overwriting what is already there.
                    Some((
                        true,
                        layer.descriptor,
                        quad(l, t, r, b, 1.0, 1.0, false, 0, [255; 4]),
                    ))
                }
            })
            .collect::<Vec<_>>();
        let mut blending = None;
        for (blend, descriptor, vertices) in draws {
            if blending != Some(blend) {
                blending = Some(blend);
                // SAFETY: the command list is open for this frame.
                unsafe {
                    self.command_list.SetPipelineState(if blend {
                        &self.egui_pipeline
                    } else {
                        &self.content_pipeline
                    });
                }
            }
            self.draw_vertices(frame_index, descriptor, &vertices, None)?;
        }
        Ok(())
    }

    fn draw_egui(
        &mut self,
        frame_index: usize,
        ppp: f32,
        primitives: &[ClippedPrimitive],
    ) -> Result<(), Box<dyn Error>> {
        unsafe {
            self.command_list.SetPipelineState(&self.egui_pipeline);
        }
        for primitive in primitives {
            let Primitive::Mesh(mesh) = &primitive.primitive else {
                continue;
            };
            let Some(texture) = self.textures.get(&mesh.texture_id) else {
                continue;
            };
            let vertices = mesh
                .vertices
                .iter()
                .map(|vertex| GpuVertex {
                    position: [
                        vertex.pos.x * ppp / self.extent.0 as f32 * 2.0 - 1.0,
                        1.0 - vertex.pos.y * ppp / self.extent.1 as f32 * 2.0,
                    ],
                    uv: [vertex.uv.x, vertex.uv.y],
                    color: vertex.color.to_array(),
                })
                .collect::<Vec<_>>();
            let clip = primitive.clip_rect;
            let rect = RECT {
                left: (clip.min.x * ppp).floor().max(0.0) as i32,
                top: (clip.min.y * ppp).floor().max(0.0) as i32,
                right: (clip.max.x * ppp).ceil().min(self.extent.0 as f32) as i32,
                bottom: (clip.max.y * ppp).ceil().min(self.extent.1 as f32) as i32,
            };
            self.draw_indexed(
                frame_index,
                texture.descriptor,
                &vertices,
                &mesh.indices,
                rect,
            )?;
        }
        Ok(())
    }

    fn draw_vertices(
        &mut self,
        frame_index: usize,
        descriptor: u32,
        vertices: &[GpuVertex],
        scissor: Option<RECT>,
    ) -> Result<(), Box<dyn Error>> {
        let indices = [0u32, 1, 2, 3, 4, 5];
        self.draw_indexed(
            frame_index,
            descriptor,
            vertices,
            &indices,
            scissor.unwrap_or(RECT {
                left: 0,
                top: 0,
                right: self.extent.0 as i32,
                bottom: self.extent.1 as i32,
            }),
        )
    }

    fn draw_indexed(
        &mut self,
        frame_index: usize,
        descriptor: u32,
        vertices: &[GpuVertex],
        indices: &[u32],
        scissor: RECT,
    ) -> Result<(), Box<dyn Error>> {
        let arena = &mut self.frame_uploads[frame_index];
        let garbage = &mut self.frame_garbage[frame_index];
        let device = self.receiver.device();
        let vbv = D3D12_VERTEX_BUFFER_VIEW {
            BufferLocation: arena.write(device, as_bytes(vertices), garbage)?,
            SizeInBytes: size_of_val(vertices) as u32,
            StrideInBytes: size_of::<GpuVertex>() as u32,
        };
        let ibv = D3D12_INDEX_BUFFER_VIEW {
            BufferLocation: arena.write(device, as_bytes(indices), garbage)?,
            SizeInBytes: size_of_val(indices) as u32,
            Format: DXGI_FORMAT_R32_UINT,
        };
        unsafe {
            self.command_list.RSSetViewports(&[D3D12_VIEWPORT {
                Width: self.extent.0 as f32,
                Height: self.extent.1 as f32,
                MaxDepth: 1.0,
                ..Default::default()
            }]);
            self.command_list.RSSetScissorRects(&[scissor]);
            self.command_list.SetGraphicsRootDescriptorTable(
                0,
                gpu_handle(&self.srv_heap, self.srv_stride, descriptor),
            );
            self.command_list.IASetVertexBuffers(0, Some(&[vbv]));
            self.command_list.IASetIndexBuffer(Some(&ibv));
            self.command_list
                .DrawIndexedInstanced(indices.len() as u32, 1, 0, 0, 0);
        }
        Ok(())
    }

    /// Hands the compositor a texture to draw wherever a plan places
    /// [`PlanLayer::Chrome`] with this `id`.
    ///
    /// `handle` is a Win32 shared handle to a premultiplied RGBA texture on
    /// this adapter, which the compositor opens on its own device. The pixels
    /// never leave the GPU: the shell rasterises them there and this samples
    /// them there.
    ///
    /// The caller must have finished writing the texture before calling, since
    /// nothing here waits on the device that produced it.
    pub fn set_chrome_layer(&mut self, id: u64, handle: isize, width: u32, height: u32) {
        if width == 0 || height == 0 || handle == 0 {
            return;
        }
        // Reopening the same handle would allocate a second view of the same
        // pixels for nothing; the shell repaints in place.
        if self
            .chrome
            .get(&id)
            .is_some_and(|held| held.handle == handle)
        {
            return;
        }
        self.chrome_opens.retain(|(existing, _)| *existing != id);
        self.chrome_removals.retain(|existing| *existing != id);
        self.chrome_opens.push((id, handle));
    }

    /// Releases a chrome layer the shell has stopped drawing.
    pub fn drop_chrome_layer(&mut self, id: u64) {
        self.chrome_opens.retain(|(existing, _)| *existing != id);
        if self.chrome.contains_key(&id) && !self.chrome_removals.contains(&id) {
            self.chrome_removals.push(id);
        }
    }

    /// Whether anything is holding chrome, so a shell can tell an empty layer
    /// from one it has not supplied yet.
    pub fn has_chrome(&self) -> bool {
        !self.chrome.is_empty() || !self.chrome_opens.is_empty()
    }

    fn upload_chrome(&mut self, frame_index: usize) -> Result<(), Box<dyn Error>> {
        for id in std::mem::take(&mut self.chrome_removals) {
            if let Some(layer) = self.chrome.remove(&id) {
                self.frame_descriptors[frame_index].push(layer.descriptor);
                self.frame_garbage[frame_index].push(layer.resource);
            }
        }
        for (id, handle) in std::mem::take(&mut self.chrome_opens) {
            // Never overwrite a shader-visible descriptor still referenced by
            // another in-flight frame. Its old slot is recycled with that frame.
            let descriptor = self
                .free_descriptors
                .pop()
                .ok_or("descriptor heap exhausted opening chrome")?;
            // SAFETY: the handle names a texture the shell created on this
            // adapter and keeps alive; opening it yields our own reference.
            let mut opened: Option<ID3D12Resource> = None;
            unsafe {
                self.receiver
                    .device()
                    .OpenSharedHandle(HANDLE(handle as *mut c_void), &mut opened)?;
            }
            let resource = opened.ok_or("opening a shared chrome texture returned nothing")?;
            // SAFETY: the descriptor slot is free and the resource outlives it.
            unsafe {
                create_srv(
                    self.receiver.device(),
                    &self.srv_heap,
                    self.srv_stride,
                    descriptor,
                    &resource,
                )
            };
            if let Some(old) = self.chrome.insert(
                id,
                ChromeLayer {
                    resource,
                    descriptor,
                    handle,
                },
            ) {
                self.frame_descriptors[frame_index].push(old.descriptor);
                self.frame_garbage[frame_index].push(old.resource);
            }
        }
        Ok(())
    }

    fn upload_texture_deltas(
        &mut self,
        frame_index: usize,
        deltas: &[(TextureId, egui::epaint::ImageDelta)],
    ) -> Result<(), Box<dyn Error>> {
        for (id, delta) in deltas {
            let patch_size = delta.image.size();
            let patch = match &delta.image {
                egui::ImageData::Color(image) => image
                    .pixels
                    .iter()
                    .flat_map(|p| p.to_array())
                    .collect::<Vec<_>>(),
                egui::ImageData::Font(image) => image
                    .srgba_pixels(None)
                    .flat_map(|p| p.to_array())
                    .collect::<Vec<_>>(),
            };
            let (size, mut pixels) = if let Some(pos) = delta.pos {
                let old = self.textures.get(id);
                // A backend switch can happen after egui emitted its initial full atlas,
                // leaving the new renderer's first delta as a partial update. Grow an
                // initially transparent retained image in that case (and whenever the
                // atlas expands).
                let size = [
                    old.map_or(0, |value| value.size[0])
                        .max(pos[0] + patch_size[0]),
                    old.map_or(0, |value| value.size[1])
                        .max(pos[1] + patch_size[1]),
                ];
                let mut pixels = vec![0; size[0] * size[1] * 4];
                if let Some(old) = old {
                    for row in 0..old.size[1] {
                        let source = row * old.size[0] * 4;
                        let destination = row * size[0] * 4;
                        pixels[destination..destination + old.size[0] * 4]
                            .copy_from_slice(&old.pixels[source..source + old.size[0] * 4]);
                    }
                }
                for row in 0..patch_size[1] {
                    let dst = ((pos[1] + row) * size[0] + pos[0]) * 4;
                    let src = row * patch_size[0] * 4;
                    pixels[dst..dst + patch_size[0] * 4]
                        .copy_from_slice(&patch[src..src + patch_size[0] * 4]);
                }
                (size, pixels)
            } else {
                (patch_size, patch)
            };
            // Never overwrite a shader-visible descriptor still referenced by another
            // in-flight frame. Its old slot is recycled with that frame's fence.
            let descriptor = self
                .free_descriptors
                .pop()
                .ok_or("egui descriptor heap exhausted")?;
            let (resource, upload) = unsafe {
                upload_texture(self.receiver.device(), &self.command_list, size, &pixels)?
            };
            unsafe {
                create_srv(
                    self.receiver.device(),
                    &self.srv_heap,
                    self.srv_stride,
                    descriptor,
                    &resource,
                )
            };
            if let Some(old) = self.textures.insert(
                *id,
                EguiTexture {
                    resource,
                    pixels: std::mem::take(&mut pixels),
                    size,
                    descriptor,
                },
            ) {
                self.frame_descriptors[frame_index].push(old.descriptor);
                self.frame_garbage[frame_index].push(old.resource);
            }
            self.frame_garbage[frame_index].push(upload);
        }
        Ok(())
    }

    /// Copies `source` into `cache` and blocks until the GPU finishes. Used
    /// for keyed-mutex streams, where the mutex must not be released before
    /// every read of the slot has completed.
    fn copy_to_cache_sync(
        &mut self,
        source: &ID3D12Resource,
        cache: &ID3D12Resource,
        cache_state: &mut D3D12_RESOURCE_STATES,
    ) -> Result<(), Box<dyn Error>> {
        let (allocator, list) = self
            .mutex_copy
            .as_ref()
            .ok_or("keyed-mutex stream has no copy command list")?
            .clone();
        unsafe {
            allocator.Reset()?;
            list.Reset(&allocator, None)?;
            transition(
                &list,
                source,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            );
            transition(&list, cache, *cache_state, D3D12_RESOURCE_STATE_COPY_DEST);
            list.CopyResource(cache, source);
            transition(
                &list,
                source,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                D3D12_RESOURCE_STATE_COMMON,
            );
            transition(
                &list,
                cache,
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
            );
            list.Close()?;
            let command: ID3D12CommandList = list.cast()?;
            self.receiver.queue().ExecuteCommandLists(&[Some(command)]);
        }
        *cache_state = D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE;
        self.wait_idle()?;
        Ok(())
    }

    fn wait_frame(&self, index: usize) -> windows::core::Result<()> {
        self.wait_value(self.frame_values[index])
    }
    fn wait_idle(&mut self) -> windows::core::Result<()> {
        unsafe {
            self.fence_value += 1;
            self.receiver
                .queue()
                .Signal(&self.fence, self.fence_value)?;
        }
        self.wait_value(self.fence_value)
    }
    fn wait_value(&self, value: u64) -> windows::core::Result<()> {
        unsafe {
            if value != 0 && self.fence.GetCompletedValue() < value {
                self.fence.SetEventOnCompletion(value, self.fence_event)?;
                WaitForSingleObject(self.fence_event, INFINITE);
            }
        }
        Ok(())
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

#[expect(clippy::too_many_arguments)]
fn quad(
    l: f32,
    t: f32,
    r: f32,
    b: f32,
    u: f32,
    v: f32,
    flip_y: bool,
    quarter_turns: u8,
    color: [u8; 4],
) -> [GpuVertex; 6] {
    let p = |position, uv| GpuVertex {
        position,
        uv,
        color,
    };
    let (top_v, bottom_v) = if flip_y { (v, 0.0) } else { (0.0, v) };
    let corners = [[0.0, top_v], [u, top_v], [u, bottom_v], [0.0, bottom_v]];
    let uv = |corner: usize| corners[(corner + quarter_turns as usize) % 4];
    [
        p([l, t], uv(0)),
        p([r, t], uv(1)),
        p([r, b], uv(2)),
        p([l, t], uv(0)),
        p([r, b], uv(2)),
        p([l, b], uv(3)),
    ]
}

unsafe fn create_backbuffers(
    device: &ID3D12Device,
    swapchain: &IDXGISwapChain3,
    heap: &ID3D12DescriptorHeap,
    stride: u32,
) -> windows::core::Result<Vec<ID3D12Resource>> {
    let mut result = Vec::with_capacity(FRAME_COUNT);
    for index in 0..FRAME_COUNT {
        let resource = unsafe { swapchain.GetBuffer::<ID3D12Resource>(index as u32)? };
        unsafe {
            device.CreateRenderTargetView(&resource, None, rtv_handle(heap, stride, index as u32))
        };
        result.push(resource);
    }
    Ok(result)
}

unsafe fn create_texture(
    device: &ID3D12Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    state: D3D12_RESOURCE_STATES,
) -> windows::core::Result<ID3D12Resource> {
    let mut resource: Option<ID3D12Resource> = None;
    unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: u64::from(width),
                Height: height,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                Flags: D3D12_RESOURCE_FLAG_NONE,
                Alignment: 0,
            },
            state,
            None,
            &mut resource,
        )?;
    }
    resource
        .ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))
}

unsafe fn create_srv(
    device: &ID3D12Device,
    heap: &ID3D12DescriptorHeap,
    stride: u32,
    index: u32,
    resource: &ID3D12Resource,
) {
    let desc = unsafe { resource.GetDesc() };
    unsafe {
        device.CreateShaderResourceView(
            resource,
            Some(&D3D12_SHADER_RESOURCE_VIEW_DESC {
                Format: desc.Format,
                ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
                Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
                Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D12_TEX2D_SRV {
                        MipLevels: 1,
                        ..Default::default()
                    },
                },
            }),
            cpu_srv_handle(heap, stride, index),
        )
    };
}

unsafe fn create_upload_buffer(
    device: &ID3D12Device,
    size: u64,
) -> windows::core::Result<ID3D12Resource> {
    let mut resource: Option<ID3D12Resource> = None;
    unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_UPLOAD,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: size.max(1),
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                ..Default::default()
            },
            D3D12_RESOURCE_STATE_GENERIC_READ,
            None,
            &mut resource,
        )?;
    }
    resource
        .ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))
}

unsafe fn upload_texture(
    device: &ID3D12Device,
    list: &ID3D12GraphicsCommandList,
    size: [usize; 2],
    pixels: &[u8],
) -> windows::core::Result<(ID3D12Resource, ID3D12Resource)> {
    let texture = unsafe {
        create_texture(
            device,
            size[0] as u32,
            size[1] as u32,
            DXGI_FORMAT_R8G8B8A8_UNORM,
            D3D12_RESOURCE_STATE_COPY_DEST,
        )?
    };
    let pitch = (size[0] * 4).div_ceil(D3D12_TEXTURE_DATA_PITCH_ALIGNMENT as usize)
        * D3D12_TEXTURE_DATA_PITCH_ALIGNMENT as usize;
    let upload = unsafe { create_upload_buffer(device, (pitch * size[1]) as u64)? };
    let mut mapped = ptr::null_mut();
    unsafe {
        upload.Map(0, None, Some(&mut mapped))?;
    }
    for row in 0..size[1] {
        unsafe {
            ptr::copy_nonoverlapping(
                pixels.as_ptr().add(row * size[0] * 4),
                mapped.cast::<u8>().add(row * pitch),
                size[0] * 4,
            );
        }
    }
    unsafe {
        upload.Unmap(0, None);
    }
    let mut dst = D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(texture.clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            SubresourceIndex: 0,
        },
    };
    let mut src = D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(upload.clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                Offset: 0,
                Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    Width: size[0] as u32,
                    Height: size[1] as u32,
                    Depth: 1,
                    RowPitch: pitch as u32,
                },
            },
        },
    };
    unsafe {
        list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
        transition(
            list,
            &texture,
            D3D12_RESOURCE_STATE_COPY_DEST,
            D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
        );
        ManuallyDrop::drop(&mut dst.pResource);
        ManuallyDrop::drop(&mut src.pResource);
    }
    Ok((texture, upload))
}

unsafe fn transition(
    list: &ID3D12GraphicsCommandList,
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) {
    if before == after {
        return;
    }
    let mut barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    };
    unsafe {
        list.ResourceBarrier(std::slice::from_ref(&barrier));
        let transition = &mut *barrier.Anonymous.Transition;
        ManuallyDrop::drop(&mut transition.pResource);
    }
}

fn create_root_signature(device: &ID3D12Device) -> windows::core::Result<ID3D12RootSignature> {
    unsafe {
        let range = D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
            NumDescriptors: 1,
            BaseShaderRegister: 0,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: 0,
        };
        let table = D3D12_ROOT_DESCRIPTOR_TABLE {
            NumDescriptorRanges: 1,
            pDescriptorRanges: &range,
        };
        let parameter = D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: table,
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
        };
        let sampler = D3D12_STATIC_SAMPLER_DESC {
            Filter: D3D12_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            MaxLOD: f32::MAX,
            ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
            ..Default::default()
        };
        let desc = D3D12_ROOT_SIGNATURE_DESC {
            NumParameters: 1,
            pParameters: &parameter,
            NumStaticSamplers: 1,
            pStaticSamplers: &sampler,
            Flags: D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT,
        };
        let mut blob = None;
        D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None)?;
        let blob = blob.ok_or_else(|| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER)
        })?;
        device.CreateRootSignature(
            0,
            std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize()),
        )
    }
}

fn create_pipeline(
    device: &ID3D12Device,
    root: &ID3D12RootSignature,
    alpha_blend: bool,
) -> windows::core::Result<ID3D12PipelineState> {
    const SHADER: &[u8] = br#"
struct VIn { float2 pos : POSITION; float2 uv : TEXCOORD0; float4 color : COLOR0; };
struct VOut { float4 pos : SV_POSITION; float2 uv : TEXCOORD0; float4 color : COLOR0; };
VOut vs_main(VIn v) { VOut o; o.pos=float4(v.pos,0,1); o.uv=v.uv; o.color=v.color; return o; }
Texture2D tex : register(t0); SamplerState samp : register(s0);
float4 ps_main(VOut i) : SV_TARGET { return tex.Sample(samp, i.uv) * i.color; }
"#;
    unsafe {
        let vs = compile(SHADER, b"vs_main\0", b"vs_5_0\0")?;
        let ps = compile(SHADER, b"ps_main\0", b"ps_5_0\0")?;
        let input = [
            D3D12_INPUT_ELEMENT_DESC {
                SemanticName: PCSTR(c"POSITION".as_ptr().cast()),
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                ..Default::default()
            },
            D3D12_INPUT_ELEMENT_DESC {
                SemanticName: PCSTR(c"TEXCOORD".as_ptr().cast()),
                Format: DXGI_FORMAT_R32G32_FLOAT,
                AlignedByteOffset: 8,
                InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                ..Default::default()
            },
            D3D12_INPUT_ELEMENT_DESC {
                SemanticName: PCSTR(c"COLOR".as_ptr().cast()),
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                AlignedByteOffset: 16,
                InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                ..Default::default()
            },
        ];
        let mut desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            pRootSignature: ManuallyDrop::new(Some(root.clone())),
            VS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: vs.GetBufferPointer(),
                BytecodeLength: vs.GetBufferSize(),
            },
            PS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: ps.GetBufferPointer(),
                BytecodeLength: ps.GetBufferSize(),
            },
            BlendState: D3D12_BLEND_DESC {
                RenderTarget: [D3D12_RENDER_TARGET_BLEND_DESC {
                    BlendEnable: alpha_blend.into(),
                    SrcBlend: D3D12_BLEND_ONE,
                    DestBlend: D3D12_BLEND_INV_SRC_ALPHA,
                    BlendOp: D3D12_BLEND_OP_ADD,
                    SrcBlendAlpha: D3D12_BLEND_ONE,
                    DestBlendAlpha: D3D12_BLEND_INV_SRC_ALPHA,
                    BlendOpAlpha: D3D12_BLEND_OP_ADD,
                    RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8,
                    ..Default::default()
                }; 8],
                ..Default::default()
            },
            SampleMask: u32::MAX,
            RasterizerState: D3D12_RASTERIZER_DESC {
                FillMode: D3D12_FILL_MODE_SOLID,
                CullMode: D3D12_CULL_MODE_NONE,
                DepthClipEnable: true.into(),
                ..Default::default()
            },
            DepthStencilState: D3D12_DEPTH_STENCIL_DESC::default(),
            InputLayout: D3D12_INPUT_LAYOUT_DESC {
                pInputElementDescs: input.as_ptr(),
                NumElements: input.len() as u32,
            },
            PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            NumRenderTargets: 1,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            ..Default::default()
        };
        desc.RTVFormats[0] = SWAPCHAIN_FORMAT;
        let result = device.CreateGraphicsPipelineState(&desc);
        ManuallyDrop::drop(&mut desc.pRootSignature);
        result
    }
}

unsafe fn compile(source: &[u8], entry: &[u8], target: &[u8]) -> windows::core::Result<ID3DBlob> {
    let mut blob = None;
    unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            None,
            None,
            None,
            PCSTR(entry.as_ptr()),
            PCSTR(target.as_ptr()),
            0,
            0,
            &mut blob,
            None,
        )?;
    }
    blob.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_POINTER))
}

fn rtv_handle(heap: &ID3D12DescriptorHeap, stride: u32, index: u32) -> D3D12_CPU_DESCRIPTOR_HANDLE {
    let mut h = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
    h.ptr += (stride * index) as usize;
    h
}
fn cpu_srv_handle(
    heap: &ID3D12DescriptorHeap,
    stride: u32,
    index: u32,
) -> D3D12_CPU_DESCRIPTOR_HANDLE {
    let mut h = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
    h.ptr += (stride * index) as usize;
    h
}
fn gpu_handle(heap: &ID3D12DescriptorHeap, stride: u32, index: u32) -> D3D12_GPU_DESCRIPTOR_HANDLE {
    let mut h = unsafe { heap.GetGPUDescriptorHandleForHeapStart() };
    h.ptr += u64::from(stride * index);
    h
}
/// QPC ticks per second, the clock producers stamp captures on and DXGI
/// reports vblanks on.
fn counter_frequency() -> f64 {
    let mut frequency = 0i64;
    // SAFETY: the out-parameter is a live local. A failure or a zero would
    // divide, so the result is floored either way.
    let queried = unsafe {
        windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut frequency)
    };
    if queried == 0 {
        return 1.0;
    }
    (frequency as f64).max(1.0)
}

fn as_bytes<T>(items: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(items.as_ptr().cast(), size_of_val(items)) }
}

#[cfg(test)]
mod tests {
    use super::{DisplayLatency, UploadArena, quad};
    use std::time::{Duration, Instant};

    /// One tick of the QPC clock per millisecond, so latencies read directly.
    const TEST_FREQUENCY: f64 = 1000.0;

    /// A vblank timestamp only dates the present DXGI names alongside it.
    /// Attributing it to the wrong present would silently report a latency the
    /// display never had.
    /// Arms the sampling window and returns the instant one window later, so a
    /// test's first real `sample` is the one that reports.
    fn armed(latency: &mut DisplayLatency) -> Instant {
        let start = Instant::now();
        assert_eq!(
            latency.sample(start),
            None,
            "an empty window reports nothing"
        );
        start + crate::debug::STATISTICS_SAMPLE_INTERVAL
    }

    #[test]
    fn a_vblank_dates_only_the_present_it_named() {
        let mut latency = DisplayLatency::default();
        let due = armed(&mut latency);
        latency.record(10, 1_000);
        latency.record(11, 1_016);
        latency.record(12, 1_032);

        // Present 11 displayed at 1032: 16 ticks after it was captured.
        latency.observe(11, 1_032, TEST_FREQUENCY);
        let report = latency.sample(due).expect("a confirmed present reports");
        assert_eq!(report.confirmed, 1);
        assert!((report.average_ms - 16.0).abs() < f64::EPSILON);

        // Present 10 went out on an earlier, never-timestamped vblank. It must
        // be discarded rather than dated with 11's, and 12 must survive to be
        // matched on its own vblank.
        latency.observe(12, 1_048, TEST_FREQUENCY);
        let report = latency
            .sample(due + crate::debug::STATISTICS_SAMPLE_INTERVAL)
            .expect("a confirmed present reports");
        assert_eq!(report.confirmed, 1);
        assert!((report.average_ms - 16.0).abs() < f64::EPSILON);
    }

    /// Statistics stall and resume; an unrecorded present must not be
    /// attributed to whatever happens to be queued.
    #[test]
    fn an_unknown_present_reports_nothing() {
        let mut latency = DisplayLatency::default();
        let due = armed(&mut latency);
        latency.record(5, 1_000);
        // Never recorded, and newer than everything queued.
        latency.observe(9, 2_000, TEST_FREQUENCY);
        assert_eq!(
            latency.sample(due),
            None,
            "an unmatched vblank must not produce a sample"
        );
    }

    /// A vblank stamped before the capture it supposedly displayed means the
    /// clocks disagree; reporting a wrapped duration would be worse than
    /// reporting nothing.
    #[test]
    fn a_vblank_older_than_its_capture_is_discarded() {
        let mut latency = DisplayLatency::default();
        let due = armed(&mut latency);
        latency.record(1, 5_000);
        latency.observe(1, 4_000, TEST_FREQUENCY);
        assert_eq!(latency.sample(due), None);
    }

    /// Frame statistics can go unavailable for long stretches, and this is
    /// diagnostics: it must not accumulate presents without bound.
    #[test]
    fn unconfirmed_presents_stay_bounded() {
        let mut latency = DisplayLatency::default();
        for present in 0..(DisplayLatency::PENDING_LIMIT as u32 * 4) {
            latency.record(present, u64::from(present) * 16);
        }
        assert_eq!(latency.pending.len(), DisplayLatency::PENDING_LIMIT);
        // The newest survive, so statistics resuming still finds a match.
        assert_eq!(
            latency.pending.back().map(|frame| frame.present_count),
            Some(DisplayLatency::PENDING_LIMIT as u32 * 4 - 1)
        );
    }

    /// The window reports the previous result until a new one is complete, so
    /// the panel does not blank between samples.
    #[test]
    fn a_partial_window_keeps_the_last_report() {
        let mut latency = DisplayLatency::default();
        let start = Instant::now();
        assert_eq!(latency.sample(start), None);
        latency.record(1, 0);
        latency.observe(1, 20, TEST_FREQUENCY);
        let report = latency
            .sample(start + crate::debug::STATISTICS_SAMPLE_INTERVAL)
            .expect("the window elapsed");
        assert!((report.average_ms - 20.0).abs() < f64::EPSILON);
        assert_eq!(
            latency.sample(
                start + crate::debug::STATISTICS_SAMPLE_INTERVAL + Duration::from_millis(1)
            ),
            Some(report),
            "a window still filling must not blank the report"
        );
    }

    /// Suballocations must never overlap: two draw calls sharing a byte would
    /// silently corrupt one of their meshes rather than fail.
    #[test]
    fn suballocations_are_aligned_and_disjoint() {
        let capacity = UploadArena::INITIAL_CAPACITY;
        let first = UploadArena::placement(0, capacity, 20).expect("an empty arena fits 20 bytes");
        assert_eq!(first, 0);

        // 20 bytes is one `GpuVertex`, so the next placement has to round up
        // rather than start inside the bytes just written.
        let second = UploadArena::placement(first + 20, capacity, 24).expect("still fits");
        assert!(second >= first + 20, "second allocation overlaps the first");
        assert_eq!(second % UploadArena::ALIGNMENT, 0);
        assert_eq!(second, 32);
    }

    #[test]
    fn a_request_past_the_end_reports_no_placement() {
        assert_eq!(UploadArena::placement(0, 64, 65), None);
        assert_eq!(UploadArena::placement(60, 64, 8), None);
        // Exactly filling the arena is a fit, not an overflow.
        assert_eq!(UploadArena::placement(0, 64, 64), Some(0));
        // Rounding the offset up must not be allowed to wrap past the end.
        assert_eq!(UploadArena::placement(u64::MAX - 1, u64::MAX, 8), None);
    }

    /// Growth is what keeps a steady-state frame from reallocating, so it has
    /// to both double and always cover the request that forced it.
    #[test]
    fn growth_doubles_but_always_covers_the_request() {
        assert_eq!(
            UploadArena::grown_capacity(0, 16),
            UploadArena::INITIAL_CAPACITY
        );
        assert_eq!(
            UploadArena::grown_capacity(UploadArena::INITIAL_CAPACITY, 16),
            UploadArena::INITIAL_CAPACITY * 2
        );
        let huge = UploadArena::INITIAL_CAPACITY * 100;
        assert!(UploadArena::grown_capacity(UploadArena::INITIAL_CAPACITY, huge) >= huge);
        assert_eq!(UploadArena::grown_capacity(u64::MAX, 16), u64::MAX);
    }

    #[test]
    fn opengl_quad_reverses_texture_v() {
        let normal = quad(-1.0, 1.0, 1.0, -1.0, 0.75, 0.5, false, 0, [255; 4]);
        let flipped = quad(-1.0, 1.0, 1.0, -1.0, 0.75, 0.5, true, 0, [255; 4]);

        assert_eq!(normal[0].uv[1], 0.0);
        assert_eq!(normal[2].uv[1], 0.5);
        assert_eq!(flipped[0].uv[1], 0.5);
        assert_eq!(flipped[2].uv[1], 0.0);
    }

    #[test]
    fn quarter_turns_rotate_texture_corners_counterclockwise() {
        // One counterclockwise quarter turn shows the source's top-right
        // corner at the destination's top-left.
        let turned = quad(-1.0, 1.0, 1.0, -1.0, 0.75, 0.5, false, 1, [255; 4]);
        assert_eq!(turned[0].uv, [0.75, 0.0]);
        assert_eq!(turned[1].uv, [0.75, 0.5]);
        assert_eq!(turned[2].uv, [0.0, 0.5]);
        assert_eq!(turned[5].uv, [0.0, 0.0]);

        // A half turn mirrors both axes.
        let half = quad(-1.0, 1.0, 1.0, -1.0, 0.75, 0.5, false, 2, [255; 4]);
        assert_eq!(half[0].uv, [0.75, 0.5]);
        assert_eq!(half[2].uv, [0.0, 0.0]);

        // A flipped half turn cancels the vertical mirror.
        let flipped_half = quad(-1.0, 1.0, 1.0, -1.0, 0.75, 0.5, true, 2, [255; 4]);
        assert_eq!(flipped_half[0].uv, [0.75, 0.0]);
        assert_eq!(flipped_half[2].uv, [0.0, 0.5]);
    }
}
