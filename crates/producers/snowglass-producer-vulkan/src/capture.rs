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
use parking_lot::Mutex;
use snowglass_hook::config;
use snowglass_ipc::{LocalSocket, Message, StreamReady};
use snowglass_platform_windows::{
    NativeWindow, ProcessId, duplicate_handle_into_process, performance_counter, window_title,
};
use snowglass_protocol::{
    AdapterLuid, ConsumerApi, ExternalImageKind, ExternalSyncKind, FrameReady, MAX_IMAGE_SLOTS,
    PROTOCOL_VERSION, PresentationTransform, StreamId, SurfaceId,
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
    surface_id: SurfaceId,
    target_window: isize,
    presentation_extent: vk::Extent2D,
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
    fn from_transport(
        extent: vk::Extent2D,
        surface_id: SurfaceId,
        target_window: isize,
        presentation_extent: vk::Extent2D,
        transport: Transport,
    ) -> Self {
        Self {
            extent,
            surface_id,
            target_window,
            presentation_extent,
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
    /// The sources captured by the last successful graph selection, for
    /// bridging frames whose presentation roots cannot be resolved.
    tracked_sources: Vec<tracking::PresentSource>,
    /// Global present count when this swapchain last presented. A window
    /// that stops presenting (a splash screen left alive but idle) must not
    /// keep holding viewer surfaces with a frozen stream.
    last_present: u64,
}

#[derive(Clone)]
struct GraphPresentation {
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
    copies_submitted: AtomicU64,
    /// Which swapchain streams tracked render targets. An application with
    /// several windows resolves the same screens on every one of them;
    /// without a single owner each window would publish redundant streams.
    tracked_owner: Mutex<Option<(vk::SwapchainKHR, u64)>>,
    /// Latest graph resolution for each live swapchain. When several windows
    /// expose the same multi-source graph, source order is paired with stable
    /// window creation order so input can return to the corresponding window
    /// even though only one swapchain owns the exported copies.
    graph_presentations: Mutex<HashMap<vk::SwapchainKHR, GraphPresentation>>,
    /// Stable presentation targets by graph-source extent. This cache
    /// deliberately outlives swapchains: resize/recreation and short graph
    /// gaps or source-list reordering must not send a stream to another HWND.
    graph_target_layouts: Mutex<HashMap<(u32, u32), PresentationTarget>>,
    /// Stable HWND order comes from Vulkan surface creation rather than
    /// swapchain creation: applications can recreate sibling swapchains in a
    /// different order during resize or startup transitions.
    window_order: Mutex<Vec<isize>>,
    /// HWNDs that actually acquired swapchains. Some toolkits create Vulkan
    /// surfaces for non-presenting controls; those must not consume a regex
    /// tracking slot ahead of the real render windows.
    swapchain_windows: Mutex<Vec<isize>>,
}
const CLAIM_HORIZON: u64 = 240;

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
            log(format!(
                "snowglass: graph presentation {swapchain:?} window {:#x}: [{sources}]",
                state.source_window
            ));
        }
        let mut presentations = self.graph_presentations.lock();
        if sources.is_empty() {
            presentations.remove(&swapchain);
        } else {
            presentations.insert(
                swapchain,
                GraphPresentation {
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

    fn presentation_targets(
        &self,
        owner_window: isize,
        owner_extent: vk::Extent2D,
        sources: &[tracking::PresentSource],
        requested_indices: &[usize],
        now: u64,
    ) -> (Vec<(usize, usize)>, Vec<PresentationTarget>) {
        let fallback = |index: usize| PresentationTarget {
            source_window: owner_window,
            presentation_extent: owner_extent,
            presentation_rect: sources
                .get(index)
                .and_then(|source| source.presentation_rect),
        };
        let same_layout = |candidate: &GraphPresentation| {
            let mut candidate_extents = candidate
                .sources
                .iter()
                .map(|source| (source.extent.width, source.extent.height))
                .collect::<Vec<_>>();
            let mut source_extents = sources
                .iter()
                .map(|source| (source.extent.width, source.extent.height))
                .collect::<Vec<_>>();
            candidate_extents.sort_unstable();
            source_extents.sort_unstable();
            candidate_extents == source_extents
        };
        let presentations = self.graph_presentations.lock();
        let mut by_window = HashMap::<isize, GraphPresentation>::new();
        for candidate in presentations.values().filter(|candidate| {
            candidate.source_window != 0
                && now.saturating_sub(candidate.last_present) <= CLAIM_HORIZON
                && same_layout(candidate)
        }) {
            match by_window.entry(candidate.source_window) {
                std::collections::hash_map::Entry::Occupied(mut entry)
                    if entry.get().creation_order < candidate.creation_order =>
                {
                    entry.insert(candidate.clone());
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(candidate.clone());
                }
                _ => {}
            }
        }
        let mut candidates = by_window.values().cloned().collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|candidate| candidate.window_order);
        let assignment = (!sources.is_empty() && candidates.len() >= sources.len())
            .then(|| assign_presentation_candidates(sources, &candidates));
        let discovered = assignment.as_ref().map(|assignment| {
            assignment
                .iter()
                .copied()
                .enumerate()
                .map(|(index, candidate_index)| {
                    let candidate = &candidates[candidate_index];
                    PresentationTarget {
                        source_window: candidate.source_window,
                        presentation_extent: candidate.presentation_extent,
                        presentation_rect: candidate_presentation_rect(
                            candidate,
                            sources[index].extent,
                        ),
                    }
                })
                .collect::<Vec<_>>()
        });
        drop(presentations);
        let mut layouts = self.graph_target_layouts.lock();
        if let Some(discovered) = discovered {
            for (source, target) in sources.iter().zip(discovered) {
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    layouts.entry((source.extent.width, source.extent.height))
                {
                    log(format!(
                        "snowglass: graph target layout {}x{} -> window {:#x} ({}x{})",
                        source.extent.width,
                        source.extent.height,
                        target.source_window,
                        target.presentation_extent.width,
                        target.presentation_extent.height,
                    ));
                    entry.insert(target);
                }
            }
        }

        let mut resolved = vec![None; requested_indices.len()];
        let mut used = Vec::new();
        if let Some(assignment) = &assignment {
            for slot in 0..requested_indices.len() {
                if !config::tracked_render_target_has_surface_pattern(slot) {
                    continue;
                }
                let Some(candidate_index) = candidates.iter().position(|candidate| {
                    window_title(NativeWindow(candidate.source_window)).is_some_and(|title| {
                        config::tracked_render_target_surface_matches(slot, &title) == Some(true)
                    })
                }) else {
                    continue;
                };
                let Some(source_index) = assignment
                    .iter()
                    .position(|&assigned| assigned == candidate_index)
                else {
                    continue;
                };
                resolved[slot] = Some((source_index, Some(candidate_index)));
                used.push(source_index);
            }
        }
        for (slot, &requested_index) in requested_indices.iter().enumerate() {
            if resolved[slot].is_some() || config::tracked_render_target_has_surface_pattern(slot) {
                continue;
            }
            let source_index = (requested_index < sources.len()
                && !used.contains(&requested_index))
            .then_some(requested_index)
            .or_else(|| (0..sources.len()).find(|index| !used.contains(index)));
            if let Some(source_index) = source_index {
                resolved[slot] = Some((source_index, None));
                used.push(source_index);
            }
        }
        let resolved = resolved
            .into_iter()
            .enumerate()
            .filter_map(|(slot, resolved)| {
                resolved.map(|(index, candidate)| (slot, index, candidate))
            })
            .collect::<Vec<_>>();
        let targets = resolved
            .iter()
            .map(|&(_, index, matched_candidate)| {
                if let Some(candidate_index) = matched_candidate {
                    let candidate = &candidates[candidate_index];
                    return PresentationTarget {
                        source_window: candidate.source_window,
                        presentation_extent: candidate.presentation_extent,
                        presentation_rect: candidate_presentation_rect(
                            candidate,
                            sources[index].extent,
                        ),
                    };
                }
                let Some(source) = sources.get(index) else {
                    return fallback(index);
                };
                let key = (source.extent.width, source.extent.height);
                let Some(cached) = layouts.get(&key).copied() else {
                    return fallback(index);
                };
                // Only the HWND is sticky; the window's geometry is not. A
                // resize recreates the swapchain with a new extent, and
                // coordinates sent against the discovery-time extent would
                // land on the old letterbox. Re-derive extent and placement
                // from the window's live presentation whenever one exists.
                let Some(live) = by_window.get(&cached.source_window) else {
                    return cached;
                };
                let refreshed = PresentationTarget {
                    source_window: cached.source_window,
                    presentation_extent: live.presentation_extent,
                    presentation_rect: live
                        .sources
                        .get(index)
                        .and_then(|source| source.presentation_rect),
                };
                layouts.insert(key, refreshed);
                refreshed
            })
            .collect();
        (
            resolved
                .into_iter()
                .map(|(slot, index, _)| (slot, index))
                .collect(),
            targets,
        )
    }

    fn claim_tracked_streaming(&self, swapchain: vk::SwapchainKHR, now: u64) -> bool {
        let mut owner = self.tracked_owner.lock();
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

    fn release_tracked_streaming(&self, swapchain: vk::SwapchainKHR) {
        let mut owner = self.tracked_owner.lock();
        if owner.is_some_and(|(incumbent, _)| incumbent == swapchain) {
            *owner = None;
        }
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
}

fn assign_presentation_candidates(
    sources: &[tracking::PresentSource],
    candidates: &[GraphPresentation],
) -> Vec<usize> {
    if sources.len() > 8 || candidates.len() > 16 {
        return (0..sources.len()).collect();
    }

    fn visit(
        source_index: usize,
        sources: &[tracking::PresentSource],
        candidates: &[GraphPresentation],
        used: u16,
        cost: u128,
        current: &mut Vec<usize>,
        best: &mut Option<(u128, Vec<usize>)>,
    ) {
        if source_index == sources.len() {
            if best.as_ref().is_none_or(|(best_cost, _)| cost < *best_cost) {
                *best = Some((cost, current.clone()));
            }
            return;
        }
        for (candidate_index, candidate) in candidates.iter().enumerate() {
            let bit = 1u16 << candidate_index;
            if used & bit != 0 {
                continue;
            }
            let next_cost = cost.saturating_add(presentation_candidate_cost(
                sources[source_index].extent,
                candidate,
            ));
            if best
                .as_ref()
                .is_some_and(|(best_cost, _)| next_cost >= *best_cost)
            {
                continue;
            }
            current.push(candidate_index);
            visit(
                source_index + 1,
                sources,
                candidates,
                used | bit,
                next_cost,
                current,
                best,
            );
            current.pop();
        }
    }

    let mut best = None;
    visit(
        0,
        sources,
        candidates,
        0,
        0,
        &mut Vec::with_capacity(sources.len()),
        &mut best,
    );
    best.map_or_else(
        || (0..sources.len()).collect(),
        |(_, assignment)| assignment,
    )
}

fn presentation_candidate_cost(source: vk::Extent2D, candidate: &GraphPresentation) -> u128 {
    let target = candidate.presentation_extent;
    let direct = (u128::from(source.width) * u128::from(target.height))
        .abs_diff(u128::from(source.height) * u128::from(target.width));
    let transposed = (u128::from(source.height) * u128::from(target.height))
        .abs_diff(u128::from(source.width) * u128::from(target.width));
    let aspect_error = direct.min(transposed);
    let presentation_rect = candidate_presentation_rect(candidate, source);
    let missing_placement = u128::from(presentation_rect.is_none());
    let target_area = u128::from(target.width) * u128::from(target.height);
    let covered_area = presentation_rect.map_or(0, |rect| {
        u128::from(rect.extent.width) * u128::from(rect.extent.height)
    });
    let uncovered_area = target_area.saturating_sub(covered_area.min(target_area));
    (missing_placement << 120) | (aspect_error << 48) | uncovered_area.min((1u128 << 48) - 1)
}

fn candidate_presentation_rect(
    candidate: &GraphPresentation,
    source_extent: vk::Extent2D,
) -> Option<vk::Rect2D> {
    candidate
        .sources
        .iter()
        .find(|source| source.extent == source_extent)
        .and_then(|source| source.presentation_rect)
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
    const GRAPH_SURFACE_TAG: u64 = 0x5347_0000_0000_0000;
    SurfaceId(GRAPH_SURFACE_TAG | index as u64)
}

fn is_graph_surface_id(surface_id: SurfaceId) -> bool {
    const GRAPH_SURFACE_TAG: u64 = 0x5347_0000_0000_0000;
    const GRAPH_SURFACE_TAG_MASK: u64 = 0xffff_0000_0000_0000;
    surface_id.0 & GRAPH_SURFACE_TAG_MASK == GRAPH_SURFACE_TAG
}

fn requires_selected_source(
    surface_id: SurfaceId,
    stream_extent: vk::Extent2D,
    swapchain_extent: vk::Extent2D,
    graph_tracking: bool,
) -> bool {
    stream_extent != swapchain_extent || (graph_tracking && is_graph_surface_id(surface_id))
}

fn direct_surface_selected(tracking_enabled: bool, tracked_slot: Option<usize>) -> bool {
    !tracking_enabled || tracked_slot.is_some()
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
            if let Err(error) = socket.enable_frame_channel(hello.process_id) {
                log(format!("snowglass: shared frame channel failed: {error}"));
                return None;
            }
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
    surface_id: SurfaceId,
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
        bottom_up: false,
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
    surface_id: SurfaceId,
    source_window: isize,
) -> Transport {
    let format = presentation_export_format(format);
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
                    "snowglass: attached {}-slot export ring {}x{} (format={}) {:?}",
                    ring.slots().len(),
                    extent.width,
                    extent.height,
                    format.as_raw(),
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
                    surface_id,
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

impl CaptureState {
    pub fn surface_created(&self, surface: vk::SurfaceKHR, source_window: isize) {
        self.surface_windows.insert(surface, source_window);
        let mut windows = self.window_order.lock();
        if source_window != 0 && !windows.contains(&source_window) {
            windows.push(source_window);
        }
        drop(windows);
        let title = window_title(NativeWindow(source_window)).unwrap_or_default();
        log(format!(
            "snowglass: associated Vulkan surface with HWND {source_window:#x} title {title:?}"
        ));
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
            self.swapchain_windows
                .lock()
                .retain(|window| *window != source_window);
        }
    }

    pub fn device_destroyed(&self, device: vk::Device) {
        self.swapchains.retain(|_, state| state.device != device);
        let mut presentations = self.graph_presentations.lock();
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
            .then(|| {
                let transport = capture.as_deref().map_or(
                    Transport {
                        ring: None,
                        producer_timeline: None,
                        consumer_timeline: None,
                        ipc: None,
                    },
                    |capture| {
                        create_transport(Some(capture), extent, format, surface_id, source_window)
                    },
                );
                CaptureStream::from_transport(extent, surface_id, source_window, extent, transport)
            })
            .into_iter()
            .collect();
        self.swapchains.insert(
            swapchain,
            SwapchainState {
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
                untracked_presents: 0,
                pending_layout: Vec::new(),
                pending_presents: 0,
                tracked_sources: Vec::new(),
                last_present: self.presents_seen.load(Ordering::Relaxed),
            },
        );
        log(format!(
            "snowglass: intercepted vkCreateSwapchainKHR (count={count}, window {source_window:#x}, tracking slot {tracked_slot:?}, selected={selected})"
        ));
    }

    pub fn swapchain_destroyed(&self, swapchain: vk::SwapchainKHR) {
        if let Some(state) = self.swapchains.get(&swapchain) {
            for image in &state.images {
                self.swapchain_images.remove(image);
                state.tracking.swapchain_image_removed(*image);
            }
        }
        self.swapchains.remove(&swapchain);
        self.graph_presentations.lock().remove(&swapchain);
        // A freed claim lets a surviving window take over the tracked streams.
        self.release_tracked_streaming(swapchain);
    }

    pub fn swapchain_images(&self, swapchain: vk::SwapchainKHR, images: &[vk::Image]) {
        if let Some(mut state) = self.swapchains.get_mut(&swapchain) {
            state.images = images.to_vec();
            if config::tracking_enabled() {
                for image in &state.images {
                    self.swapchain_images.insert(*image, swapchain);
                    state.tracking.swapchain_image_added(*image, state.extent);
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
            log(format!(
                "snowglass: intercepted vkQueuePresentKHR (count={count})"
            ));
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
        let presented_image = state.images.get(image_index as usize).copied()?;
        let capture_rect = tracking.capture_rect(presented_image, state.extent);
        let rect_is_content = capture_rect.is_some_and(|rect| rect.extent != state.extent);

        // reset track on reallocatio
        let reallocated = tracking.sources_reallocated(&state.tracked_sources);
        let mut resolved = (config::tracking_enabled() && config::graph_tracking_enabled())
            .then(|| {
                tracking.take_present_sources(
                    presented_image,
                    config::tracked_render_target_count().unwrap_or(1),
                )
            })
            .unwrap_or_default();
        let previous = tracking.refresh_sources(&state.tracked_sources);
        if reallocated {
            log("snowglass: Vulkan render targets reallocated; resetting tracking graph");
            tracking.reset_graph();
        }
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
        let now = self.presents_seen.load(Ordering::Relaxed);
        self.observe_graph_presentation(swapchain, state, &resolved, now);
        let requested_indices = config::tracked_render_target_indices().unwrap_or_else(|| vec![0]);
        let (selected_requests, graph_targets) = self.presentation_targets(
            state.source_window,
            state.extent,
            &resolved,
            &requested_indices,
            now,
        );
        state.tracked_sources = resolved.clone();
        // Several windows resolve the same screens; the incumbent owner streams
        // them and the others stream nothing rather than publishing duplicates
        // that race for viewer surfaces.
        state.last_present = now;
        let claimed_elsewhere =
            !resolved.is_empty() && !self.claim_tracked_streaming(swapchain, now);
        if claimed_elsewhere {
            resolved.clear();
            state.tracked_sources.clear();
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
        let selected_surface = direct_surface_selected(
            config::tracking_enabled(),
            self.tracked_surface_slot(state.source_window),
        );
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
        } else if !selected_surface {
            state.untracked_presents = 0;
            Vec::new()
        } else {
            state.untracked_presents = 0;
            vec![state.extent]
        };
        // Holding a tracked layout — including through a selection gap — keeps
        // the process-wide ownership fresh, so a sibling window cannot steal the
        // streams during a scene transition.
        if desired.len() > 1
            || desired
                .first()
                .is_some_and(|extent| *extent != state.extent)
        {
            let _ = self.claim_tracked_streaming(swapchain, now);
        }
        while state.streams.len() > desired.len() {
            if let Some(mut stream) = state.streams.pop() {
                stream.retire();
            }
        }
        let format = state.format;
        let keep_tracked_metadata =
            graph_sources.is_empty() && tracked_layout && desired == current;
        for (index, extent) in desired.iter().copied().enumerate() {
            let proposed_target = graph_targets.get(index).copied().unwrap_or_else(|| {
                if !keep_tracked_metadata {
                    return PresentationTarget {
                        source_window: state.source_window,
                        presentation_extent: state.extent,
                        presentation_rect: None,
                    };
                }
                state.streams.get(index).map_or(
                    PresentationTarget {
                        source_window: state.source_window,
                        presentation_extent: state.extent,
                        presentation_rect: None,
                    },
                    |stream| PresentationTarget {
                        source_window: stream.target_window,
                        presentation_extent: stream.presentation_extent,
                        presentation_rect: None,
                    },
                )
            });
            let existing = state
                .streams
                .get(index)
                .filter(|stream| stream.extent == extent);
            // A partial graph resolution must not downgrade an established
            // cross-window association back to the owner HWND. The full graph
            // will refresh frame assignment when it returns; transport metadata
            // remains latched in the meantime.
            let target = existing
                .filter(|stream| {
                    is_graph_surface_id(stream.surface_id)
                        && stream.target_window != state.source_window
                        && proposed_target.source_window == state.source_window
                })
                .map_or(proposed_target, |stream| PresentationTarget {
                    source_window: stream.target_window,
                    presentation_extent: stream.presentation_extent,
                    presentation_rect: proposed_target.presentation_rect,
                });
            let proposed_surface_id = graph_request_slots.get(index).copied().map_or_else(
                || {
                    if keep_tracked_metadata {
                        state.streams.get(index).map_or_else(
                            || stream_surface_id(state.source_window, index),
                            |stream| stream.surface_id,
                        )
                    } else {
                        self.tracked_surface_slot(state.source_window).map_or_else(
                            || stream_surface_id(state.source_window, index),
                            graph_surface_id,
                        )
                    }
                },
                graph_surface_id,
            );
            let surface_id = existing
                .filter(|stream| is_graph_surface_id(stream.surface_id))
                .map_or(proposed_surface_id, |stream| stream.surface_id);
            if index >= state.streams.len() {
                log(format!(
                    "snowglass: stream {index} ({}x{}, surface {:#x}) targets window {:#x}",
                    extent.width, extent.height, surface_id.0, target.source_window
                ));
                let transport = create_transport(
                    state.capture.as_deref(),
                    extent,
                    format,
                    surface_id,
                    target.source_window,
                );
                state.streams.push(CaptureStream::from_transport(
                    extent,
                    surface_id,
                    target.source_window,
                    target.presentation_extent,
                    transport,
                ));
                continue;
            }
            let stream = &mut state.streams[index];
            if stream.extent != extent
                || stream.surface_id != surface_id
                || stream.target_window != target.source_window
            {
                log(format!(
                    "snowglass: stream {index} ({}x{}, surface {:#x}) retargets window {:#x} -> {:#x}",
                    extent.width,
                    extent.height,
                    surface_id.0,
                    stream.target_window,
                    target.source_window
                ));
                stream.retire();
                let transport = create_transport(
                    state.capture.as_deref(),
                    extent,
                    format,
                    surface_id,
                    target.source_window,
                );
                *stream = CaptureStream::from_transport(
                    extent,
                    surface_id,
                    target.source_window,
                    target.presentation_extent,
                    transport,
                );
            } else if stream.presentation_extent != target.presentation_extent {
                // A window resize changes only presentation geometry, which
                // travels per frame; rebuilding the transport would churn the
                // viewer connection for metadata.
                stream.presentation_extent = target.presentation_extent;
            }
        }
        let presentation_source =
            (config::tracking_enabled() && graph_sources.is_empty() && !rect_is_content)
                .then(|| tracking.render_target(presented_image))
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
                let found = graph_sources
                    .iter()
                    .enumerate()
                    .find_map(|(index, source)| {
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
                    ring.desc().format,
                    stream.surface_id,
                    stream.target_window,
                );
            }
            let Some(engine) = stream.copy_engine.as_mut() else {
                continue;
            };
            let assigned_source = assigned.get(index).copied().flatten();
            let source = assigned_source.map(|source| &graph_sources[source]);
            // During a selection gap a tracked stream has no source this frame;
            // the whole-frame fallbacks only make sense at swapchain extent.
            if source.is_none()
                && requires_selected_source(
                    stream.surface_id,
                    stream.extent,
                    swapchain_extent,
                    config::graph_tracking_enabled(),
                )
            {
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
                    Some(rect) => engine.submit_region_copy(
                        queue,
                        image_index,
                        rect.offset,
                        rect.extent,
                        &waits,
                    ),
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
                    let count = self.copies_submitted.fetch_add(1, Ordering::Relaxed) + 1;
                    stream.frame_number += 1;
                    let cropped =
                        source.is_none() && (rect_is_content || presentation_source.is_some());
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
                    let notification = Message::FrameReady(FrameReady {
                        stream_id: StreamId(swapchain.as_raw() ^ ((index as u64) << 56)),
                        slot: submission.slot,
                        render_width: render_extent.width,
                        render_height: render_extent.height,
                        capture_x: presentation_rect.map_or(0, |rect| rect.offset.x as u32),
                        capture_y: presentation_rect.map_or(0, |rect| rect.offset.y as u32),
                        capture_width: presentation_rect
                            .map_or(render_extent.width, |rect| rect.extent.width),
                        capture_height: presentation_rect
                            .map_or(render_extent.height, |rect| rect.extent.height),
                        presentation_width: stream.presentation_extent.width,
                        presentation_height: stream.presentation_extent.height,
                        presentation_transform: if presentation_source.is_some() {
                            PresentationTransform::UNAVAILABLE
                        } else if let Some(rect) = presentation_rect {
                            PresentationTransform::axis_aligned(
                                rect.offset.x as u32,
                                rect.offset.y as u32,
                                rect.extent.width,
                                rect.extent.height,
                            )
                        } else {
                            PresentationTransform::UNAVAILABLE
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
                        let placement = presentation_rect.map_or_else(
                            || "unavailable".to_owned(),
                            |rect| {
                                format!(
                                    "{},{} {}x{}",
                                    rect.offset.x,
                                    rect.offset.y,
                                    rect.extent.width,
                                    rect.extent.height,
                                )
                            },
                        );
                        log(format!(
                            "snowglass: submitted capture {}x{} (count={count}, stream={index}, tracked={}, placement={placement})",
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
        &self,
        tracking: &tracking::Tracking,
        queue: vk::Queue,
        queue_family_index: u32,
        command_buffers: &[vk::CommandBuffer],
    ) {
        if config::tracking_enabled() {
            return;
        }
        let mut selected = HashMap::<vk::SwapchainKHR, tracking::RenderTargetCandidate>::new();
        for command in command_buffers {
            let Some(candidate) = tracking.command_candidate(*command) else {
                continue;
            };
            let Some(swapchain) = self.swapchain_images.get(&candidate.0).map(|entry| *entry)
            else {
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
            let Some(mut state) = self.swapchains.get_mut(&swapchain) else {
                continue;
            };
            let images = state.images.clone();
            let capture = state.capture.clone();
            let presentation_extent = state.extent;
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
                        let count = self.copies_submitted.fetch_add(1, Ordering::Relaxed) + 1;
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
                            presentation_width: presentation_extent.width,
                            presentation_height: presentation_extent.height,
                            presentation_transform: PresentationTransform::UNAVAILABLE,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_source(image: u64, width: u32, height: u32) -> tracking::PresentSource {
        tracking::PresentSource {
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
    fn title_pinned_swapchain_does_not_require_a_graph_source() {
        let swapchain_extent = vk::Extent2D {
            width: 854,
            height: 480,
        };
        let surface_id = graph_surface_id(0);

        assert!(!requires_selected_source(
            surface_id,
            swapchain_extent,
            swapchain_extent,
            false,
        ));
        assert!(requires_selected_source(
            surface_id,
            swapchain_extent,
            swapchain_extent,
            true,
        ));
        assert!(requires_selected_source(
            surface_id,
            vk::Extent2D {
                width: 640,
                height: 360,
            },
            swapchain_extent,
            false,
        ));
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
        assert!(is_graph_surface_id(graph_surface_id(1)));
        assert!(!is_graph_surface_id(stream_surface_id(20, 1)));
    }

    #[test]
    fn shared_graph_targets_follow_surface_order_not_recreated_swapchain_order() {
        let capture = CaptureState::default();
        let sources = [graph_source(1, 240, 400), graph_source(2, 240, 320)];
        {
            let mut presentations = capture.graph_presentations.lock();
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
            let mut presentations = capture.graph_presentations.lock();
            // Deliberately put the bottom HWND first. Source 0 is still the
            // 400x240 top screen after its configured quarter turn.
            presentations.insert(vk::SwapchainKHR::from_raw(1), bottom);
            presentations.insert(vk::SwapchainKHR::from_raw(2), top);
        }

        let (_, targets) = capture.presentation_targets(
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
            let mut presentations = capture.graph_presentations.lock();
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
        let (_, initial) = capture.presentation_targets(10, extent, &sources, &[0, 1], 100);
        assert_eq!(initial[0].source_window, 10);
        assert_eq!(initial[1].source_window, 20);

        {
            let mut presentations = capture.graph_presentations.lock();
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
        let (_, stable) = capture.presentation_targets(10, extent, &sources, &[0, 1], 101);
        assert_eq!(stable[0].source_window, 10);
        assert_eq!(stable[1].source_window, 20);
    }

    #[test]
    fn shared_graph_targets_survive_selection_gaps_and_swapchain_replacement() {
        let capture = CaptureState::default();
        let sources = [graph_source(1, 240, 400), graph_source(2, 240, 320)];
        {
            let mut presentations = capture.graph_presentations.lock();
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
        let (_, initial) = capture.presentation_targets(10, extent, &sources, &[0, 1], 100);
        assert_eq!(initial[1].source_window, 20);

        // Destruction removes swapchain observations, but not the stable
        // source-layout association used by replacement swapchains.
        capture.graph_presentations.lock().clear();
        let (_, during_gap) = capture.presentation_targets(10, extent, &sources, &[0, 1], 101);
        assert_eq!(during_gap[0].source_window, 10);
        assert_eq!(during_gap[1].source_window, 20);

        let mut presentations = capture.graph_presentations.lock();
        presentations.insert(
            vk::SwapchainKHR::from_raw(3),
            graph_presentation(20, 3, 1, &sources),
        );
        presentations.insert(
            vk::SwapchainKHR::from_raw(4),
            graph_presentation(10, 4, 0, &sources),
        );
        drop(presentations);
        let (_, recreated) = capture.presentation_targets(10, extent, &sources, &[0, 1], 102);
        assert_eq!(recreated[0].source_window, 10);
        assert_eq!(recreated[1].source_window, 20);
    }

    #[test]
    fn graph_target_geometry_follows_window_resize() {
        let capture = CaptureState::default();
        let sources = [graph_source(1, 240, 400), graph_source(2, 240, 320)];
        {
            let mut presentations = capture.graph_presentations.lock();
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
        let (_, initial) = capture.presentation_targets(10, extent, &sources, &[0, 1], 100);
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
            let mut presentations = capture.graph_presentations.lock();
            presentations.clear();
            presentations.insert(
                vk::SwapchainKHR::from_raw(3),
                graph_presentation(10, 3, 0, &sources),
            );
            let mut resized = graph_presentation(20, 4, 1, &resized_sources);
            resized.presentation_extent = resized_extent;
            presentations.insert(vk::SwapchainKHR::from_raw(4), resized);
        }

        let (_, refreshed) = capture.presentation_targets(10, extent, &sources, &[0, 1], 101);
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
        capture.graph_presentations.lock().clear();
        let (_, idle) = capture.presentation_targets(10, extent, &sources, &[0, 1], 102);
        assert_eq!(idle[1].source_window, 20);
        assert_eq!(dimensions(idle[1].presentation_extent), (800, 600));
    }
}
