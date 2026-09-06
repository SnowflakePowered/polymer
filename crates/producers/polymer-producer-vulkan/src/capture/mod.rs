//! Swapchain capture orchestration: per-target export streams, their viewer
//! transports, and the copies that feed them.
//!
//! The hook layer forwards raw swapchain lifecycle and presentation events;
//! everything about what to capture and how to move it lives here.

use crate::tracking;
mod operation;
mod retirement;
mod routing;
use crate::{
    ExportImageRing, ExportImageRingDesc, ExportTimelineSemaphore, ExternalHandleMode,
    SetDeviceLoaderData, SwapchainCopyEngine,
};
use ash::{vk, vk::Handle};
use dashmap::DashMap;
use parking_lot::Mutex;
use polymer_hook::config;
use polymer_platform_windows::{NativeWindow, performance_counter, window_title};
use polymer_presentation::producer::ExportDescriptor;
use polymer_presentation::producer::ProducerSession;
use polymer_protocol::{
    AdapterLuid, ConsumerApi, ExternalImageKind, ExternalSyncKind, PresentationTransform, StreamId,
    SurfaceId,
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
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

/// One exported stream: a ring, its synchronization, its viewer connection,
/// and the engine copying into it. Each tracked render target gets its own
/// stream so the viewer can show the screens as separate surfaces.
struct CaptureStream {
    extent: vk::Extent2D,
    surface_id: SurfaceId,
    target_window: isize,
    presentation_extent: vk::Extent2D,
    native: Option<StreamResources>,
    requirements: Option<CaptureRequirements>,
    retry: polymer_presentation::producer::ConnectionRetry,
}

#[derive(PartialEq)]
struct CaptureRequirements {
    extent: vk::Extent2D,
    window: isize,
    format: vk::Format,
    queue_family: u32,
    images: Vec<vk::Image>,
}

type RetiredCopies = Arc<Mutex<retirement::RetiredCopies>>;

struct StreamResources {
    copy_engine: Option<SwapchainCopyEngine>,
    retirement: RetiredCopies,
    ipc: Option<ProducerSession>,
}
impl Drop for StreamResources {
    fn drop(&mut self) {
        if let Some(engine) = self.copy_engine.take() {
            self.retirement.lock().retire(engine);
        }
    }
}
impl CaptureStream {
    fn new(
        extent: vk::Extent2D,
        surface_id: SurfaceId,
        target_window: isize,
        presentation_extent: vk::Extent2D,
    ) -> Self {
        Self {
            extent,
            surface_id,
            target_window,
            presentation_extent,
            native: None,
            requirements: None,
            retry: Default::default(),
        }
    }
    fn retire(&mut self) {
        self.native = None;
        self.retry = Default::default();
    }
    fn initialize(
        &mut self,
        capture: Option<&CaptureDevice>,
        presentation_semaphores: &crate::swapchain::PresentationSemaphores,
        retirement: &RetiredCopies,
        format: vk::Format,
        images: &[vk::Image],
        queue_family_index: u32,
    ) {
        let requirements = CaptureRequirements {
            extent: self.extent,
            window: self.target_window,
            format,
            queue_family: queue_family_index,
            images: images.to_vec(),
        };
        if self.requirements.as_ref() != Some(&requirements) {
            self.retire();
            self.requirements = Some(requirements);
        }
        if self
            .native
            .as_ref()
            .is_some_and(|native| !native.connected())
        {
            self.native = None;
            self.retry = polymer_presentation::producer::ConnectionRetry::Parked;
        }
        if self.native.is_some() || !self.retry.begin() {
            return;
        }
        match unsafe {
            self.create_resources(
                capture,
                presentation_semaphores,
                retirement,
                format,
                images,
                queue_family_index,
            )
        } {
            Ok(Some(native)) => self.native = Some(native),
            Ok(None) => self.retry = polymer_presentation::producer::ConnectionRetry::Parked,
            Err(error) => {
                tracing::debug!("polymer: Vulkan capture initialization deferred: {error}")
            }
        }
    }
}

impl CaptureStream {
    unsafe fn create_resources(
        &self,
        capture: Option<&CaptureDevice>,
        presentation_semaphores: &crate::swapchain::PresentationSemaphores,
        retirement: &RetiredCopies,
        format: vk::Format,
        images: &[vk::Image],
        queue_family_index: u32,
    ) -> Result<Option<StreamResources>, String> {
        let transport = create_transport(
            capture,
            self.extent,
            format,
            self.surface_id,
            self.target_window,
        );
        if transport.ipc.is_none() {
            return if transport.parked {
                Ok(None)
            } else {
                Err("transport unavailable".into())
            };
        }
        let capture = capture.ok_or("capture device unavailable")?;
        let ring = transport.ring.ok_or("export ring unavailable")?;
        let timeline = transport
            .producer_timeline
            .ok_or("producer timeline unavailable")?;
        let copy_engine = SwapchainCopyEngine::create(
            capture.device.clone(),
            presentation_semaphores,
            self.surface_id.0,
            ring,
            timeline,
            transport.consumer_timeline,
            images.to_vec(),
            self.extent,
            queue_family_index,
            capture.set_loader_data,
        )
        .map_err(|error| error.to_string())?;
        Ok(Some(StreamResources {
            copy_engine: Some(copy_engine),
            retirement: retirement.clone(),
            ipc: transport.ipc,
        }))
    }
}

struct SwapchainState {
    image_array_layers: u32,
    retirement: RetiredCopies,
    presentation_semaphores: crate::swapchain::PresentationSemaphores,
    device: vk::Device,
    capture: Option<Arc<CaptureDevice>>,
    tracking: Arc<tracking::Tracking>,
    format: vk::Format,
    extent: vk::Extent2D,
    source_window: isize,
    creation_order: u64,
    window_order: usize,
    images: Vec<vk::Image>,
    /// Stream 0 always exists (the whole-frame capture when nothing is
    /// tracked); additional streams appear per tracked render target.
    streams: Vec<CaptureStream>,
    /// Validated resources, stable slots, and shared layout hysteresis.
    history: polymer_presentation::history::ResourceHistory<tracking::PresentSource>,
    /// Global present count when this swapchain last presented. A window
    /// that stops presenting (a splash screen left alive but idle) must not
    /// keep holding viewer surfaces with a frozen stream.
    last_present: u64,
}

impl Drop for SwapchainState {
    fn drop(&mut self) {
        self.streams.clear();
        // Native destruction is an explicit synchronization boundary. Normal
        // layout changes and disconnects only poll, and never wait here.
        self.retirement.lock().wait();
    }
}

#[derive(Clone)]
struct GraphPresentation {
    domain: u64,
    source_window: isize,
    presentation_extent: vk::Extent2D,
    creation_order: u64,
    window_order: usize,
    last_present: u64,
    sources: Vec<tracking::PresentSource>,
}

#[derive(Clone, Copy)]
struct PresentationTarget {
    source_window: isize,
    presentation_extent: vk::Extent2D,
    presentation_rect: Option<vk::Rect2D>,
}

/// Capture state shared by the hook contexts associated with one Vulkan
/// instance and propagated to its devices and queues.
#[derive(Default)]
pub struct CaptureState {
    swapchains: DashMap<vk::SwapchainKHR, SwapchainState>,
    surface_windows: DashMap<vk::SurfaceKHR, isize>,
    swapchain_images: DashMap<vk::Image, vk::SwapchainKHR>,
    swapchains_created: AtomicU64,
    presents_seen: AtomicU64,
    /// Which swapchain streams tracked render targets. An application with
    /// several windows resolves the same screens on every one of them;
    /// without a single owner each window would publish redundant streams.
    presentation_registry: polymer_presentation::registry::PresentationRegistry,
    /// Latest graph resolution for each live swapchain. When several windows
    /// expose the same multi-source graph, source order is paired with stable
    /// window creation order so input can return to the corresponding window
    /// even though only one swapchain owns the exported copies.
    graph_presentations: DashMap<vk::SwapchainKHR, GraphPresentation>,
    graph_target_layouts: DashMap<(u64, u32, u32), PresentationTarget>,
    /// Stable HWND order comes from Vulkan surface creation rather than
    /// swapchain creation: applications can recreate sibling swapchains in a
    /// different order during resize or startup transitions.
    window_order: Mutex<Vec<isize>>,
    /// HWNDs that actually acquired swapchains. Some toolkits create Vulkan
    /// surfaces for non-presenting controls; those must not consume a regex
    /// tracking slot ahead of the real render windows.
    swapchain_windows: Mutex<Vec<isize>>,
}

/// Presents (across all swapchains) an idle swapchain may hold streams for.
const IDLE_STREAM_HORIZON: u64 = 600;

impl CaptureState {
    fn tracked_surface_slot(&self, source_window: isize) -> Option<usize> {
        if !config::tracking_enabled() {
            return None;
        }
        let windows = self.swapchain_windows.lock().clone();
        config::tracked_render_target_surface_slots(&windows, |window| {
            window_title(NativeWindow(window))
        })
        .iter()
        .position(|window| *window == Some(source_window))
    }

    fn observe_graph_presentation(
        &self,
        swapchain: vk::SwapchainKHR,
        state: &SwapchainState,
        sources: &[tracking::PresentSource],
        now: u64,
    ) {
        if !sources.is_empty() {
            let sources = sources
                .iter()
                .map(|source| {
                    format!(
                        "{:?}:{}x{}:{}",
                        source.image,
                        source.extent.width,
                        source.extent.height,
                        source.presentation_rect.map_or_else(
                            || "?".to_owned(),
                            |rect| format!(
                                "{},{}+{}x{}",
                                rect.offset.x, rect.offset.y, rect.extent.width, rect.extent.height
                            )
                        )
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            tracing::debug!(
                "polymer: graph presentation {swapchain:?} window {:#x}: [{sources}]",
                state.source_window
            );
        }
        let presentations = &self.graph_presentations;
        if sources.is_empty() {
            presentations.remove(&swapchain);
        } else {
            presentations.insert(
                swapchain,
                GraphPresentation {
                    domain: state.device.as_raw(),
                    source_window: state.source_window,
                    presentation_extent: state.extent,
                    creation_order: state.creation_order,
                    window_order: state.window_order,
                    last_present: now,
                    sources: sources.to_vec(),
                },
            );
        }
    }

    fn claim_tracked_streaming(
        &self,
        swapchain: vk::SwapchainKHR,
        device: vk::Device,
        sources: &[tracking::PresentSource],
        now: u64,
    ) -> bool {
        let identities = sources
            .iter()
            .map(|source| polymer_presentation::registry::SourceIdentity {
                domain: device.as_raw(),
                resource: source.incarnation,
                subresource: source.subresource.0 | (source.subresource.1 << 8),
            })
            .collect::<Vec<_>>();
        self.presentation_registry
            .claim(swapchain.as_raw(), &identities, now)
    }
    fn release_tracked_streaming(&self, swapchain: vk::SwapchainKHR) {
        self.presentation_registry.forget_owner(swapchain.as_raw());
    }

    /// Retires the streams of swapchains that have stopped presenting, freeing
    /// their viewer surfaces. Presenting again rebuilds them on demand.
    fn retire_idle_streams(&self, now: u64) {
        for mut entry in self.swapchains.iter_mut() {
            if entry.last_present.saturating_add(IDLE_STREAM_HORIZON) >= now
                || entry.streams.is_empty()
            {
                continue;
            }
            tracing::debug!(
                "polymer: retiring {} stream(s) of an idle swapchain (window {:#x})",
                entry.streams.len(),
                entry.source_window
            );
            for mut stream in entry.streams.drain(..) {
                stream.retire();
            }
        }
    }
}

/// Stable logical identity for one of a window's capture streams. It is kept
/// separate from the real HWND so every stream can route input to the window.
fn stream_surface_id(source_window: isize, index: usize) -> SurfaceId {
    SurfaceId(source_window as u64 ^ ((index as u64) << 56))
}

/// Graph selections are indexed independently of capture ownership and their
/// presentation HWND. Keeping this identity stable preserves viewer ordering,
/// CLI rotation, and replacement across swapchain recreation.
fn graph_surface_id(index: usize) -> SurfaceId {
    SurfaceId(polymer_presentation::registry::PresentationRegistry::logical_surface_id(index))
}

fn direct_surface_selected(tracking_enabled: bool, tracked_slot: Option<usize>) -> bool {
    !tracking_enabled || tracked_slot.is_some()
}

struct ViewerConnection {
    socket: ProducerSession,
    process_id: u32,
    preferred_consumer: ConsumerApi,
}

fn begin_viewer_connection() -> std::io::Result<ViewerConnection> {
    let session = config::session().map_err(|error| std::io::Error::other(error.to_string()))?;
    let socket = ProducerSession::connect(&session)?;
    Ok(ViewerConnection {
        process_id: socket.viewer_pid(),
        preferred_consumer: socket.preferred_consumer(),
        socket,
    })
}

fn publish_stream(
    mut connection: ViewerConnection,
    ring: &ExportImageRing,
    producer_timeline: &ExportTimelineSemaphore,
    consumer_timeline: Option<&ExportTimelineSemaphore>,
    adapter_luid: AdapterLuid,
    extent: vk::Extent2D,
    format: vk::Format,
    surface_id: SurfaceId,
    source_window: isize,
) -> std::io::Result<ProducerSession> {
    let viewer_pid = connection.process_id;

    let handles = ring
        .slots()
        .iter()
        .map(|slot| slot.raw_handle() as i64)
        .collect::<Vec<_>>();
    let d3d12 = ring.desc().handle_mode == ExternalHandleMode::D3d12;
    let stream_format = if d3d12 {
        match dxgi_format(format) {
            Some(format) => format,
            None => {
                tracing::debug!(
                    "polymer: Vulkan format {} has no D3D12 interop mapping",
                    format.as_raw()
                );
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "unsupported export configuration",
                ));
            }
        }
    } else {
        format.as_raw() as u32
    };
    let ready = ExportDescriptor {
        surface_id,
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
        image_handles: &handles,
        timeline_semaphore_handle: producer_timeline.raw_handle() as i64,
        consumer_semaphore_handle: consumer_timeline
            .map_or(0, |timeline| timeline.raw_handle() as i64),
        bottom_up: false,
    };
    connection.socket.publish(ready)?;
    tracing::debug!("polymer: connected export ring to viewer process {viewer_pid}");
    Ok(connection.socket)
}

fn dxgi_format(format: vk::Format) -> Option<u32> {
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R10G10B10A2_UNORM,
        DXGI_FORMAT_R16G16B16A16_FLOAT,
    };
    Some(match format {
        vk::Format::R8G8B8A8_UNORM => DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32,
        vk::Format::R8G8B8A8_SRGB => DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32,
        vk::Format::B8G8R8A8_UNORM => DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
        vk::Format::B8G8R8A8_SRGB => DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
        vk::Format::A2B10G10R10_UNORM_PACK32 => DXGI_FORMAT_R10G10B10A2_UNORM.0 as u32,
        vk::Format::R16G16B16A16_SFLOAT => DXGI_FORMAT_R16G16B16A16_FLOAT.0 as u32,
        _ => return None,
    })
}

/// Swapchain pixels are already encoded for presentation. Exporting an sRGB
/// image makes consumers decode those bytes while sampling or blitting, then
/// write the resulting linear values to an UNORM presentation target. Use the
/// format-compatible UNORM twin so copies preserve the encoded pixel values.
fn presentation_export_format(format: vk::Format) -> vk::Format {
    match format {
        vk::Format::R8G8B8A8_SRGB => vk::Format::R8G8B8A8_UNORM,
        vk::Format::B8G8R8A8_SRGB => vk::Format::B8G8R8A8_UNORM,
        _ => format,
    }
}

/// Everything needed to move capture frames to the viewer: the exported
/// image ring, its synchronization timelines, and the stream connection.
#[derive(Default)]
struct Transport {
    parked: bool,
    ring: Option<ExportImageRing>,
    producer_timeline: Option<ExportTimelineSemaphore>,
    consumer_timeline: Option<ExportTimelineSemaphore>,
    ipc: Option<ProducerSession>,
}

/// Builds a capture transport at the given extent. Called at swapchain
/// creation with the swapchain extent, and again from the present path when
/// tracked content needs a different allocation than the swapchain.
fn create_transport(
    capture: Option<&CaptureDevice>,
    extent: vk::Extent2D,
    format: vk::Format,
    surface_id: SurfaceId,
    source_window: isize,
) -> Transport {
    let format = presentation_export_format(format);
    let connection = match ProducerSession::setup_result(begin_viewer_connection()) {
        Ok(Some(connection)) => Some(connection),
        Ok(None) => {
            return Transport {
                parked: true,
                ..Default::default()
            };
        }
        Err(error) => {
            tracing::debug!("polymer: viewer connection failed: {error}");
            return Transport::default();
        }
    };
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
            tracing::debug!(
                "polymer: Vulkan device cannot provide D3D12 interop; using Vulkan opaque handles",
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
                tracing::debug!(
                    "polymer: attached {}-slot export ring {}x{} (format={}) {:?}",
                    ring.slots().len(),
                    extent.width,
                    extent.height,
                    format.as_raw(),
                    handles
                );
                Some(ring)
            }
            Err(error) => {
                tracing::debug!("polymer: export ring allocation failed: {error}");
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
                tracing::debug!("polymer: export timeline semaphore creation failed: {error}");
                None
            }
        }
    });
    let consumer_timeline = ring.as_ref().and_then(|_| {
        let capture = capture?;
        ExportTimelineSemaphore::create_with_function(
            capture.device.clone(),
            capture.get_semaphore_win32_handle,
            capture.import_semaphore_win32_handle,
            capture.adapter_luid,
            handle_mode,
        )
        .map_err(|error| {
            tracing::debug!("polymer: consumer timeline semaphore creation failed: {error}");
            error
        })
        .ok()
    });
    if consumer_timeline.is_none() {
        return Transport {
            parked: false,
            ring,
            producer_timeline,
            consumer_timeline,
            ipc: None,
        };
    }
    let mut parked = false;
    let ipc = connection.and_then(|connection| {
        ring.as_ref()
            .zip(producer_timeline.as_ref())
            .and_then(|(ring, timeline)| {
                match ProducerSession::setup_result(publish_stream(
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
                    surface_id,
                    source_window,
                )) {
                    Ok(Some(socket)) => Some(socket),
                    Ok(None) => {
                        parked = true;
                        None
                    }
                    Err(error) => {
                        tracing::debug!("polymer: export publication failed: {error}");
                        None
                    }
                }
            })
    });
    Transport {
        parked,
        ring,
        producer_timeline,
        consumer_timeline,
        ipc,
    }
}

impl CaptureState {
    pub fn surface_created(&self, surface: vk::SurfaceKHR, source_window: isize) {
        self.surface_windows.insert(surface, source_window);
        let mut windows = self.window_order.lock();
        if source_window != 0 && !windows.contains(&source_window) {
            windows.push(source_window);
        }
        drop(windows);
        let title = window_title(NativeWindow(source_window)).unwrap_or_default();
        tracing::debug!(
            "polymer: associated Vulkan surface with HWND {source_window:#x} title {title:?}"
        );
    }

    pub fn surface_destroyed(&self, surface: vk::SurfaceKHR) {
        let source_window = self
            .surface_windows
            .remove(&surface)
            .map(|(_, source_window)| source_window);
        if let Some(source_window) = source_window
            && !self
                .surface_windows
                .iter()
                .any(|entry| *entry.value() == source_window)
            && !self
                .swapchains
                .iter()
                .any(|state| state.source_window == source_window)
        {
            self.graph_target_layouts
                .retain(|_, target| target.source_window != source_window);
            self.swapchain_windows
                .lock()
                .retain(|window| *window != source_window);
        }
    }

    pub fn device_destroyed(&self, device: vk::Device) {
        let owners = self
            .swapchains
            .iter()
            .filter(|state| state.device == device)
            .map(|state| *state.key())
            .collect::<Vec<_>>();
        for owner in owners {
            self.release_tracked_streaming(owner);
        }
        self.graph_target_layouts
            .retain(|(domain, _, _), _| *domain != device.as_raw());
        self.swapchains.retain(|_, state| state.device != device);
        let presentations = &self.graph_presentations;
        presentations.retain(|swapchain, _| self.swapchains.contains_key(swapchain));
    }

    pub fn swapchain_created(
        &self,
        swapchain: vk::SwapchainKHR,
        device: vk::Device,
        capture: Option<Arc<CaptureDevice>>,
        tracking: Arc<tracking::Tracking>,
        extent: vk::Extent2D,
        format: vk::Format,
        surface: vk::SurfaceKHR,
        image_array_layers: u32,
    ) {
        let count = self.swapchains_created.fetch_add(1, Ordering::Relaxed) + 1;
        let source_window = self
            .surface_windows
            .get(&surface)
            .map(|entry| *entry)
            .unwrap_or(0);
        let window_order = self
            .window_order
            .lock()
            .iter()
            .position(|window| *window == source_window)
            .unwrap_or(usize::MAX);
        {
            let mut windows = self.swapchain_windows.lock();
            if source_window != 0 && !windows.contains(&source_window) {
                windows.push(source_window);
            }
        }
        let tracked_slot = self.tracked_surface_slot(source_window);
        let surface_id =
            tracked_slot.map_or_else(|| stream_surface_id(source_window, 0), graph_surface_id);
        let selected = direct_surface_selected(config::tracking_enabled(), tracked_slot);
        let streams = selected
            .then(|| CaptureStream::new(extent, surface_id, source_window, extent))
            .into_iter()
            .collect();
        self.swapchains.insert(
            swapchain,
            SwapchainState {
                image_array_layers,
                retirement: Default::default(),
                presentation_semaphores: Default::default(),
                device,
                capture,
                tracking,
                format,
                extent,
                source_window,
                creation_order: count,
                window_order,
                images: Vec::new(),
                streams,
                history: Default::default(),
                last_present: self.presents_seen.load(Ordering::Relaxed),
            },
        );
        tracing::debug!(
            "polymer: intercepted vkCreateSwapchainKHR (count={count}, window {source_window:#x}, tracking slot {tracked_slot:?}, selected={selected})"
        );
    }

    pub fn swapchain_destroyed(&self, swapchain: vk::SwapchainKHR) {
        if let Some(state) = self.swapchains.get(&swapchain) {
            for image in &state.images {
                self.swapchain_images.remove(image);
                state.tracking.swapchain_image_removed(*image);
            }
        }
        self.swapchains.remove(&swapchain);
        self.graph_presentations.remove(&swapchain);
        // A freed claim lets a surviving window take over the tracked streams.
        self.release_tracked_streaming(swapchain);
    }

    pub fn swapchain_images(&self, swapchain: vk::SwapchainKHR, images: &[vk::Image]) {
        if let Some(mut state) = self.swapchains.get_mut(&swapchain) {
            state.images = images.to_vec();
            for image in &state.images {
                state.tracking.swapchain_image_added(*image, state.extent);
                state
                    .tracking
                    .image_subresources(*image, 1, state.image_array_layers);
            }
            if config::tracking_enabled() {
                for image in &state.images {
                    self.swapchain_images.insert(*image, swapchain);
                }
            }
            tracing::debug!(
                "polymer: associated {} swapchain images ({}x{}, format={})",
                state.images.len(),
                state.extent.width,
                state.extent.height,
                state.format.as_raw()
            );
        }
    }

    /// Handles one `vkQueuePresentKHR`: submits capture copies for every
    /// presented swapchain and returns the semaphores presentation must wait on
    /// instead of the application's, when any copy was chained.
    pub fn present(
        &self,
        tracking: &tracking::Tracking,
        queue: vk::Queue,
        queue_family_index: u32,
        presents: &[(vk::SwapchainKHR, u32)],
        waits: &[vk::Semaphore],
    ) -> Option<Vec<vk::Semaphore>> {
        let mut chained_waits = waits.to_vec();
        let mut copied = false;
        self.retire_idle_streams(self.presents_seen.load(Ordering::Relaxed));
        for &(swapchain, image_index) in presents {
            let copy_ready = self.swapchains.get_mut(&swapchain).and_then(|mut state| {
                self.present_swapchain(
                    tracking,
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
        let count = self.presents_seen.fetch_add(1, Ordering::Relaxed) + 1;
        if count == 1 || count % 120 == 0 {
            tracing::debug!("polymer: intercepted vkQueuePresentKHR (count={count})");
        }
        copied.then_some(chained_waits)
    }

    fn present_swapchain(
        &self,
        tracking: &tracking::Tracking,
        queue: vk::Queue,
        queue_family_index: u32,
        swapchain: vk::SwapchainKHR,
        image_index: u32,
        state: &mut SwapchainState,
        chained_waits: &[vk::Semaphore],
    ) -> Option<vk::Semaphore> {
        state.retirement.lock().poll();
        let presented_image = state.images.get(image_index as usize).copied()?;
        let capture_rect = tracking.capture_rect(presented_image, state.extent);
        let rect_is_content = capture_rect.is_some_and(|rect| rect.extent != state.extent);

        let observed = config::tracking_enabled()
            .then(|| {
                tracking.take_present_sources(
                    presented_image,
                    config::tracked_render_target_count().unwrap_or(1),
                )
            })
            .unwrap_or_default();
        let exact = !observed.is_empty() && observed.iter().all(|source| source.exact_selection);
        let reconciled = state.history.reconcile(observed, exact, |source| {
            let Some(refreshed) = tracking.refresh_source(source) else {
                return false;
            };
            *source = refreshed;
            true
        });
        let mut resolved = reconciled
            .surfaces
            .into_iter()
            .zip(reconciled.slots)
            .map(|(mut source, slot)| {
                source.logical_slot = Some(slot);
                source
            })
            .collect::<Vec<_>>();
        let now = self.presents_seen.load(Ordering::Relaxed);
        self.observe_graph_presentation(swapchain, state, &resolved, now);
        let requested_indices = config::tracked_render_target_indices().unwrap_or_else(|| vec![0]);
        let (selected_requests, graph_targets) = self.presentation_targets(
            state.device.as_raw(),
            state.source_window,
            state.extent,
            &resolved,
            &requested_indices,
            now,
        );
        // Several windows resolve the same screens; the incumbent owner streams
        // them and the others stream nothing rather than publishing duplicates
        // that race for viewer surfaces.
        state.last_present = now;
        let claimed_elsewhere = !resolved.is_empty()
            && !self.claim_tracked_streaming(swapchain, state.device, &resolved, now);
        if claimed_elsewhere {
            resolved.clear();
        }
        // The caller tracks selection entries by index; each requested index
        // becomes its own stream, in request order.
        let graph_request_slots = selected_requests
            .iter()
            .filter_map(|&(slot, index)| resolved.get(index).map(|_| slot))
            .collect::<Vec<_>>();
        let graph_sources = selected_requests
            .iter()
            .filter_map(|&(_, index)| resolved.get(index).copied())
            .collect::<Vec<_>>();
        let selected_surface = direct_surface_selected(
            config::tracking_enabled(),
            self.tracked_surface_slot(state.source_window),
        );
        let fallback = PresentationTarget {
            source_window: state.source_window,
            presentation_extent: state.extent,
            presentation_rect: None,
        };
        let mut old_streams = std::mem::take(&mut state.streams)
            .into_iter()
            .map(|stream| (stream.surface_id, stream))
            .collect::<HashMap<_, _>>();
        let desired = graph_sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                (
                    source.extent,
                    graph_surface_id(graph_request_slots[index]),
                    graph_targets.get(index).copied().unwrap_or(fallback),
                )
            })
            .collect::<Vec<_>>();
        let desired = if desired.is_empty()
            && !claimed_elsewhere
            && selected_surface
            && resolved.is_empty()
        {
            vec![(
                state.extent,
                self.tracked_surface_slot(state.source_window).map_or_else(
                    || stream_surface_id(state.source_window, 0),
                    graph_surface_id,
                ),
                fallback,
            )]
        } else {
            desired
        };
        let format = state.format;
        for (extent, surface_id, target) in desired {
            let source_changed = graph_request_slots.iter().position(|&slot| graph_surface_id(slot) == surface_id)
                .and_then(|index| graph_sources[index].logical_slot).is_some_and(|source_slot| {
                    reconciled.changes.iter().any(|change| matches!(change,
                        polymer_presentation::history::SurfaceChange::Add { slot }
                        | polymer_presentation::history::SurfaceChange::Replace { slot } if *slot == source_slot))
                });
            let mut stream = old_streams.remove(&surface_id).unwrap_or_else(|| {
                CaptureStream::new(
                    extent,
                    surface_id,
                    target.source_window,
                    target.presentation_extent,
                )
            });
            stream.extent = extent;
            stream.target_window = target.source_window;
            stream.presentation_extent = target.presentation_extent;
            if source_changed {
                stream.retry.source_changed();
            }
            state.streams.push(stream);
        }
        // Removed streams enqueue their copy resources for nonblocking
        // retirement. Swapchain-owned presentation semaphores remain alive.
        drop(old_streams);
        let swapchain_extent = state.extent;
        let swapchain_images = state.images.clone();
        let capture = state.capture.clone();
        let assigned = state
            .streams
            .iter()
            .map(|stream| {
                graph_request_slots
                    .iter()
                    .position(|&slot| graph_surface_id(slot) == stream.surface_id)
            })
            .collect::<Vec<_>>();
        // Each copy consumes the previous semaphore in the chain, so the final
        // copy-ready semaphore covers every stream.
        let mut waits = chained_waits.to_vec();
        let mut last_ready = None;
        for (index, stream) in state.streams.iter_mut().enumerate() {
            stream.initialize(
                capture.as_deref(),
                &state.presentation_semaphores,
                &state.retirement,
                format,
                &swapchain_images,
                queue_family_index,
            );
            let assigned_source = assigned.get(index).copied().flatten();
            let source = assigned_source.map(|source| &graph_sources[source]);
            // During a selection gap a tracked stream has no source this frame;
            // the whole-frame fallbacks only make sense at swapchain extent.
            if source.is_none() && stream.extent != swapchain_extent {
                continue;
            }
            let render_extent = match source {
                Some(source) => source.extent,
                None => capture_rect.map_or(swapchain_extent, |rect| rect.extent),
            };
            let Some(native) = stream.native.as_mut() else {
                continue;
            };
            if !native.connected() {
                stream.native = None;
                stream.retry = polymer_presentation::producer::ConnectionRetry::Parked;
                continue;
            }
            let submission = unsafe {
                native.copy_frame(
                    queue,
                    image_index,
                    source,
                    capture_rect,
                    rect_is_content,
                    &waits,
                )
            };
            match submission {
                Ok(Some(submission)) => {
                    // Chain the submitted semaphore even if connection polling
                    // retired the engine after submission. The swapchain owns
                    // the semaphore independently of the retired copy resources.
                    let cropped = source.is_none() && rect_is_content;
                    let presentation_rect = if source.is_some() {
                        assigned_source
                            .and_then(|source| graph_targets.get(source))
                            .and_then(|target| target.presentation_rect)
                    } else if cropped {
                        capture_rect
                    } else {
                        Some(vk::Rect2D {
                            offset: vk::Offset2D { x: 0, y: 0 },
                            extent: render_extent,
                        })
                    };
                    let notification = polymer_presentation::producer::CapturedFrame {
                        stream_id: StreamId(
                            swapchain.as_raw() ^ stream.surface_id.0.rotate_left(17),
                        ),
                        slot: submission.slot,
                        sync_value: submission.sync_value,
                        timestamp_qpc: performance_counter().unwrap_or(0),
                        extent: polymer_presentation::Extent2D {
                            width: render_extent.width,
                            height: render_extent.height,
                        },
                    }
                    .present(polymer_presentation::producer::FramePlacement {
                        extent: polymer_presentation::Extent2D {
                            width: stream.presentation_extent.width,
                            height: stream.presentation_extent.height,
                        },
                        bounds: polymer_presentation::Rect {
                            x: (presentation_rect.map_or(0, |rect| rect.offset.x as u32)) as i32,
                            y: (presentation_rect.map_or(0, |rect| rect.offset.y as u32)) as i32,
                            extent: polymer_presentation::Extent2D {
                                width: presentation_rect
                                    .map_or(render_extent.width, |rect| rect.extent.width),
                                height: presentation_rect
                                    .map_or(render_extent.height, |rect| rect.extent.height),
                            },
                        },
                        transform: if let Some(rect) = presentation_rect {
                            polymer_presentation::producer::FramePlacement::mapped(
                                polymer_presentation::Rect {
                                    x: rect.offset.x,
                                    y: rect.offset.y,
                                    extent: polymer_presentation::Extent2D {
                                        width: rect.extent.width,
                                        height: rect.extent.height,
                                    },
                                },
                                source.is_some_and(|source| source.orientation.0),
                                source.is_some_and(|source| source.orientation.1),
                            )
                        } else {
                            PresentationTransform::UNAVAILABLE
                        },
                    });
                    if let Some(native) = stream.native.as_mut() {
                        ProducerSession::publish_frame(&mut native.ipc, notification);
                    }
                    waits.clear();
                    waits.push(submission.present_ready);
                    last_ready = Some(submission.present_ready);
                }
                Ok(None) => {}
                Err(_) => {}
            }
        }
        last_ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swapchain_array_metadata_rejects_missing_layers_and_destroyed_images() {
        let state = CaptureState::default();
        let tracking = Arc::new(tracking::Tracking::default());
        let swapchain = vk::SwapchainKHR::from_raw(1);
        let image = vk::Image::from_raw(2);
        state.swapchain_created(
            swapchain,
            vk::Device::null(),
            None,
            tracking.clone(),
            vk::Extent2D {
                width: 64,
                height: 64,
            },
            vk::Format::R8G8B8A8_UNORM,
            vk::SurfaceKHR::null(),
            2,
        );
        state.swapchain_images(swapchain, &[image]);
        tracking.image_view_created(
            vk::ImageView::from_raw(3),
            image,
            vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(2),
        );
        let mut source = graph_source(2, 64, 64);
        source.incarnation = 0;
        source.subresource = (0, 1);
        assert!(tracking.refresh_source(&source).is_some());
        source.subresource = (0, 2);
        assert!(tracking.refresh_source(&source).is_none());
        source.subresource = (0, 1);
        state.swapchain_destroyed(swapchain);
        assert!(tracking.refresh_source(&source).is_none());
        tracking.swapchain_image_added(
            image,
            vk::Extent2D {
                width: 64,
                height: 64,
            },
        );
        tracking.image_subresources(image, 1, 2);
        tracking.image_view_created(
            vk::ImageView::from_raw(4),
            image,
            vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .level_count(1)
                .layer_count(2),
        );
        assert!(tracking.refresh_source(&source).is_none());
    }

    fn graph_source(image: u64, width: u32, height: u32) -> tracking::PresentSource {
        tracking::PresentSource {
            logical_slot: None,
            subresource: (0, 0),
            incarnation: image,
            orientation: (false, false),
            exact_selection: false,
            source_offset: vk::Offset2D::default(),
            allocation_extent: vk::Extent2D { width, height },
            image: vk::Image::from_raw(image),
            extent: vk::Extent2D { width, height },
            layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            presentation_rect: Some(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D {
                    width: 400,
                    height: 480,
                },
            }),
        }
    }

    fn graph_presentation(
        source_window: isize,
        creation_order: u64,
        window_order: usize,
        sources: &[tracking::PresentSource],
    ) -> GraphPresentation {
        GraphPresentation {
            domain: 0,
            source_window,
            presentation_extent: vk::Extent2D {
                width: 400,
                height: 480,
            },
            creation_order,
            window_order,
            last_present: 100,
            sources: sources.to_vec(),
        }
    }

    #[test]
    fn presentation_export_preserves_srgb_bytes_as_unorm() {
        assert!(
            presentation_export_format(vk::Format::R8G8B8A8_SRGB) == vk::Format::R8G8B8A8_UNORM
        );
        assert!(
            presentation_export_format(vk::Format::B8G8R8A8_SRGB) == vk::Format::B8G8R8A8_UNORM
        );
        assert!(
            presentation_export_format(vk::Format::R16G16B16A16_SFLOAT)
                == vk::Format::R16G16B16A16_SFLOAT
        );
    }

    #[test]
    fn direct_tracking_excludes_surfaces_beyond_the_requested_slots() {
        assert!(direct_surface_selected(true, Some(0)));
        assert!(!direct_surface_selected(true, None));
        assert!(direct_surface_selected(false, None));
    }

    #[test]
    fn removed_first_source_leaves_the_second_requested_slot_unchanged() {
        let capture = CaptureState::default();
        let mut history = polymer_presentation::history::ResourceHistory::default();
        history.reconcile(
            vec![graph_source(1, 400, 240), graph_source(2, 320, 240)],
            true,
            |_| true,
        );
        let reconciled = history.reconcile(Vec::new(), false, |source| source.image.as_raw() != 1);
        let sources = reconciled
            .surfaces
            .into_iter()
            .zip(reconciled.slots)
            .map(|(mut source, slot)| {
                source.logical_slot = Some(slot);
                source
            })
            .collect::<Vec<_>>();
        let (requests, targets) = capture.presentation_targets(
            0,
            10,
            vk::Extent2D {
                width: 800,
                height: 600,
            },
            &sources,
            &[0, 1],
            1,
        );
        assert_eq!(requests, [(1, 0)]);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].source_window, 10);
    }

    #[test]
    fn capture_states_are_isolated() {
        let first = CaptureState::default();
        let second = CaptureState::default();
        let surface = vk::SurfaceKHR::from_raw(1);

        first.surface_created(surface, 42);

        assert_eq!(
            first.surface_windows.get(&surface).map(|entry| *entry),
            Some(42)
        );
        assert!(!second.surface_windows.contains_key(&surface));
    }

    #[test]
    fn presentation_window_slot_survives_until_its_last_surface_is_destroyed() {
        let capture = CaptureState::default();
        let first = vk::SurfaceKHR::from_raw(1);
        let second = vk::SurfaceKHR::from_raw(2);
        capture.surface_created(first, 42);
        capture.surface_created(second, 42);
        capture.swapchain_windows.lock().push(42);

        capture.surface_destroyed(first);
        assert!(capture.swapchain_windows.lock().contains(&42));

        capture.surface_destroyed(second);
        assert!(!capture.swapchain_windows.lock().contains(&42));
    }

    #[test]
    fn graph_surface_identity_does_not_depend_on_its_target_window() {
        assert_eq!(graph_surface_id(1), SurfaceId(0x5347_0000_0000_0001));
        assert_ne!(graph_surface_id(0), graph_surface_id(1));
        assert_ne!(graph_surface_id(1), stream_surface_id(20, 1));
    }

    #[test]
    fn shared_graph_targets_follow_surface_order_not_recreated_swapchain_order() {
        let capture = CaptureState::default();
        let sources = [graph_source(1, 240, 400), graph_source(2, 240, 320)];
        {
            let presentations = &capture.graph_presentations;
            // Azahar recreates the secondary swapchain before the primary one.
            presentations.insert(
                vk::SwapchainKHR::from_raw(30),
                graph_presentation(20, 3, 1, &sources),
            );
            presentations.insert(
                vk::SwapchainKHR::from_raw(40),
                graph_presentation(10, 4, 0, &sources),
            );
        }

        let (_, targets) = capture.presentation_targets(
            0,
            10,
            vk::Extent2D {
                width: 400,
                height: 480,
            },
            &sources,
            &[0, 1],
            100,
        );

        assert_eq!(targets[0].source_window, 10);
        assert_eq!(targets[1].source_window, 20);
    }

    #[test]
    fn graph_targets_match_transposed_source_aspects_to_separate_windows() {
        let capture = CaptureState::default();
        let sources = [graph_source(1, 240, 400), graph_source(2, 240, 320)];
        let mut bottom = graph_presentation(20, 1, 0, &sources);
        bottom.presentation_extent = vk::Extent2D {
            width: 320,
            height: 240,
        };
        let mut top = graph_presentation(10, 2, 1, &sources);
        top.presentation_extent = vk::Extent2D {
            width: 400,
            height: 240,
        };
        {
            let presentations = &capture.graph_presentations;
            // Deliberately put the bottom HWND first. Source 0 is still the
            // 400x240 top screen after its configured quarter turn.
            presentations.insert(vk::SwapchainKHR::from_raw(1), bottom);
            presentations.insert(vk::SwapchainKHR::from_raw(2), top);
        }

        let (_, targets) = capture.presentation_targets(
            0,
            10,
            vk::Extent2D {
                width: 400,
                height: 240,
            },
            &sources,
            &[0, 1],
            100,
        );

        assert_eq!(targets[0].source_window, 10);
        assert_eq!(targets[1].source_window, 20);
    }

    #[test]
    fn discovered_graph_target_mapping_is_not_overwritten_by_later_order_changes() {
        let capture = CaptureState::default();
        let sources = [graph_source(1, 240, 400), graph_source(2, 240, 320)];
        {
            let presentations = &capture.graph_presentations;
            presentations.insert(
                vk::SwapchainKHR::from_raw(1),
                graph_presentation(10, 1, 0, &sources),
            );
            presentations.insert(
                vk::SwapchainKHR::from_raw(2),
                graph_presentation(20, 2, 1, &sources),
            );
        }
        let extent = vk::Extent2D {
            width: 400,
            height: 480,
        };
        let (_, initial) = capture.presentation_targets(0, 10, extent, &sources, &[0, 1], 100);
        assert_eq!(initial[0].source_window, 10);
        assert_eq!(initial[1].source_window, 20);

        {
            let presentations = &capture.graph_presentations;
            presentations.clear();
            presentations.insert(
                vk::SwapchainKHR::from_raw(3),
                graph_presentation(20, 3, 0, &sources),
            );
            presentations.insert(
                vk::SwapchainKHR::from_raw(4),
                graph_presentation(10, 4, 1, &sources),
            );
        }
        let (_, stable) = capture.presentation_targets(0, 10, extent, &sources, &[0, 1], 101);
        assert_eq!(stable[0].source_window, 10);
        assert_eq!(stable[1].source_window, 20);
    }

    #[test]
    fn shared_graph_targets_survive_selection_gaps_and_swapchain_replacement() {
        let capture = CaptureState::default();
        let sources = [graph_source(1, 240, 400), graph_source(2, 240, 320)];
        {
            let presentations = &capture.graph_presentations;
            presentations.insert(
                vk::SwapchainKHR::from_raw(1),
                graph_presentation(10, 1, 0, &sources),
            );
            presentations.insert(
                vk::SwapchainKHR::from_raw(2),
                graph_presentation(20, 2, 1, &sources),
            );
        }
        let extent = vk::Extent2D {
            width: 400,
            height: 480,
        };
        let (_, initial) = capture.presentation_targets(0, 10, extent, &sources, &[0, 1], 100);
        assert_eq!(initial[1].source_window, 20);

        // Destruction removes swapchain observations, but not the stable
        // source-layout association used by replacement swapchains.
        capture.graph_presentations.clear();
        let (_, during_gap) = capture.presentation_targets(0, 10, extent, &sources, &[0, 1], 101);
        assert_eq!(during_gap[0].source_window, 10);
        assert_eq!(during_gap[1].source_window, 20);

        let presentations = &capture.graph_presentations;
        presentations.insert(
            vk::SwapchainKHR::from_raw(3),
            graph_presentation(20, 3, 1, &sources),
        );
        presentations.insert(
            vk::SwapchainKHR::from_raw(4),
            graph_presentation(10, 4, 0, &sources),
        );
        let (_, recreated) = capture.presentation_targets(0, 10, extent, &sources, &[0, 1], 102);
        assert_eq!(recreated[0].source_window, 10);
        assert_eq!(recreated[1].source_window, 20);
    }

    #[test]
    fn graph_target_geometry_follows_window_resize() {
        let capture = CaptureState::default();
        let sources = [graph_source(1, 240, 400), graph_source(2, 240, 320)];
        {
            let presentations = &capture.graph_presentations;
            presentations.insert(
                vk::SwapchainKHR::from_raw(1),
                graph_presentation(10, 1, 0, &sources),
            );
            presentations.insert(
                vk::SwapchainKHR::from_raw(2),
                graph_presentation(20, 2, 1, &sources),
            );
        }
        let extent = vk::Extent2D {
            width: 400,
            height: 480,
        };
        let dimensions = |extent: vk::Extent2D| (extent.width, extent.height);
        let (_, initial) = capture.presentation_targets(0, 10, extent, &sources, &[0, 1], 100);
        assert_eq!(initial[1].source_window, 20);
        assert_eq!(dimensions(initial[1].presentation_extent), (400, 480));

        // The secondary window is resized: its swapchain is recreated with a
        // larger extent and its presents carry the new placement. The sticky
        // HWND association must keep following the live geometry.
        let resized_extent = vk::Extent2D {
            width: 800,
            height: 600,
        };
        let resized_rect = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: resized_extent,
        };
        let resized_sources = sources.map(|source| tracking::PresentSource {
            presentation_rect: Some(resized_rect),
            ..source
        });
        {
            let presentations = &capture.graph_presentations;
            presentations.clear();
            presentations.insert(
                vk::SwapchainKHR::from_raw(3),
                graph_presentation(10, 3, 0, &sources),
            );
            let mut resized = graph_presentation(20, 4, 1, &resized_sources);
            resized.presentation_extent = resized_extent;
            presentations.insert(vk::SwapchainKHR::from_raw(4), resized);
        }

        let (_, refreshed) = capture.presentation_targets(0, 10, extent, &sources, &[0, 1], 101);
        assert_eq!(refreshed[0].source_window, 10);
        assert_eq!(dimensions(refreshed[0].presentation_extent), (400, 480));
        assert_eq!(refreshed[1].source_window, 20);
        assert_eq!(dimensions(refreshed[1].presentation_extent), (800, 600));
        let rect = refreshed[1]
            .presentation_rect
            .expect("resized placement is carried through");
        assert_eq!((rect.offset.x, rect.offset.y), (0, 0));
        assert_eq!(dimensions(rect.extent), (800, 600));

        // The refreshed geometry is retained as the last-known value while
        // the window's presents drop out (a minimized window keeps routing
        // against its last live geometry).
        capture.graph_presentations.clear();
        let (_, idle) = capture.presentation_targets(0, 10, extent, &sources, &[0, 1], 102);
        assert_eq!(idle[1].source_window, 20);
        assert_eq!(dimensions(idle[1].presentation_extent), (800, 600));
    }
}

impl StreamResources {
    fn connected(&self) -> bool {
        self.ipc.as_ref().is_some_and(ProducerSession::connected)
    }
}
