//! Swapchain capture orchestration: per-target export streams, their viewer
//! transports, and the copies that feed them.
//!
//! The hook layer forwards raw swapchain lifecycle and presentation events;
//! everything about what to capture and how to move it lives here.

use crate::tracking;
use crate::{
    ExportImageRing, ExportImageRingDesc, ExportTimelineSemaphore, ExternalHandleMode,
    SetDeviceLoaderData, SwapchainCopyEngine,
};
use ash::{vk, vk::Handle};
use dashmap::DashMap;
use snowglass_hook::config;
use snowglass_ipc::{LocalSocket, Message, StreamReady};
use snowglass_platform_windows::{ProcessId, duplicate_handle_into_process, performance_counter};
use snowglass_protocol::{
    AdapterLuid, ConsumerApi, ExternalImageKind, ExternalSyncKind, FrameReady, MAX_IMAGE_SLOTS,
    PROTOCOL_VERSION, StreamId,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
};

/// Everything the capture needs from a hooked device: an owned dispatch and
/// the external-memory entry points the export transport is built from.
pub struct CaptureDevice {
    pub device: ash::Device,
    pub memory_properties: vk::PhysicalDeviceMemoryProperties,
    pub get_memory_win32_handle: vk::PFN_vkGetMemoryWin32HandleKHR,
    pub get_memory_win32_handle_properties: vk::PFN_vkGetMemoryWin32HandlePropertiesKHR,
    pub get_semaphore_win32_handle: vk::PFN_vkGetSemaphoreWin32HandleKHR,
    pub import_semaphore_win32_handle: vk::PFN_vkImportSemaphoreWin32HandleKHR,
    pub set_loader_data: Option<SetDeviceLoaderData>,
    pub supports_foreign_queue: bool,
    pub adapter_luid: AdapterLuid,
}

fn log(message: impl std::fmt::Display) {
    use std::io::Write;

    eprintln!("{message}");
    let Some(path) = config::layer_log_file() else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{message}");
    }
}

/// One exported stream: a ring, its synchronization, its viewer connection,
/// and the engine copying into it. Each tracked render target gets its own
/// stream so the viewer can show the screens as separate surfaces.
struct CaptureStream {
    extent: vk::Extent2D,
    ring: Option<ExportImageRing>,
    producer_timeline: Option<ExportTimelineSemaphore>,
    consumer_timeline: Option<ExportTimelineSemaphore>,
    copy_engine: Option<SwapchainCopyEngine>,
    ipc: Option<LocalSocket>,
    frame_number: u64,
    /// Presents this stream has existed for, successful or not. The
    /// reconnect cadence keys off this rather than `frame_number`: a stream
    /// without a viewer stalls once its ring fills, so completed frames stop
    /// advancing exactly when reconnecting matters most.
    presents: u64,
}

impl CaptureStream {
    fn from_transport(extent: vk::Extent2D, transport: Transport) -> Self {
        Self {
            extent,
            ring: transport.ring,
            producer_timeline: transport.producer_timeline,
            consumer_timeline: transport.consumer_timeline,
            copy_engine: None,
            ipc: transport.ipc,
            frame_number: 0,
            presents: 0,
        }
    }

    fn retire(&mut self) {
        if let Some(engine) = self.copy_engine.take() {
            engine.wait_idle();
        }
    }
}

struct SwapchainState {
    device: vk::Device,
    capture: Option<Arc<CaptureDevice>>,
    format: vk::Format,
    extent: vk::Extent2D,
    source_window: isize,
    images: Vec<vk::Image>,
    /// Stream 0 always exists (the whole-frame capture when nothing is
    /// tracked); additional streams appear per tracked render target.
    streams: Vec<CaptureStream>,
    /// Consecutive presents whose selection came up empty. A tracked stream
    /// layout survives short gaps (scene transitions destroy and recreate
    /// the screens) instead of churning back to the whole-frame stream.
    untracked_presents: u32,
    /// A proposed stream layout different from the current one, and how many
    /// consecutive presents proposed it. Rebuilding transports is expensive
    /// and churns viewer connections, so a new layout must persist before it
    /// is adopted.
    pending_layout: Vec<vk::Extent2D>,
    pending_presents: u32,
    /// The images captured by the last successful graph selection, for
    /// bridging frames whose presentation roots cannot be resolved.
    tracked_sources: Vec<vk::Image>,
    /// Global present count when this swapchain last presented. A window
    /// that stops presenting (a splash screen left alive but idle) must not
    /// keep holding viewer surfaces with a frozen stream.
    last_present: u64,
}

static SWAPCHAIN: LazyLock<DashMap<vk::SwapchainKHR, SwapchainState>> =
    LazyLock::new(Default::default);
static SURFACE_WINDOW: LazyLock<DashMap<vk::SurfaceKHR, isize>> = LazyLock::new(Default::default);
static SWAPCHAIN_IMAGE: LazyLock<DashMap<vk::Image, vk::SwapchainKHR>> =
    LazyLock::new(Default::default);
static SWAPCHAINS_CREATED: AtomicU64 = AtomicU64::new(0);
static PRESENTS_SEEN: AtomicU64 = AtomicU64::new(0);
static COPIES_SUBMITTED: AtomicU64 = AtomicU64::new(0);

/// Which swapchain streams tracked render targets. An application with
/// several windows resolves the same screens on every one of them; without a
/// single owner each window would publish redundant duplicate streams and
/// race the others for viewer surfaces. Ownership is process-wide and
/// sticky: the incumbent keeps it as long as it keeps resolving sources.
static TRACKED_OWNER: LazyLock<std::sync::Mutex<Option<(vk::SwapchainKHR, u64)>>> =
    LazyLock::new(Default::default);
const CLAIM_HORIZON: u64 = 240;

/// Presents (across all swapchains) an idle swapchain may hold streams for.
const IDLE_STREAM_HORIZON: u64 = 600;

fn claim_tracked_streaming(swapchain: vk::SwapchainKHR, now: u64) -> bool {
    let mut owner = TRACKED_OWNER
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    match *owner {
        Some((incumbent, refreshed))
            if incumbent != swapchain && now.saturating_sub(refreshed) <= CLAIM_HORIZON =>
        {
            false
        }
        _ => {
            *owner = Some((swapchain, now));
            true
        }
    }
}

fn release_tracked_streaming(swapchain: vk::SwapchainKHR) {
    let mut owner = TRACKED_OWNER
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if owner.is_some_and(|(incumbent, _)| incumbent == swapchain) {
        *owner = None;
    }
}

/// Retires the streams of swapchains that have stopped presenting, freeing
/// their viewer surfaces. Presenting again rebuilds them on demand.
fn retire_idle_streams(now: u64) {
    for mut entry in SWAPCHAIN.iter_mut() {
        if entry.last_present.saturating_add(IDLE_STREAM_HORIZON) >= now
            || entry.streams.is_empty()
        {
            continue;
        }
        log(format!(
            "snowglass: retiring {} stream(s) of an idle swapchain (window {:#x})",
            entry.streams.len(),
            entry.source_window
        ));
        for mut stream in entry.streams.drain(..) {
            stream.retire();
        }
    }
}

/// The window identity published for a stream. Extra streams get offset
/// identities so the viewer treats each tracked screen as its own surface
/// instead of replacing the window's previous stream; the offsets are odd,
/// which no real window handle can collide with.
fn stream_window(source_window: isize, index: usize) -> isize {
    if source_window == 0 || index == 0 {
        source_window
    } else {
        source_window + (index as isize * 2 - 1)
    }
}

struct ViewerConnection {
    socket: LocalSocket,
    process_id: u32,
    preferred_consumer: ConsumerApi,
}

fn begin_viewer_connection() -> Option<ViewerConnection> {
    let session = config::session().ok()?;
    let mut socket = match LocalSocket::connect(&session) {
        Ok(socket) => socket,
        Err(error) => {
            log(format!(
                "snowglass: viewer socket connection failed: {error}"
            ));
            return None;
        }
    };
    match socket.receive() {
        Ok(Message::Hello(hello)) if hello.protocol_version == PROTOCOL_VERSION => {
            Some(ViewerConnection {
                socket,
                process_id: hello.process_id,
                preferred_consumer: hello.preferred_consumer,
            })
        }
        Ok(Message::Hello(hello)) => {
            log(format!(
                "snowglass: viewer protocol {} is incompatible with {}",
                hello.protocol_version, PROTOCOL_VERSION
            ));
            None
        }
        Ok(_) => {
            log("snowglass: viewer did not begin with a Hello message");
            None
        }
        Err(error) => {
            log(format!("snowglass: viewer handshake failed: {error}"));
            None
        }
    }
}

fn publish_stream(
    mut connection: ViewerConnection,
    ring: &ExportImageRing,
    producer_timeline: &ExportTimelineSemaphore,
    consumer_timeline: Option<&ExportTimelineSemaphore>,
    adapter_luid: AdapterLuid,
    extent: vk::Extent2D,
    format: vk::Format,
    source_window: isize,
) -> Option<LocalSocket> {
    let viewer_pid = connection.process_id;
    if ring.slots().len() > MAX_IMAGE_SLOTS {
        log("snowglass: export ring exceeds protocol slot capacity");
        return None;
    }
    let mut handles = [0; MAX_IMAGE_SLOTS];
    for (destination, slot) in handles.iter_mut().zip(ring.slots()) {
        match duplicate_handle_into_process(slot.raw_handle(), ProcessId(viewer_pid)) {
            Ok(handle) => *destination = handle as i64,
            Err(error) => {
                log(format!(
                    "snowglass: image handle duplication failed: {error}"
                ));
                return None;
            }
        }
    }
    let d3d12 = ring.desc().handle_mode == ExternalHandleMode::D3d12;
    let stream_format = if d3d12 {
        match dxgi_format(format) {
            Some(format) => format,
            None => {
                log(format!(
                    "snowglass: Vulkan format {} has no D3D12 interop mapping",
                    format.as_raw()
                ));
                return None;
            }
        }
    } else {
        format.as_raw() as u32
    };
    let ready = StreamReady {
        producer_process_id: std::process::id(),
        adapter_luid,
        source_window: source_window as i64,
        width: extent.width,
        height: extent.height,
        format: stream_format,
        image_kind: if d3d12 {
            ExternalImageKind::VulkanD3d12Resource
        } else {
            ExternalImageKind::VulkanOpaqueWin32
        },
        sync_kind: if d3d12 {
            ExternalSyncKind::D3d12Fence
        } else {
            ExternalSyncKind::VulkanTimelineSemaphore
        },
        slot_count: ring.slots().len() as u32,
        image_handles: handles,
        timeline_semaphore_handle: match duplicate_handle_into_process(
            producer_timeline.raw_handle(),
            ProcessId(viewer_pid),
        ) {
            Ok(handle) => handle as i64,
            Err(error) => {
                log(format!(
                    "snowglass: timeline handle duplication failed: {error}"
                ));
                return None;
            }
        },
        consumer_semaphore_handle: match consumer_timeline {
            Some(timeline) => {
                match duplicate_handle_into_process(timeline.raw_handle(), ProcessId(viewer_pid)) {
                    Ok(handle) => handle as i64,
                    Err(error) => {
                        log(format!(
                            "snowglass: consumer fence duplication failed: {error}"
                        ));
                        return None;
                    }
                }
            }
            None => 0,
        },
    };
    if let Err(error) = connection.socket.send(Message::StreamReady(ready)) {
        log(format!("snowglass: StreamReady send failed: {error}"));
        return None;
    }
    // The viewer acknowledges only accepted streams; a rejection closes the
    // socket instead. Without waiting for the ack, a rejected stream looks
    // alive forever (a stalled ring stops sending the frames that would
    // surface the dead socket) and reconnect retries never trigger.
    match connection.socket.receive() {
        Ok(Message::StreamAccepted) => {}
        Ok(_) => {
            log("snowglass: viewer did not acknowledge the stream");
            return None;
        }
        Err(error) => {
            log(format!("snowglass: viewer declined the stream: {error}"));
            return None;
        }
    }
    log(format!(
        "snowglass: connected export ring to viewer process {viewer_pid}"
    ));
    Some(connection.socket)
}

fn dxgi_format(format: vk::Format) -> Option<u32> {
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB, DXGI_FORMAT_R8G8B8A8_UNORM,
        DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, DXGI_FORMAT_R10G10B10A2_UNORM,
        DXGI_FORMAT_R16G16B16A16_FLOAT,
    };
    Some(match format {
        vk::Format::R8G8B8A8_UNORM => DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32,
        vk::Format::R8G8B8A8_SRGB => DXGI_FORMAT_R8G8B8A8_UNORM_SRGB.0 as u32,
        vk::Format::B8G8R8A8_UNORM => DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
        vk::Format::B8G8R8A8_SRGB => DXGI_FORMAT_B8G8R8A8_UNORM_SRGB.0 as u32,
        vk::Format::A2B10G10R10_UNORM_PACK32 => DXGI_FORMAT_R10G10B10A2_UNORM.0 as u32,
        vk::Format::R16G16B16A16_SFLOAT => DXGI_FORMAT_R16G16B16A16_FLOAT.0 as u32,
        _ => return None,
    })
}

/// Everything needed to move capture frames to the viewer: the exported
/// image ring, its synchronization timelines, and the stream connection.
struct Transport {
    ring: Option<ExportImageRing>,
    producer_timeline: Option<ExportTimelineSemaphore>,
    consumer_timeline: Option<ExportTimelineSemaphore>,
    ipc: Option<LocalSocket>,
}

/// Builds a capture transport at the given extent. Called at swapchain
/// creation with the swapchain extent, and again from the present path when
/// tracked content needs a different allocation than the swapchain.
fn create_transport(
    capture: Option<&CaptureDevice>,
    extent: vk::Extent2D,
    format: vk::Format,
    source_window: isize,
) -> Transport {
    let connection = begin_viewer_connection();
    let handle_mode = if connection
        .as_ref()
        .is_some_and(|value| value.preferred_consumer == ConsumerApi::D3d12)
        && capture.is_some_and(|capture| {
            capture.supports_foreign_queue
                && (capture.adapter_luid.low_part != 0 || capture.adapter_luid.high_part != 0)
        }) {
        ExternalHandleMode::D3d12
    } else {
        if connection
            .as_ref()
            .is_some_and(|value| value.preferred_consumer == ConsumerApi::D3d12)
        {
            log(
                "snowglass: Vulkan device cannot provide D3D12 interop; using Vulkan opaque handles",
            );
        }
        ExternalHandleMode::VulkanOpaque
    };
    let ring = capture.and_then(|capture| {
        match ExportImageRing::create_with_function(
            capture.device.clone(),
            capture.get_memory_win32_handle,
            capture.get_memory_win32_handle_properties,
            &capture.memory_properties,
            capture.adapter_luid,
            ExportImageRingDesc {
                extent,
                format,
                slots: 3,
                handle_mode,
            },
        ) {
            Ok(ring) => {
                let handles = ring
                    .slots()
                    .iter()
                    .map(|slot| slot.raw_handle())
                    .collect::<Vec<_>>();
                log(format!(
                    "snowglass: attached {}-slot export ring {}x{} {:?}",
                    ring.slots().len(),
                    extent.width,
                    extent.height,
                    handles
                ));
                Some(ring)
            }
            Err(error) => {
                log(format!("snowglass: export ring allocation failed: {error}"));
                None
            }
        }
    });
    let producer_timeline = ring.as_ref().and_then(|_| {
        let capture = capture?;
        match ExportTimelineSemaphore::create_with_function(
            capture.device.clone(),
            capture.get_semaphore_win32_handle,
            capture.import_semaphore_win32_handle,
            capture.adapter_luid,
            handle_mode,
        ) {
            Ok(timeline) => Some(timeline),
            Err(error) => {
                log(format!(
                    "snowglass: export timeline semaphore creation failed: {error}"
                ));
                None
            }
        }
    });
    let consumer_timeline = (handle_mode == ExternalHandleMode::D3d12)
        .then(|| {
            let capture = capture?;
            ExportTimelineSemaphore::create_with_function(
                capture.device.clone(),
                capture.get_semaphore_win32_handle,
                capture.import_semaphore_win32_handle,
                capture.adapter_luid,
                handle_mode,
            )
            .map_err(|error| {
                log(format!(
                    "snowglass: consumer timeline semaphore creation failed: {error}"
                ));
                error
            })
            .ok()
        })
        .flatten();
    let ipc = connection.and_then(|connection| {
        ring.as_ref()
            .zip(producer_timeline.as_ref())
            .and_then(|(ring, timeline)| {
                publish_stream(
                    connection,
                    ring,
                    timeline,
                    consumer_timeline.as_ref(),
                    capture.map_or(
                        AdapterLuid {
                            low_part: 0,
                            high_part: 0,
                        },
                        |capture| capture.adapter_luid,
                    ),
                    extent,
                    format,
                    source_window,
                )
            })
    });
    Transport {
        ring,
        producer_timeline,
        consumer_timeline,
        ipc,
    }
}

pub fn surface_created(surface: vk::SurfaceKHR, source_window: isize) {
    SURFACE_WINDOW.insert(surface, source_window);
    log(format!(
        "snowglass: associated Vulkan surface with HWND {source_window:#x}"
    ));
}

pub fn surface_destroyed(surface: vk::SurfaceKHR) {
    SURFACE_WINDOW.remove(&surface);
}

pub fn device_destroyed(device: vk::Device) {
    SWAPCHAIN.retain(|_, state| state.device != device);
}

pub fn swapchain_created(
    swapchain: vk::SwapchainKHR,
    device: vk::Device,
    capture: Option<Arc<CaptureDevice>>,
    extent: vk::Extent2D,
    format: vk::Format,
    surface: vk::SurfaceKHR,
) {
    let count = SWAPCHAINS_CREATED.fetch_add(1, Ordering::Relaxed) + 1;
    let source_window = SURFACE_WINDOW.get(&surface).map(|entry| *entry).unwrap_or(0);
    let transport = capture.as_deref().map_or(
        Transport {
            ring: None,
            producer_timeline: None,
            consumer_timeline: None,
            ipc: None,
        },
        |capture| create_transport(Some(capture), extent, format, source_window),
    );
    SWAPCHAIN.insert(
        swapchain,
        SwapchainState {
            device,
            capture,
            format,
            extent,
            source_window,
            images: Vec::new(),
            streams: vec![CaptureStream::from_transport(extent, transport)],
            untracked_presents: 0,
            pending_layout: Vec::new(),
            pending_presents: 0,
            tracked_sources: Vec::new(),
            last_present: PRESENTS_SEEN.load(Ordering::Relaxed),
        },
    );
    log(format!(
        "snowglass: intercepted vkCreateSwapchainKHR (count={count})"
    ));
}

pub fn swapchain_destroyed(swapchain: vk::SwapchainKHR) {
    if let Some(state) = SWAPCHAIN.get(&swapchain) {
        for image in &state.images {
            SWAPCHAIN_IMAGE.remove(image);
            tracking::swapchain_image_removed(*image);
        }
    }
    SWAPCHAIN.remove(&swapchain);
    // A freed claim lets a surviving window take over the tracked streams.
    release_tracked_streaming(swapchain);
}

pub fn swapchain_images(swapchain: vk::SwapchainKHR, images: &[vk::Image]) {
    if let Some(mut state) = SWAPCHAIN.get_mut(&swapchain) {
        state.images = images.to_vec();
        if config::tracking_enabled() {
            for image in &state.images {
                SWAPCHAIN_IMAGE.insert(*image, swapchain);
                tracking::swapchain_image_added(*image);
            }
        }
        log(format!(
            "snowglass: associated {} swapchain images ({}x{}, format={})",
            state.images.len(),
            state.extent.width,
            state.extent.height,
            state.format.as_raw()
        ));
    }
}

/// Handles one `vkQueuePresentKHR`: submits capture copies for every
/// presented swapchain and returns the semaphores presentation must wait on
/// instead of the application's, when any copy was chained.
pub fn present(
    queue: vk::Queue,
    queue_family_index: u32,
    presents: &[(vk::SwapchainKHR, u32)],
    waits: &[vk::Semaphore],
) -> Option<Vec<vk::Semaphore>> {
    let mut chained_waits = waits.to_vec();
    let mut copied = false;
    retire_idle_streams(PRESENTS_SEEN.load(Ordering::Relaxed));
    for &(swapchain, image_index) in presents {
        let copy_ready = SWAPCHAIN.get_mut(&swapchain).and_then(|mut state| {
            present_swapchain(
                queue,
                queue_family_index,
                swapchain,
                image_index,
                &mut state,
                &chained_waits,
            )
        });
        if let Some(copy_ready) = copy_ready {
            chained_waits.clear();
            chained_waits.push(copy_ready);
            copied = true;
        }
    }
    let count = PRESENTS_SEEN.fetch_add(1, Ordering::Relaxed) + 1;
    if count == 1 || count % 120 == 0 {
        log(format!(
            "snowglass: intercepted vkQueuePresentKHR (count={count})"
        ));
    }
    copied.then_some(chained_waits)
}

fn present_swapchain(
    queue: vk::Queue,
    queue_family_index: u32,
    swapchain: vk::SwapchainKHR,
    image_index: u32,
    state: &mut SwapchainState,
    chained_waits: &[vk::Semaphore],
) -> Option<vk::Semaphore> {
    let presented_image = state.images.get(image_index as usize).copied()?;
    let capture_rect = tracking::capture_rect(presented_image, state.extent);
    let rect_is_content = capture_rect.is_some_and(|rect| rect.extent != state.extent);
    // The dependency graph resolves the offscreen images that fed this
    // present; it supersedes the single-source fallbacks. A take can
    // transiently fail (feedback loops make every root prune every other),
    // so an empty result re-resolves the last good selection while its
    // images are still live, and a partial one updates matching slots.
    let mut resolved = (config::tracking_enabled() && config::graph_tracking_enabled())
        .then(|| {
            tracking::take_present_sources(
                presented_image,
                config::tracked_render_target_count().unwrap_or(1),
            )
        })
        .unwrap_or_default();
    let previous = tracking::refresh_sources(&state.tracked_sources);
    if resolved.is_empty() {
        resolved = previous;
    } else if resolved.len() < previous.len() {
        let mut merged = previous;
        let mut updated = vec![false; merged.len()];
        for source in resolved {
            if let Some(index) = merged.iter().enumerate().position(|(index, old)| {
                !updated[index]
                    && (old.extent.width, old.extent.height)
                        == (source.extent.width, source.extent.height)
            }) {
                merged[index] = source;
                updated[index] = true;
            } else {
                merged.push(source);
                updated.push(true);
            }
        }
        resolved = merged;
    }
    resolved.truncate(config::tracked_render_target_count().unwrap_or(1));
    state.tracked_sources = resolved.iter().map(|source| source.image).collect();
    // Several windows resolve the same screens; the incumbent owner streams
    // them and the others stream nothing rather than publishing duplicates
    // that race for viewer surfaces.
    let now = PRESENTS_SEEN.load(Ordering::Relaxed);
    state.last_present = now;
    let claimed_elsewhere = !resolved.is_empty() && !claim_tracked_streaming(swapchain, now);
    if claimed_elsewhere {
        resolved.clear();
        state.tracked_sources.clear();
    }
    // The caller tracks selection entries by index; each requested index
    // becomes its own stream, in request order.
    let graph_sources = config::tracked_render_target_indices()
        .unwrap_or_else(|| vec![0])
        .iter()
        .filter_map(|&index| resolved.get(index).copied())
        .collect::<Vec<_>>();
    // One stream per tracked source, or the classic whole-frame stream when
    // nothing resolves. Streams whose extent changed rebuild their transport
    // so tracked screens keep native resolution; surplus streams retire when
    // tracking shrinks or when another window streams the tracked sources.
    // Tracked layouts ride out short selection gaps and hold off layout
    // changes until they persist, rather than churning through rebuilds.
    let current = state
        .streams
        .iter()
        .map(|stream| stream.extent)
        .collect::<Vec<_>>();
    let tracked_layout =
        current.len() > 1 || current.first().is_some_and(|first| *first != state.extent);
    let desired = if claimed_elsewhere {
        state.untracked_presents = 0;
        state.pending_presents = 0;
        Vec::new()
    } else if !graph_sources.is_empty() {
        state.untracked_presents = 0;
        let proposed = graph_sources
            .iter()
            .map(|source| source.extent)
            .collect::<Vec<_>>();
        if proposed == current || !tracked_layout {
            state.pending_presents = 0;
            proposed
        } else {
            if state.pending_layout == proposed {
                state.pending_presents += 1;
            } else {
                state.pending_layout = proposed;
                state.pending_presents = 1;
            }
            if state.pending_presents >= 60 {
                state.pending_presents = 0;
                state.pending_layout.clone()
            } else {
                current.clone()
            }
        }
    } else if tracked_layout && state.untracked_presents < 120 {
        state.untracked_presents += 1;
        current.clone()
    } else {
        state.untracked_presents = 0;
        vec![state.extent]
    };
    // Holding a tracked layout — including through a selection gap — keeps
    // the process-wide ownership fresh, so a sibling window cannot steal the
    // streams during a scene transition.
    if desired.len() > 1 || desired.first().is_some_and(|extent| *extent != state.extent) {
        let _ = claim_tracked_streaming(swapchain, now);
    }
    while state.streams.len() > desired.len() {
        if let Some(mut stream) = state.streams.pop() {
            stream.retire();
        }
    }
    let format = state.format;
    let source_window = state.source_window;
    for (index, extent) in desired.iter().copied().enumerate() {
        let window = stream_window(source_window, index);
        if index >= state.streams.len() {
            let transport = create_transport(state.capture.as_deref(), extent, format, window);
            state
                .streams
                .push(CaptureStream::from_transport(extent, transport));
            continue;
        }
        let stream = &mut state.streams[index];
        if stream.extent != extent && (stream.copy_engine.is_some() || stream.ring.is_some()) {
            stream.retire();
            let transport = create_transport(state.capture.as_deref(), extent, format, window);
            *stream = CaptureStream::from_transport(extent, transport);
        }
    }
    let presentation_source = (config::tracking_enabled()
        && graph_sources.is_empty()
        && !rect_is_content)
        .then(|| tracking::render_target(presented_image))
        .flatten();
    let swapchain_extent = state.extent;
    let swapchain_images = state.images.clone();
    let capture = state.capture.clone();
    // While a layout change is being dampened the source list can differ
    // from the stream list; feed each stream the source matching its extent
    // and let unmatched streams idle a frame.
    let stream_extents = state
        .streams
        .iter()
        .map(|stream| stream.extent)
        .collect::<Vec<_>>();
    let mut source_used = vec![false; graph_sources.len()];
    let assigned = stream_extents
        .iter()
        .map(|extent| {
            let found = graph_sources.iter().enumerate().find_map(|(index, source)| {
                (!source_used[index]
                    && (source.extent.width, source.extent.height)
                        == (extent.width, extent.height))
                    .then_some(index)
            });
            if let Some(index) = found {
                source_used[index] = true;
            }
            found
        })
        .collect::<Vec<_>>();
    // Each copy consumes the previous semaphore in the chain, so the final
    // copy-ready semaphore covers every stream.
    let mut waits = chained_waits.to_vec();
    let mut last_ready = None;
    for (index, stream) in state.streams.iter_mut().enumerate() {
        stream.presents += 1;
        if stream.copy_engine.is_none() {
            let (Some(ring), Some(timeline)) =
                (stream.ring.take(), stream.producer_timeline.take())
            else {
                continue;
            };
            let consumer_timeline = stream.consumer_timeline.take();
            let Some(capture) = capture.as_ref() else {
                continue;
            };
            match SwapchainCopyEngine::create(
                capture.device.clone(),
                ring,
                timeline,
                consumer_timeline,
                swapchain_images.clone(),
                stream.extent,
                queue_family_index,
                capture.set_loader_data,
            ) {
                Ok(engine) => stream.copy_engine = Some(engine),
                Err(error) => {
                    log(format!("snowglass: copy engine creation failed: {error}"));
                    continue;
                }
            }
        }
        // A stream can be rejected while the viewer's surfaces are
        // transiently full (a sibling window's stream had not retired
        // yet); retry the connection periodically.
        if stream.ipc.is_none()
            && stream.presents.is_multiple_of(120)
            && let Some(engine) = stream.copy_engine.as_ref()
            && let Some(capture) = capture.as_ref()
            && let Some(connection) = begin_viewer_connection()
        {
            let (ring, producer_timeline, consumer_timeline) = engine.transport_parts();
            stream.ipc = publish_stream(
                connection,
                ring,
                producer_timeline,
                consumer_timeline,
                capture.adapter_luid,
                stream.extent,
                format,
                stream_window(source_window, index),
            );
        }
        let Some(engine) = stream.copy_engine.as_mut() else {
            continue;
        };
        let source = assigned
            .get(index)
            .copied()
            .flatten()
            .map(|source| &graph_sources[source]);
        // During a selection gap a tracked stream has no source this frame;
        // the whole-frame fallbacks only make sense at swapchain extent.
        if source.is_none() && stream.extent != swapchain_extent {
            continue;
        }
        let render_extent = match source {
            Some(source) => source.extent,
            None => presentation_source.map_or_else(
                || capture_rect.map_or(swapchain_extent, |rect| rect.extent),
                |candidate| candidate.extent,
            ),
        };
        let submission = if let Some(source) = source {
            engine.submit_image_copy(
                queue,
                image_index,
                source.image,
                source.extent,
                source.layout,
                &waits,
            )
        } else if rect_is_content {
            match capture_rect {
                Some(rect) => {
                    engine.submit_region_copy(queue, image_index, rect.offset, rect.extent, &waits)
                }
                None => continue,
            }
        } else if let Some(candidate) = presentation_source {
            engine.submit_image_copy(
                queue,
                image_index,
                candidate.image,
                candidate.extent,
                candidate.layout,
                &waits,
            )
        } else {
            engine.submit_copy(queue, image_index, &waits)
        };
        match submission {
            Ok(submission) => {
                let count = COPIES_SUBMITTED.fetch_add(1, Ordering::Relaxed) + 1;
                stream.frame_number += 1;
                let cropped =
                    source.is_none() && (rect_is_content || presentation_source.is_some());
                let notification = Message::FrameReady(FrameReady {
                    stream_id: StreamId(swapchain.as_raw() ^ ((index as u64) << 56)),
                    slot: submission.slot,
                    render_width: render_extent.width,
                    render_height: render_extent.height,
                    capture_x: if cropped {
                        capture_rect.map_or(0, |rect| rect.offset.x as u32)
                    } else {
                        0
                    },
                    capture_y: if cropped {
                        capture_rect.map_or(0, |rect| rect.offset.y as u32)
                    } else {
                        0
                    },
                    capture_width: if cropped {
                        capture_rect.map_or(swapchain_extent.width, |r| r.extent.width)
                    } else {
                        render_extent.width
                    },
                    capture_height: if cropped {
                        capture_rect.map_or(swapchain_extent.height, |r| r.extent.height)
                    } else {
                        render_extent.height
                    },
                    frame_number: stream.frame_number,
                    sync_value: submission.sync_value,
                    timestamp_qpc: performance_counter().unwrap_or(0),
                });
                if stream
                    .ipc
                    .as_mut()
                    .is_some_and(|socket| socket.send(notification).is_err())
                {
                    log("snowglass: viewer socket disconnected");
                    stream.ipc = None;
                }
                if count == 1 || count % 120 == 0 {
                    log(format!(
                        "snowglass: submitted capture {}x{} (count={count}, stream={index}, tracked={})",
                        render_extent.width,
                        render_extent.height,
                        source.is_some(),
                    ));
                }
                waits.clear();
                waits.push(submission.present_ready);
                last_ready = Some(submission.present_ready);
            }
            Err(vk::Result::NOT_READY) => {}
            Err(error) => {
                log(format!("snowglass: swapchain copy failed: {error}"));
            }
        }
    }
    last_ready
}

/// The default (untracked) capture path: the strongest render-target
/// candidate of each submission is copied on the graphics queue, outside the
/// presentation chain.
pub fn tracked_submission(
    queue: vk::Queue,
    queue_family_index: u32,
    command_buffers: &[vk::CommandBuffer],
) {
    if config::tracking_enabled() {
        return;
    }
    let mut selected = HashMap::<vk::SwapchainKHR, tracking::RenderTargetCandidate>::new();
    for command in command_buffers {
        let Some(candidate) = tracking::command_candidate(*command) else {
            continue;
        };
        let Some(swapchain) = SWAPCHAIN_IMAGE.get(&candidate.0).map(|entry| *entry) else {
            continue;
        };
        selected
            .entry(swapchain)
            .and_modify(|current| {
                if candidate.1.is_better_than(*current) {
                    *current = candidate.1;
                }
            })
            .or_insert(candidate.1);
    }
    for (swapchain, candidate) in selected {
        let Some(mut state) = SWAPCHAIN.get_mut(&swapchain) else {
            continue;
        };
        let images = state.images.clone();
        let capture = state.capture.clone();
        let Some(stream) = state.streams.first_mut() else {
            continue;
        };
        if stream.copy_engine.is_none()
            && let (Some(ring), Some(timeline)) =
                (stream.ring.take(), stream.producer_timeline.take())
        {
            let consumer_timeline = stream.consumer_timeline.take();
            let Some(capture) = capture.as_ref() else {
                continue;
            };
            match SwapchainCopyEngine::create(
                capture.device.clone(),
                ring,
                timeline,
                consumer_timeline,
                images,
                stream.extent,
                queue_family_index,
                capture.set_loader_data,
            ) {
                Ok(engine) => stream.copy_engine = Some(engine),
                Err(error) => log(format!(
                    "snowglass: tracked copy engine creation failed: {error}"
                )),
            }
        }
        if let Some(engine) = stream.copy_engine.as_mut() {
            match engine.submit_tracked_copy(
                queue,
                candidate.image,
                candidate.extent,
                candidate.layout,
            ) {
                Ok(submission) => {
                    let count = COPIES_SUBMITTED.fetch_add(1, Ordering::Relaxed) + 1;
                    stream.frame_number += 1;
                    let notification = Message::FrameReady(FrameReady {
                        stream_id: StreamId(swapchain.as_raw()),
                        slot: submission.slot,
                        render_width: candidate.extent.width,
                        render_height: candidate.extent.height,
                        capture_x: 0,
                        capture_y: 0,
                        capture_width: candidate.extent.width,
                        capture_height: candidate.extent.height,
                        frame_number: stream.frame_number,
                        sync_value: submission.sync_value,
                        timestamp_qpc: performance_counter().unwrap_or(0),
                    });
                    if stream
                        .ipc
                        .as_mut()
                        .is_some_and(|socket| socket.send(notification).is_err())
                    {
                        stream.ipc = None;
                    }
                    if count == 1 || count % 120 == 0 {
                        log(format!(
                            "snowglass: submitted graphics-queue capture {}x{} (count={count})",
                            candidate.extent.width, candidate.extent.height
                        ));
                    }
                }
                Err(vk::Result::NOT_READY) => {}
                Err(error) => log(format!("snowglass: tracked copy failed: {error}")),
            }
        }
    }
}
