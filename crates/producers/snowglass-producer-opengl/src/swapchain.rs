//! Per-context capture lifecycle and framebuffer presentation.

use crate::{
    interop::{
        GL_COLOR_ATTACHMENT0, GL_COLOR_BUFFER_BIT, GL_DRAW_FRAMEBUFFER,
        GL_DRAW_FRAMEBUFFER_BINDING, GL_NEAREST, GL_READ_FRAMEBUFFER, GL_READ_FRAMEBUFFER_BINDING,
        GL_SCISSOR_TEST, GL_TEXTURE_2D, GL_TEXTURE_2D_ARRAY, Hdc, current_context, glDisable,
        glEnable, glGetIntegerv, glIsEnabled,
        memory_object_win32::{ExternalImage, GL_DEVICE_LUID_EXT, GlFunctions},
        nv_dx, pbo,
    },
    timeline::Timeline,
    tracking::{DrawCandidate, QuadBounds, Tracking},
};
use parking_lot::Mutex;
use snowglass_hook::config;
use snowglass_ipc::{LocalSocket, Message, StreamReady};
use snowglass_platform_windows::{
    NativeWindow, ProcessId, duplicate_handle_into_process, performance_counter, window_title,
};
use snowglass_protocol::{
    AdapterLuid, ExternalImageKind, ExternalSyncKind, FrameReady, MAX_IMAGE_SLOTS,
    PROTOCOL_VERSION, PresentationTransform, StreamId, SurfaceId,
};
use std::{
    cmp::Reverse,
    collections::HashMap,
    ffi::c_void,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use windows::Win32::{
    Foundation::HANDLE,
    Graphics::{Direct3D12::ID3D12Device, Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM},
};

#[repr(C)]
#[derive(Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn WindowFromDC(hdc: Hdc) -> *mut c_void;
    fn GetClientRect(hwnd: *mut c_void, rect: *mut Rect) -> i32;
}

struct ImageSlot {
    image: ExternalImage,
    release_value: u64,
}

unsafe impl Send for ImageSlot {}

struct ExtCapture {
    width: u32,
    height: u32,
    window_width: u32,
    window_height: u32,
    functions: GlFunctions,
    framebuffer: u32,
    read_framebuffer: u32,
    slots: Vec<ImageSlot>,
    _device: ID3D12Device,
    ready: Timeline,
    consumer: Timeline,
    ipc: Option<LocalSocket>,
    adapter_luid: AdapterLuid,
    surface_id: SurfaceId,
    source_window: isize,
    bottom_up: bool,
    next_slot: usize,
    sync_value: u64,
    frame_number: u64,

    presents: u64,
    deferred_frames: u64,
}

unsafe impl Send for ExtCapture {}

enum Capture {
    Ext(ExtCapture),
    Nv(NvCapture),
    Pbo(PboCapture),
}

impl Capture {
    #[expect(clippy::too_many_arguments)]
    unsafe fn new(
        surface_id: SurfaceId,
        source_window: isize,
        width: u32,
        height: u32,
        window_width: u32,
        window_height: u32,
        bottom_up: bool,
    ) -> Result<Self, String> {
        if unsafe { GlFunctions::load() }.is_some() {
            return unsafe {
                ExtCapture::new(
                    surface_id,
                    source_window,
                    width,
                    height,
                    window_width,
                    window_height,
                    bottom_up,
                )
            }
            .map(Self::Ext);
        }
        if let Ok(capture) = unsafe {
            NvCapture::new(
                surface_id,
                source_window,
                width,
                height,
                window_width,
                window_height,
                bottom_up,
            )
        } {
            return Ok(Self::Nv(capture));
        }
        unsafe {
            PboCapture::new(
                surface_id,
                source_window,
                width,
                height,
                window_width,
                window_height,
                bottom_up,
            )
        }
        .map(Self::Pbo)
    }
    fn width(&self) -> u32 {
        match self {
            Self::Ext(v) => v.width,
            Self::Nv(v) => v.width,
            Self::Pbo(v) => v.width,
        }
    }
    fn height(&self) -> u32 {
        match self {
            Self::Ext(v) => v.height,
            Self::Nv(v) => v.height,
            Self::Pbo(v) => v.height,
        }
    }
    fn surface_id(&self) -> SurfaceId {
        match self {
            Self::Ext(v) => v.surface_id,
            Self::Nv(v) => v.surface_id,
            Self::Pbo(v) => v.surface_id,
        }
    }
    fn whole_window(&self) -> bool {
        match self {
            Self::Ext(v) => v.bottom_up,
            Self::Nv(v) => v.bottom_up,
            Self::Pbo(v) => v.whole_window,
        }
    }
    fn set_window_extent(&mut self, width: u32, height: u32) {
        match self {
            Self::Ext(v) => {
                v.window_width = width;
                v.window_height = height;
            }
            Self::Nv(v) => {
                v.window_width = width;
                v.window_height = height;
            }
            Self::Pbo(v) => {
                v.window_width = width;
                v.window_height = height;
            }
        }
    }
    fn tick(&mut self) {
        match self {
            Self::Ext(v) => v.presents += 1,
            Self::Nv(v) => v.presents += 1,
            Self::Pbo(v) => v.presents += 1,
        }
    }
    fn should_reconnect(&self) -> bool {
        match self {
            Self::Ext(v) => v.ipc.is_none() && v.presents.is_multiple_of(120),
            Self::Nv(v) => v.ipc.is_none() && v.presents.is_multiple_of(120),
            Self::Pbo(v) => v.ipc.is_none() && v.presents.is_multiple_of(120),
        }
    }
    fn reconnect(&mut self) {
        match self {
            Self::Ext(v) => v.reconnect(),
            Self::Nv(v) => v.reconnect(),
            Self::Pbo(v) => v.reconnect(),
        }
    }
    unsafe fn copy_frame(
        &mut self,
        stream: StreamId,
        source: Option<&DrawCandidate>,
    ) -> Result<(), String> {
        match self {
            Self::Ext(v) => unsafe { v.copy_frame(stream, source) },
            Self::Nv(v) => unsafe { v.copy_frame(stream, source) },
            Self::Pbo(v) => unsafe { v.copy_frame(stream, source) },
        }
    }
    fn defer(&mut self) -> u64 {
        match self {
            Self::Ext(v) => {
                v.deferred_frames = v.deferred_frames.wrapping_add(1);
                v.deferred_frames
            }
            Self::Nv(v) => {
                v.deferred_frames = v.deferred_frames.wrapping_add(1);
                v.deferred_frames
            }
            Self::Pbo(v) => {
                v.deferred_frames = v.deferred_frames.wrapping_add(1);
                v.deferred_frames
            }
        }
    }
}

struct NvCapture {
    width: u32,
    height: u32,
    window_width: u32,
    window_height: u32,
    framebuffer: u32,
    read_framebuffer: u32,
    transport: nv_dx::Ring,
    ipc: Option<LocalSocket>,
    surface_id: SurfaceId,
    source_window: isize,
    bottom_up: bool,
    next_slot: usize,
    frame_number: u64,
    presents: u64,
    deferred_frames: u64,
}

struct PboCapture {
    width: u32,
    height: u32,
    window_width: u32,
    window_height: u32,
    read_framebuffer: u32,
    bind_framebuffer: crate::interop::GlBindFramebuffer,
    framebuffer_texture_2d: crate::interop::GlFramebufferTexture2D,
    framebuffer_texture_layer: crate::interop::GlFramebufferTextureLayer,
    delete_framebuffers: crate::interop::GlDeleteFramebuffers,
    transport: pbo::Transport,
    ipc: Option<LocalSocket>,
    surface_id: SurfaceId,
    source_window: isize,
    whole_window: bool,
    frame_number: u64,
    presents: u64,
    deferred_frames: u64,
}

unsafe impl Send for PboCapture {}

unsafe impl Send for NvCapture {}

#[derive(Default)]
struct WindowCaptures {
    streams: Vec<Capture>,
    tracked_sources: Vec<DrawCandidate>,
    untracked_frames: u32,
    pending_layout: Vec<(u32, u32)>,
    pending_frames: u32,
    /// The real HWND this capture presents to; every stream routes input here.
    hwnd: isize,
    /// Position in HWND first-seen order, pairing same-extent sources across
    /// windows deterministically.
    window_order: usize,
    /// Swap-clock stamp of the last presentation. Idle windows drop out of
    /// the cross-window selection instead of pinning stale entries.
    last_swap: u64,
    /// Last known present-quad placement per global selection index
    /// (bottom-up window coordinates). Pairing can transiently fail (an
    /// overlay draw joins the present); the placement holds until the next
    /// successful pairing or a window resize.
    placements: HashMap<usize, QuadBounds>,
    placement_extent: (u32, u32),
}

static CAPTURES: OnceLock<Mutex<HashMap<(usize, usize), WindowCaptures>>> = OnceLock::new();

fn captures() -> &'static Mutex<HashMap<(usize, usize), WindowCaptures>> {
    CAPTURES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Global swap counter used as the staleness clock for the cross-window
/// selection.
static SWAP_CLOCK: AtomicU64 = AtomicU64::new(0);

/// Swaps (across all windows) a window may go without presenting before its
/// tracked sources leave the cross-window selection.
const SELECTION_HORIZON: u64 = 240;

/// Stable HWND order comes from first presentation rather than per-swap
/// state: two windows presenting same-extent screens need a deterministic
/// pairing that survives swap interleaving.
static WINDOW_ORDER: Mutex<Vec<isize>> = Mutex::new(Vec::new());

fn window_order(hwnd: isize) -> usize {
    let mut windows = WINDOW_ORDER.lock();
    if let Some(position) = windows.iter().position(|window| *window == hwnd) {
        position
    } else {
        windows.push(hwnd);
        windows.len() - 1
    }
}

fn tracked_window_slot(hwnd: isize) -> Option<usize> {
    if !config::tracking_enabled() {
        return None;
    }
    let windows = WINDOW_ORDER.lock().clone();
    config::tracked_render_target_surface_slots(&windows, |window| {
        window_title(NativeWindow(window))
    })
    .iter()
    .position(|window| *window == Some(hwnd))
}

/// Graph selections are indexed independently of the window that presents
/// them, exactly like the Vulkan producer. Keeping this identity stable
/// preserves viewer ordering and CLI rotation across windows and swaps.
fn graph_surface_id(index: usize) -> SurfaceId {
    const GRAPH_SURFACE_TAG: u64 = 0x5347_0000_0000_0000;
    SurfaceId(GRAPH_SURFACE_TAG | index as u64)
}

fn surface_is_selected(
    allowed: &HashMap<isize, Vec<SurfaceId>>,
    window: isize,
    surface: SurfaceId,
) -> bool {
    allowed
        .get(&window)
        .is_some_and(|surfaces| surfaces.contains(&surface))
}

/// One tracked source's deterministic identity in the cross-window selection:
/// tallest/widest first, then window creation order, then the window's own
/// selection order. Sorting these tuples yields the global index space that
/// `--track-render-targets` selects from.
type SelectionIdentity = (Reverse<u32>, Reverse<u32>, usize, usize);

fn selection_identity(source: &DrawCandidate, order: usize, index: usize) -> SelectionIdentity {
    (Reverse(source.height), Reverse(source.width), order, index)
}

fn resolve_surface_selection(
    requested: &[usize],
    selection: &[(SelectionIdentity, isize)],
    mut surface_matches: impl FnMut(usize, isize) -> Option<bool>,
) -> Vec<(usize, SelectionIdentity)> {
    let mut resolved = vec![None; requested.len()];
    let mut used = Vec::new();
    for (slot, &requested_index) in requested.iter().enumerate() {
        if surface_matches(slot, 0).is_none() {
            continue;
        }
        let Some(selected) = selection
            .iter()
            .filter(|(_, hwnd)| surface_matches(slot, *hwnd) == Some(true))
            .nth(requested_index)
        else {
            continue;
        };
        resolved[slot] = Some(selected.0);
        used.push(selected.0);
    }
    for (slot, &requested_index) in requested.iter().enumerate() {
        if resolved[slot].is_some() || surface_matches(slot, 0).is_some() {
            continue;
        }
        let selected = selection
            .get(requested_index)
            .filter(|(identity, _)| !used.contains(identity))
            .or_else(|| {
                selection
                    .iter()
                    .find(|(identity, _)| !used.contains(identity))
            });
        if let Some(&(identity, _)) = selected {
            resolved[slot] = Some(identity);
            used.push(identity);
        }
    }
    resolved
        .into_iter()
        .enumerate()
        .filter_map(|(slot, identity)| identity.map(|identity| (slot, identity)))
        .collect()
}

/// Pairs the present-draw quads observed this frame with the selected
/// sources. When the frame drew exactly as many quads as sources were
/// selected, selection order pairs with draw order — page-flipped faces hop
/// between textures frame to frame, so texture identity is only consulted
/// when the counts cannot pair, and then only when a source's texture was
/// drawn by exactly one quad. Returns one placement per selected source.
fn pair_present_quads(
    selected: &[(usize, DrawCandidate)],
    quads: &[(u32, QuadBounds)],
) -> Vec<Option<QuadBounds>> {
    if !quads.is_empty() && quads.len() == selected.len() {
        return quads.iter().map(|&(_, quad)| Some(quad)).collect();
    }
    selected
        .iter()
        .map(|(_, source)| {
            let mut matching = quads
                .iter()
                .filter(|(texture, _)| *texture == source.texture);
            match (matching.next(), matching.next()) {
                (Some(&(_, quad)), None) => Some(quad),
                _ => None,
            }
        })
        .collect()
}

/// Called immediately before the real GDI `SwapBuffers`.
pub unsafe fn before_swap_buffers(tracking: &Tracking, hdc: isize) {
    let context = unsafe { current_context() };
    if context.is_null() {
        return;
    }
    let hdc = hdc as Hdc;
    let hwnd = unsafe { WindowFromDC(hdc) };
    if hwnd.is_null() {
        return;
    }
    let mut rect = Rect::default();
    if unsafe { GetClientRect(hwnd, &mut rect) } == 0 {
        return;
    }
    let window_width = (rect.right - rect.left).max(0) as u32;
    let window_height = (rect.bottom - rect.top).max(0) as u32;
    if window_width == 0 || window_height == 0 {
        return;
    }
    let (observed, trusted) = tracking.take_draw_candidates(context as usize);
    let present_quads = tracking.take_present_quads(context as usize);
    let key = (hdc as usize, context as usize);
    let now = SWAP_CLOCK.fetch_add(1, Ordering::Relaxed) + 1;
    let order = window_order(hwnd as isize);
    let mut map = captures().lock();
    {
        let entry = map.entry(key).or_default();
        // A trusted graph selection is complete: padding it with slots from an
        // earlier frame would pin stale page-flip faces alongside live ones.
        // Held slots are re-validated first: a renderer scale switch orphans
        // the old screen textures, and stale slots would otherwise block the
        // new-extent selection from being adopted while the capture keeps
        // blitting ids that now hold other content.
        let tracked = if trusted {
            observed
        } else {
            let refreshed = tracking.refresh_sources(&entry.tracked_sources);
            // A dropped held source means its texture was reallocated or
            // deleted — a live renderer-scale switch. The incremental graph
            // carries stale edges from the old resolution and re-resolves only
            // one screen; resetting lets the next frames rebuild both from
            // scratch, exactly as a fresh launch does.
            if refreshed.len() < entry.tracked_sources.len() {
                tracing::debug!(
                    "snowglass: OpenGL render targets reallocated; resetting tracking graph"
                );
                tracking.reset_graph();
            }
            merge_tracked_sources(
                &refreshed,
                observed,
                config::tracked_render_target_count().unwrap_or(1),
            )
        };
        entry.tracked_sources = tracked;
        entry.hwnd = hwnd as isize;
        entry.window_order = order;
        entry.last_swap = now;
    }
    // The requested indices select from the deterministic cross-window
    // selection — the union of every live window's tracked sources, tallest/
    // widest first — so `--track-render-targets 0,1` means the same screens
    // whether an application presents them in one window or several. Each
    // window then streams only the selected sources it presents itself,
    // which makes the stream's input HWND simply the presenting window.
    let mut selection = map
        .values()
        .filter(|captures| captures.last_swap.saturating_add(SELECTION_HORIZON) >= now)
        .flat_map(|captures| {
            captures
                .tracked_sources
                .iter()
                .enumerate()
                .map(|(index, source)| {
                    (
                        selection_identity(source, captures.window_order, index),
                        captures.hwnd,
                    )
                })
        })
        .collect::<Vec<_>>();
    selection.sort_unstable_by_key(|&(identity, _)| identity);
    let titles = selection
        .iter()
        .filter_map(|&(_, hwnd)| window_title(NativeWindow(hwnd)).map(|title| (hwnd, title)))
        .collect::<HashMap<_, _>>();
    let requested = config::tracked_render_target_indices().unwrap_or_default();
    let resolved = resolve_surface_selection(&requested, &selection, |slot, hwnd| {
        if !config::tracked_render_target_has_surface_pattern(slot) {
            return None;
        }
        if hwnd == 0 {
            return Some(false);
        }
        let title = titles.get(&hwnd)?;
        config::tracked_render_target_surface_matches(slot, title)
    });
    let tracked_slot = tracked_window_slot(hwnd as isize);
    if config::tracking_enabled() {
        let mut allowed = HashMap::<isize, Vec<SurfaceId>>::new();
        if resolved.is_empty() {
            let windows = WINDOW_ORDER.lock().clone();
            for (slot, window) in config::tracked_render_target_surface_slots(&windows, |window| {
                window_title(NativeWindow(window))
            })
            .into_iter()
            .enumerate()
            {
                if let Some(window) = window {
                    allowed
                        .entry(window)
                        .or_default()
                        .push(graph_surface_id(slot));
                }
            }
        } else {
            for &(slot, identity) in &resolved {
                if let Some(&(_, window)) = selection
                    .iter()
                    .find(|&&(candidate, _)| candidate == identity)
                {
                    allowed
                        .entry(window)
                        .or_default()
                        .push(graph_surface_id(slot));
                }
            }
        }
        for captures in map.values_mut() {
            let window = captures.hwnd;
            captures
                .streams
                .retain(|capture| surface_is_selected(&allowed, window, capture.surface_id()));
        }
    }
    let entry = map
        .get_mut(&key)
        .expect("window captures entry inserted above");
    let mut chosen = entry
        .tracked_sources
        .iter()
        .enumerate()
        .filter_map(|(index, source)| {
            let identity = selection_identity(source, order, index);
            let slot = resolved
                .iter()
                .find_map(|&(slot, selected)| (selected == identity).then_some(slot))?;
            Some((slot, *source))
        })
        .collect::<Vec<_>>();
    chosen.sort_unstable_by_key(|&(slot, _)| slot);
    // Present-draw quads carry the real window placement (screen gaps,
    // asymmetric layouts). Successful pairings are remembered per selection
    // index so a transiently unpairable frame (an overlay joins the present)
    // keeps the last real placement; a resize invalidates them.
    if entry.placement_extent != (window_width, window_height) {
        entry.placements.clear();
        entry.placement_extent = (window_width, window_height);
    }
    for (pairing, &(slot, _)) in pair_present_quads(&chosen, &present_quads)
        .into_iter()
        .zip(&chosen)
    {
        if let Some(quad) = pairing
            && entry.placements.insert(slot, quad) != Some(quad)
        {
            tracing::debug!(
                "snowglass: present placement {slot} -> {},{} {}x{} (bottom-up, window {window_width}x{window_height})",
                quad.x,
                quad.y,
                quad.width,
                quad.height
            );
        }
    }
    for (slot, source) in &mut chosen {
        if let Some(&quad) = entry.placements.get(slot) {
            source.bounds = quad;
            source.window_bounds = true;
        }
    }
    // One stream per selected source, or the classic whole-window stream
    // when nothing resolves. Tracked layouts ride out short selection gaps
    // and hold off layout changes until they persist, rather than churning
    // through transport rebuilds on scene transitions.
    let window_extent = (window_width, window_height);
    let window_surface = tracked_slot
        .map(graph_surface_id)
        .unwrap_or(SurfaceId(hwnd as u64));
    let current = entry
        .streams
        .iter()
        .map(|capture| (capture.width(), capture.height()))
        .collect::<Vec<_>>();
    let tracked_layout = entry.streams.iter().any(|capture| !capture.whole_window());
    let proposed = chosen
        .iter()
        .map(|&(slot, source)| (source.width, source.height, graph_surface_id(slot)))
        .collect::<Vec<_>>();
    let proposed_extents = proposed
        .iter()
        .map(|&(width, height, _)| (width, height))
        .collect::<Vec<_>>();
    // A held layout keeps the existing stream identities so selection gaps
    // and dampened changes never rebuild transports.
    let held = entry
        .streams
        .iter()
        .map(|capture| (capture.width(), capture.height(), capture.surface_id()))
        .collect::<Vec<_>>();
    let desired = if !proposed.is_empty() {
        entry.untracked_frames = 0;
        if proposed_extents == current || !tracked_layout {
            entry.pending_frames = 0;
            proposed
        } else {
            if entry.pending_layout == proposed_extents {
                entry.pending_frames += 1;
            } else {
                entry.pending_layout = proposed_extents;
                entry.pending_frames = 1;
            }
            if entry.pending_frames >= 60 {
                entry.pending_frames = 0;
                proposed
            } else {
                held
            }
        }
    } else if tracked_layout && entry.untracked_frames < 120 {
        entry.untracked_frames += 1;
        held
    } else if config::tracking_enabled() && (!resolved.is_empty() || tracked_slot.is_none()) {
        entry.untracked_frames = 0;
        Vec::new()
    } else {
        entry.untracked_frames = 0;
        vec![(window_width, window_height, window_surface)]
    };
    if desired
        .iter()
        .any(|&(width, height, _)| width == 0 || height == 0)
    {
        return;
    }
    let desired_extents = desired
        .iter()
        .map(|&(width, height, _)| (width, height))
        .collect::<Vec<_>>();
    if desired_extents != current && !chosen.is_empty() {
        tracing::debug!(
            "snowglass: selected OpenGL render targets {}",
            chosen
                .iter()
                .map(|(slot, source)| format!(
                    "{slot}: texture {} layer {} ({:#x}, {}x{})",
                    source.texture, source.layer, source.target, source.width, source.height
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    while entry.streams.len() > desired.len() {
        entry.streams.pop();
    }
    for (index, &(width, height, surface)) in desired.iter().enumerate() {
        let up_to_date = entry.streams.get(index).is_some_and(|capture| {
            (capture.width(), capture.height(), capture.surface_id()) == (width, height, surface)
        });
        if up_to_date {
            // Window geometry travels per frame; a resize must not rebuild
            // the transport, only refresh the presentation metadata.
            let capture = &mut entry.streams[index];
            capture.set_window_extent(window_width, window_height);
            continue;
        }
        // The whole-window fallback reads the bottom-up default framebuffer;
        // tracked textures keep guest memory order and pass through as-is.
        let bottom_up = chosen.is_empty() && (width, height) == window_extent;
        match unsafe {
            Capture::new(
                surface,
                hwnd as isize,
                width,
                height,
                window_width,
                window_height,
                bottom_up,
            )
        } {
            Ok(capture) => {
                if index < entry.streams.len() {
                    entry.streams[index] = capture;
                } else {
                    entry.streams.push(capture);
                }
            }
            Err(error) => {
                tracing::debug!("snowglass: OpenGL capture creation failed: {error}");
                return;
            }
        }
    }
    // While a layout change is being dampened the source list can differ
    // from the stream list; feed each stream the source matching its extent
    // and let unmatched streams idle a frame.
    let mut source_used = vec![false; chosen.len()];
    for (index, capture) in entry.streams.iter_mut().enumerate() {
        capture.tick();
        // A stream can be rejected while the viewer's surfaces are
        // transiently full; retry the connection periodically.
        if capture.should_reconnect() {
            capture.reconnect();
        }
        let source = chosen
            .iter()
            .enumerate()
            .find_map(|(source_index, (_, source))| {
                (!source_used[source_index]
                    && (source.width, source.height) == (capture.width(), capture.height()))
                    .then_some(source_index)
            });
        if let Some(source_index) = source {
            source_used[source_index] = true;
        }
        let source = source.map(|source_index| &chosen[source_index].1);
        // During a selection gap a tracked stream has no source this frame;
        // the whole-window fallback only makes sense at window extent.
        if source.is_none() && (capture.width(), capture.height()) != window_extent {
            continue;
        }
        let stream_id = StreamId((key.1 as u64) ^ ((index as u64) << 56));
        if let Err(error) = unsafe { capture.copy_frame(stream_id, source) } {
            let deferred = capture.defer();
            if deferred == 1 || deferred % 600 == 0 {
                tracing::debug!(
                    "snowglass: OpenGL capture deferred: {error} (count={})",
                    deferred
                );
            }
        }
    }
}

impl NvCapture {
    #[expect(clippy::too_many_arguments)]
    unsafe fn new(
        surface_id: SurfaceId,
        source_window: isize,
        width: u32,
        height: u32,
        window_width: u32,
        window_height: u32,
        bottom_up: bool,
    ) -> Result<Self, String> {
        let transport = unsafe { nv_dx::Ring::new(width, height) }?;
        let mut framebuffer = 0;
        let mut read_framebuffer = 0;
        unsafe {
            (transport.functions.gen_framebuffers)(1, &mut framebuffer);
            (transport.functions.gen_framebuffers)(1, &mut read_framebuffer);
        }
        let ipc = connect_nv_viewer(
            &transport,
            surface_id,
            source_window,
            width,
            height,
            bottom_up,
        )
        .map_err(|error| {
            tracing::debug!("snowglass: OpenGL NV viewer connection failed: {error}");
            error
        })
        .ok();
        tracing::debug!(
            "snowglass: attached WGL_NV_DX_interop ring {width}x{height} (surface {:#x})",
            surface_id.0
        );
        Ok(Self {
            width,
            height,
            window_width,
            window_height,
            framebuffer,
            read_framebuffer,
            transport,
            ipc,
            surface_id,
            source_window,
            bottom_up,
            next_slot: 0,
            frame_number: 0,
            presents: 0,
            deferred_frames: 0,
        })
    }

    fn reconnect(&mut self) {
        if let Ok(socket) = connect_nv_viewer(
            &self.transport,
            self.surface_id,
            self.source_window,
            self.width,
            self.height,
            self.bottom_up,
        ) {
            self.ipc = Some(socket);
        }
    }

    unsafe fn copy_frame(
        &mut self,
        stream_id: StreamId,
        source: Option<&DrawCandidate>,
    ) -> Result<(), String> {
        let count = self.transport.slots.len();
        let mut acquired = None;
        for offset in 0..count {
            let index = (self.next_slot + offset) % count;
            if unsafe { self.transport.acquire(index) }.is_ok() {
                acquired = Some(index);
                break;
            }
        }
        let slot_index = acquired.ok_or("all NV interop slots are awaiting the viewer")?;
        let slot = &self.transport.slots[slot_index];
        let f = self.transport.functions;
        let mut old_read = 0;
        let mut old_draw = 0;
        let scissor_enabled = unsafe { glIsEnabled(GL_SCISSOR_TEST) } != 0;
        unsafe {
            glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &mut old_read);
            glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut old_draw);
            (f.bind_framebuffer)(
                GL_READ_FRAMEBUFFER,
                if source.is_some() {
                    self.read_framebuffer
                } else {
                    0
                },
            );
            (f.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, self.framebuffer);
            (f.framebuffer_texture_2d)(
                GL_DRAW_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                slot.texture,
                0,
            );
            if scissor_enabled {
                glDisable(GL_SCISSOR_TEST);
            }
            if let Some(source) = source {
                if source.target == GL_TEXTURE_2D_ARRAY {
                    (f.framebuffer_texture_layer)(
                        GL_READ_FRAMEBUFFER,
                        GL_COLOR_ATTACHMENT0,
                        source.texture,
                        0,
                        source.layer,
                    );
                } else {
                    (f.framebuffer_texture_2d)(
                        GL_READ_FRAMEBUFFER,
                        GL_COLOR_ATTACHMENT0,
                        source.target,
                        source.texture,
                        0,
                    );
                }
                (f.blit_framebuffer)(
                    0,
                    0,
                    source.width as i32,
                    source.height as i32,
                    0,
                    0,
                    source.width as i32,
                    self.height as i32,
                    GL_COLOR_BUFFER_BIT,
                    GL_NEAREST,
                );
            } else {
                (f.blit_framebuffer)(
                    0,
                    0,
                    self.width as i32,
                    self.height as i32,
                    0,
                    0,
                    self.width as i32,
                    self.height as i32,
                    GL_COLOR_BUFFER_BIT,
                    GL_NEAREST,
                );
            }
            if scissor_enabled {
                glEnable(GL_SCISSOR_TEST);
            }
            (f.bind_framebuffer)(GL_READ_FRAMEBUFFER, old_read as u32);
            (f.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, old_draw as u32);
        }
        unsafe { self.transport.release(slot_index) }?;
        self.next_slot = (slot_index + 1) % count;
        self.frame_number = self.frame_number.wrapping_add(1);
        let bounds = frame_bounds(source, self.window_width, self.window_height);
        let message = Message::FrameReady(FrameReady {
            stream_id,
            slot: slot_index as u32,
            render_width: self.width,
            render_height: self.height,
            capture_x: bounds.x,
            capture_y: bounds.y,
            capture_width: bounds.width,
            capture_height: bounds.height,
            presentation_width: self.window_width,
            presentation_height: self.window_height,
            presentation_transform: PresentationTransform::axis_aligned(
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height,
            ),
            frame_number: self.frame_number,
            sync_value: self.frame_number,
            timestamp_qpc: performance_counter().unwrap_or(0),
        });
        if self
            .ipc
            .as_mut()
            .is_some_and(|ipc| ipc.send(message).is_err())
        {
            self.ipc = None;
        }
        Ok(())
    }
}

impl PboCapture {
    #[expect(clippy::too_many_arguments)]
    unsafe fn new(
        surface_id: SurfaceId,
        source_window: isize,
        width: u32,
        height: u32,
        window_width: u32,
        window_height: u32,
        whole_window: bool,
    ) -> Result<Self, String> {
        let transport = unsafe { pbo::Transport::new(width, height) }?;
        let gen_framebuffers: crate::interop::GlGenFramebuffers =
            unsafe { crate::interop::proc(b"glGenFramebuffers\0") }
                .ok_or("missing glGenFramebuffers")?;
        let bind_framebuffer = unsafe { crate::interop::proc(b"glBindFramebuffer\0") }
            .ok_or("missing glBindFramebuffer")?;
        let framebuffer_texture_2d = unsafe { crate::interop::proc(b"glFramebufferTexture2D\0") }
            .ok_or("missing glFramebufferTexture2D")?;
        let framebuffer_texture_layer =
            unsafe { crate::interop::proc(b"glFramebufferTextureLayer\0") }
                .ok_or("missing glFramebufferTextureLayer")?;
        let delete_framebuffers = unsafe { crate::interop::proc(b"glDeleteFramebuffers\0") }
            .ok_or("missing glDeleteFramebuffers")?;
        let mut read_framebuffer = 0;
        unsafe { gen_framebuffers(1, &mut read_framebuffer) };
        let ipc = connect_pbo_viewer(&transport, surface_id, source_window, width, height)
            .map_err(|error| {
                tracing::debug!("snowglass: OpenGL PBO viewer connection failed: {error}");
                error
            })
            .ok();
        tracing::debug!(
            "snowglass: attached asynchronous OpenGL PBO ring {width}x{height} (surface {:#x})",
            surface_id.0
        );
        Ok(Self {
            width,
            height,
            window_width,
            window_height,
            read_framebuffer,
            bind_framebuffer,
            framebuffer_texture_2d,
            framebuffer_texture_layer,
            delete_framebuffers,
            transport,
            ipc,
            surface_id,
            source_window,
            whole_window,
            frame_number: 0,
            presents: 0,
            deferred_frames: 0,
        })
    }

    fn reconnect(&mut self) {
        if let Ok(socket) = connect_pbo_viewer(
            &self.transport,
            self.surface_id,
            self.source_window,
            self.width,
            self.height,
        ) {
            self.ipc = Some(socket);
        }
    }

    unsafe fn copy_frame(
        &mut self,
        stream_id: StreamId,
        source: Option<&DrawCandidate>,
    ) -> Result<(), String> {
        let mut old_read = 0;
        unsafe {
            glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &mut old_read);
            (self.bind_framebuffer)(
                GL_READ_FRAMEBUFFER,
                if source.is_some() {
                    self.read_framebuffer
                } else {
                    0
                },
            );
            if let Some(source) = source {
                if source.target == GL_TEXTURE_2D_ARRAY {
                    (self.framebuffer_texture_layer)(
                        GL_READ_FRAMEBUFFER,
                        GL_COLOR_ATTACHMENT0,
                        source.texture,
                        0,
                        source.layer,
                    );
                } else {
                    (self.framebuffer_texture_2d)(
                        GL_READ_FRAMEBUFFER,
                        GL_COLOR_ATTACHMENT0,
                        source.target,
                        source.texture,
                        0,
                    );
                }
            }
        }
        let result = unsafe { self.transport.transfer() };
        unsafe { (self.bind_framebuffer)(GL_READ_FRAMEBUFFER, old_read as u32) };
        let Some(slot_index) = result? else {
            return Ok(());
        };
        self.frame_number = self.frame_number.wrapping_add(1);
        let bounds = frame_bounds(source, self.window_width, self.window_height);
        let message = Message::FrameReady(FrameReady {
            stream_id,
            slot: slot_index as u32,
            render_width: self.width,
            render_height: self.height,
            capture_x: bounds.x,
            capture_y: bounds.y,
            capture_width: bounds.width,
            capture_height: bounds.height,
            presentation_width: self.window_width,
            presentation_height: self.window_height,
            presentation_transform: PresentationTransform::axis_aligned(
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height,
            ),
            frame_number: self.frame_number,
            sync_value: self.frame_number,
            timestamp_qpc: performance_counter().unwrap_or(0),
        });
        if self
            .ipc
            .as_mut()
            .is_some_and(|ipc| ipc.send(message).is_err())
        {
            self.ipc = None;
        }
        Ok(())
    }
}

impl Drop for PboCapture {
    fn drop(&mut self) {
        unsafe { (self.delete_framebuffers)(1, &self.read_framebuffer) }
    }
}

impl Drop for NvCapture {
    fn drop(&mut self) {
        unsafe {
            (self.transport.functions.delete_framebuffers)(1, &self.framebuffer);
            (self.transport.functions.delete_framebuffers)(1, &self.read_framebuffer);
        }
    }
}

fn frame_bounds(
    source: Option<&DrawCandidate>,
    window_width: u32,
    window_height: u32,
) -> QuadBounds {
    match source {
        Some(value) if value.window_bounds => QuadBounds {
            x: value.bounds.x,
            y: window_height.saturating_sub(value.bounds.y.saturating_add(value.bounds.height)),
            width: value.bounds.width,
            height: value.bounds.height,
        },
        _ => QuadBounds {
            x: 0,
            y: 0,
            width: window_width,
            height: window_height,
        },
    }
}

impl ExtCapture {
    unsafe fn new(
        surface_id: SurfaceId,
        source_window: isize,
        width: u32,
        height: u32,
        window_width: u32,
        window_height: u32,
        bottom_up: bool,
    ) -> Result<Self, String> {
        let functions = unsafe { GlFunctions::load() }
            .ok_or("GL_EXT_memory_object_win32 and GL_EXT_semaphore_win32 are unavailable")?;
        let mut luid_bytes = [0u8; 8];
        unsafe { (functions.get_unsigned_byte)(GL_DEVICE_LUID_EXT, luid_bytes.as_mut_ptr()) };
        let adapter_luid = AdapterLuid {
            low_part: u32::from_le_bytes(luid_bytes[..4].try_into().expect("four bytes")),
            high_part: i32::from_le_bytes(luid_bytes[4..].try_into().expect("four bytes")),
        };
        let (device, adapter) = unsafe { ExternalImage::create_device(adapter_luid) }?;
        let adapter_desc = unsafe { adapter.GetDesc1() }
            .map_err(|error| format!("IDXGIAdapter1::GetDesc1: {error}"))?;
        let adapter_luid = AdapterLuid {
            low_part: adapter_desc.AdapterLuid.LowPart,
            high_part: adapter_desc.AdapterLuid.HighPart,
        };
        let mut framebuffer = 0;
        let mut read_framebuffer = 0;
        unsafe {
            (functions.gen_framebuffers)(1, &mut framebuffer);
            (functions.gen_framebuffers)(1, &mut read_framebuffer);
        }
        let mut slots = Vec::with_capacity(3);
        for _ in 0..3 {
            slots.push(ImageSlot {
                image: unsafe { ExternalImage::new(&device, &functions, width, height) }?,
                release_value: 0,
            });
        }
        let ready = unsafe { Timeline::new(&device, &functions) }?;
        let consumer = unsafe { Timeline::new(&device, &functions) }?;
        let ipc = match connect_viewer(
            &slots,
            ready.handle(),
            consumer.handle(),
            adapter_luid,
            surface_id,
            source_window,
            width,
            height,
            bottom_up,
        ) {
            Ok(socket) => Some(socket),
            Err(error) => {
                tracing::debug!("snowglass: OpenGL viewer connection failed: {error}");
                None
            }
        };
        tracing::debug!(
            "snowglass: attached OpenGL EXT external-object ring {width}x{height} (surface {:#x}, window {source_window:#x} {window_width}x{window_height})",
            surface_id.0
        );
        Ok(Self {
            width,
            height,
            window_width,
            window_height,
            functions,
            framebuffer,
            read_framebuffer,
            slots,
            _device: device,
            ready,
            consumer,
            ipc,
            adapter_luid,
            surface_id,
            source_window,
            bottom_up,
            next_slot: 0,
            sync_value: 0,
            frame_number: 0,
            presents: 0,
            deferred_frames: 0,
        })
    }

    /// Re-publishes this stream over a fresh viewer connection after a
    /// rejection or disconnect.
    fn reconnect(&mut self) {
        if let Ok(socket) = connect_viewer(
            &self.slots,
            self.ready.handle(),
            self.consumer.handle(),
            self.adapter_luid,
            self.surface_id,
            self.source_window,
            self.width,
            self.height,
            self.bottom_up,
        ) {
            self.ipc = Some(socket);
        }
    }

    unsafe fn copy_frame(
        &mut self,
        stream_id: StreamId,
        source: Option<&DrawCandidate>,
    ) -> Result<(), String> {
        let completed = unsafe { self.consumer.completed_value() };
        let Some(slot_index) = (0..self.slots.len())
            .map(|offset| (self.next_slot + offset) % self.slots.len())
            .find(|&index| {
                let release = self.slots[index].release_value;
                release == 0 || release <= completed
            })
        else {
            return Err("all external image slots are awaiting the viewer".into());
        };
        let slot = &mut self.slots[slot_index];
        let mut old_read = 0;
        let mut old_draw = 0;
        let scissor_enabled = unsafe { glIsEnabled(GL_SCISSOR_TEST) } != 0;
        unsafe {
            glGetIntegerv(GL_READ_FRAMEBUFFER_BINDING, &mut old_read);
            glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut old_draw);
            if source.is_some() {
                (self.functions.bind_framebuffer)(GL_READ_FRAMEBUFFER, self.read_framebuffer);
            } else {
                (self.functions.bind_framebuffer)(GL_READ_FRAMEBUFFER, 0);
            }
            (self.functions.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, self.framebuffer);
            (self.functions.framebuffer_texture_2d)(
                GL_DRAW_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                slot.image.texture,
                0,
            );
            if scissor_enabled {
                glDisable(GL_SCISSOR_TEST);
            }

            match source {
                None => {
                    (self.functions.blit_framebuffer)(
                        0,
                        0,
                        self.width as i32,
                        self.height as i32,
                        0,
                        0,
                        self.width as i32,
                        self.height as i32,
                        GL_COLOR_BUFFER_BIT,
                        GL_NEAREST,
                    );
                }
                Some(source) => {
                    if source.target == GL_TEXTURE_2D_ARRAY {
                        (self.functions.framebuffer_texture_layer)(
                            GL_READ_FRAMEBUFFER,
                            GL_COLOR_ATTACHMENT0,
                            source.texture,
                            0,
                            source.layer,
                        );
                    } else {
                        (self.functions.framebuffer_texture_2d)(
                            GL_READ_FRAMEBUFFER,
                            GL_COLOR_ATTACHMENT0,
                            source.target,
                            source.texture,
                            0,
                        );
                    }
                    (self.functions.blit_framebuffer)(
                        0,
                        0,
                        source.width as i32,
                        source.height as i32,
                        0,
                        0,
                        source.width as i32,
                        self.height as i32,
                        GL_COLOR_BUFFER_BIT,
                        GL_NEAREST,
                    );
                }
            }
            if scissor_enabled {
                glEnable(GL_SCISSOR_TEST);
            }
            (self.functions.bind_framebuffer)(GL_READ_FRAMEBUFFER, old_read as u32);
            (self.functions.bind_framebuffer)(GL_DRAW_FRAMEBUFFER, old_draw as u32);
        }
        let sync_value = self.sync_value.wrapping_add(1).max(1);
        unsafe {
            self.ready
                .signal(&self.functions, slot.image.texture, sync_value)
        };
        slot.release_value = sync_value;
        self.sync_value = sync_value;
        self.next_slot = (slot_index + 1) % self.slots.len();
        self.frame_number = self.frame_number.wrapping_add(1);
        // Where the content lands in the window's client area, top-down.
        // Only bounds known to be window coordinates — present-quad
        // placements and default-framebuffer rasters — are used; sources
        // observed on offscreen draws claim the whole window instead, and
        // the viewer infers the placement (stacked same-window layout or
        // the centered letterbox the compositors themselves use).
        let bounds = match source {
            Some(value) if value.window_bounds => QuadBounds {
                x: value.bounds.x,
                y: self
                    .window_height
                    .saturating_sub(value.bounds.y.saturating_add(value.bounds.height)),
                width: value.bounds.width,
                height: value.bounds.height,
            },
            _ => QuadBounds {
                x: 0,
                y: 0,
                width: self.window_width,
                height: self.window_height,
            },
        };
        let message = Message::FrameReady(FrameReady {
            stream_id,
            slot: slot_index as u32,
            render_width: self.width,
            render_height: self.height,
            capture_x: bounds.x,
            capture_y: bounds.y,
            capture_width: bounds.width,
            capture_height: bounds.height,
            presentation_width: self.window_width,
            presentation_height: self.window_height,
            presentation_transform: PresentationTransform::axis_aligned(
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height,
            ),
            frame_number: self.frame_number,
            sync_value,
            timestamp_qpc: performance_counter().unwrap_or(0),
        });
        if self.frame_number <= self.slots.len() as u64 || self.frame_number % 120 == 0 {
            tracing::debug!(
                "snowglass: submitted OpenGL EXT frame {} tracked={} at {}x{} into slot {} (quad {},{} {}x{})",
                self.frame_number,
                source.is_some(),
                self.width,
                self.height,
                slot_index,
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height
            );
        }
        if self
            .ipc
            .as_mut()
            .is_some_and(|ipc| ipc.send(message).is_err())
        {
            self.ipc = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interop::GL_TEXTURE_2D;

    fn source(texture: u32, width: u32, height: u32) -> DrawCandidate {
        DrawCandidate {
            texture,
            target: GL_TEXTURE_2D,
            layer: 0,
            width,
            height,
            bounds: QuadBounds {
                x: 0,
                y: 0,
                width,
                height,
            },
            window_bounds: false,
            score: 0,
        }
    }

    #[test]
    fn graph_surface_identity_matches_the_vulkan_producer() {
        assert_eq!(graph_surface_id(0), SurfaceId(0x5347_0000_0000_0000));
        assert_eq!(graph_surface_id(1), SurfaceId(0x5347_0000_0000_0001));
    }

    #[test]
    fn cross_window_selection_orders_tallest_first_regardless_of_window_arrival() {
        // The bottom-screen window (240x320) was seen first, but the taller
        // top screen must still take global index 0.
        let mut selection = vec![
            selection_identity(&source(1, 240, 320), 0, 0),
            selection_identity(&source(2, 240, 400), 1, 0),
        ];
        selection.sort_unstable();
        assert_eq!(
            selection
                .iter()
                .position(|&item| item == selection_identity(&source(2, 240, 400), 1, 0)),
            Some(0)
        );
        assert_eq!(
            selection
                .iter()
                .position(|&item| item == selection_identity(&source(1, 240, 320), 0, 0)),
            Some(1)
        );
    }

    #[test]
    fn same_extent_screens_pair_by_window_creation_order() {
        // Two windows each present one 256x192 screen: the first-created
        // window keeps global index 0 no matter how swaps interleave.
        let mut selection = vec![
            selection_identity(&source(2, 256, 192), 1, 0),
            selection_identity(&source(1, 256, 192), 0, 0),
        ];
        selection.sort_unstable();
        assert_eq!(selection[0], selection_identity(&source(1, 256, 192), 0, 0));
        assert_eq!(selection[1], selection_identity(&source(2, 256, 192), 1, 0));
    }

    #[test]
    fn surface_match_pins_a_slot_and_unqualified_slots_take_remaining_sources() {
        let main = selection_identity(&source(1, 1280, 720), 0, 0);
        let gamepad = selection_identity(&source(2, 854, 480), 1, 0);
        let mut selection = vec![(main, 10), (gamepad, 20)];
        selection.sort_unstable_by_key(|&(identity, _)| identity);
        let resolved = resolve_surface_selection(&[0, 1], &selection, |slot, hwnd| {
            (slot == 0).then_some(hwnd == 20)
        });
        assert_eq!(resolved[0], (0, gamepad));
        assert_eq!(resolved[1], (1, main));
    }

    #[test]
    fn one_request_resolves_only_one_cross_window_source() {
        let main = selection_identity(&source(1, 1280, 720), 0, 0);
        let gamepad = selection_identity(&source(2, 854, 480), 1, 0);
        let mut selection = vec![(main, 10), (gamepad, 20)];
        selection.sort_unstable_by_key(|&(identity, _)| identity);

        let resolved = resolve_surface_selection(&[0], &selection, |_, _| None);

        assert_eq!(resolved, [(0, main)]);
    }

    #[test]
    fn one_allowed_opengl_surface_excludes_sibling_rings() {
        let allowed = HashMap::from([(10, vec![graph_surface_id(0)])]);

        assert!(surface_is_selected(&allowed, 10, graph_surface_id(0)));
        assert!(!surface_is_selected(&allowed, 20, graph_surface_id(0)));
        assert!(!surface_is_selected(&allowed, 10, graph_surface_id(1)));
    }

    fn quad(y: u32) -> QuadBounds {
        QuadBounds {
            x: 26,
            y,
            width: 312,
            height: 233,
        }
    }

    #[test]
    fn present_quads_pair_with_selection_in_draw_order_when_counts_match() {
        // Page-flipped faces hop between textures frame to frame (both quads
        // sample the current face), so equal counts pair by draw order.
        let selected = [(0, source(12, 256, 192)), (1, source(11, 256, 192))];
        let quads = [(11, quad(312)), (11, quad(0))];
        assert_eq!(
            pair_present_quads(&selected, &quads),
            vec![Some(quad(312)), Some(quad(0))]
        );
    }

    #[test]
    fn unpairable_present_quads_match_by_unique_texture_only() {
        let selected = [(0, source(12, 256, 192)), (1, source(11, 256, 192))];
        // An overlay draw joined the present: counts cannot pair, and only
        // the source drawn by exactly one quad resolves.
        let quads = [(11, quad(312)), (12, quad(0)), (12, quad(100))];
        assert_eq!(
            pair_present_quads(&selected, &quads),
            vec![None, Some(quad(312))]
        );
        assert_eq!(pair_present_quads(&selected, &[]), vec![None, None]);
    }

    #[test]
    fn single_window_selection_preserves_its_own_order_for_equal_extents() {
        // A cohort of same-extent screens in one window keeps the window's
        // selection order as the global order.
        let mut selection = vec![
            selection_identity(&source(1, 256, 192), 0, 0),
            selection_identity(&source(2, 256, 192), 0, 1),
        ];
        selection.sort_unstable();
        assert_eq!(selection[0].3, 0);
        assert_eq!(selection[1].3, 1);
    }
}

fn merge_tracked_sources(
    previous: &[DrawCandidate],
    observed: Vec<DrawCandidate>,
    limit: usize,
) -> Vec<DrawCandidate> {
    if previous.is_empty() || observed.len() >= limit {
        return observed;
    }
    if observed.is_empty() {
        return previous.to_vec();
    }
    // A partial observation updates the matching slots and keeps the rest;
    // independently tracked screens may differ in extent, so unmatched
    // previous entries must survive rather than being filtered to one size.
    let mut merged = previous.to_vec();
    let mut updated = vec![false; merged.len()];
    for candidate in observed {
        let exact = merged.iter().position(|old| {
            (old.texture, old.target, old.layer)
                == (candidate.texture, candidate.target, candidate.layer)
        });
        let compatible = merged.iter().enumerate().position(|(index, old)| {
            !updated[index]
                && old.target == candidate.target
                && old.layer == candidate.layer
                && (old.width, old.height) == (candidate.width, candidate.height)
        });
        if let Some(index) = exact.or(compatible) {
            merged[index] = candidate;
            updated[index] = true;
        } else if merged.len() < limit {
            merged.push(candidate);
            updated.push(true);
        }
    }
    merged.truncate(limit);
    merged
}

impl Drop for ExtCapture {
    fn drop(&mut self) {
        unsafe {
            (self.functions.delete_framebuffers)(1, &self.framebuffer);
            (self.functions.delete_framebuffers)(1, &self.read_framebuffer);
            self.ready.destroy(&self.functions);
            self.consumer.destroy(&self.functions);
        }
    }
}

fn connect_nv_viewer(
    transport: &nv_dx::Ring,
    surface_id: SurfaceId,
    source_window: isize,
    width: u32,
    height: u32,
    bottom_up: bool,
) -> std::io::Result<LocalSocket> {
    let session = config::session()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?;
    let mut socket = LocalSocket::connect(&session)?;
    let viewer_pid = match socket.receive()? {
        Message::Hello(hello) if hello.protocol_version == PROTOCOL_VERSION => hello.process_id,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "incompatible viewer",
            ));
        }
    };
    socket.enable_frame_channel(viewer_pid)?;
    let mut image_handles = [0i64; MAX_IMAGE_SLOTS];
    for (destination, slot) in image_handles.iter_mut().zip(transport.ring.slots()) {
        *destination =
            duplicate_handle_into_process(slot.raw_handle() as isize, ProcessId(viewer_pid))?
                as i64;
    }
    socket.send(Message::StreamReady(StreamReady {
        producer_process_id: std::process::id(),
        surface_id,
        adapter_luid: transport.adapter_luid,
        source_window: source_window as i64,
        width,
        height,
        format: DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32,
        image_kind: ExternalImageKind::D3d11SharedResourceNt,
        sync_kind: ExternalSyncKind::D3d11KeyedMutex,
        slot_count: transport.ring.slots().len() as u32,
        image_handles,
        timeline_semaphore_handle: 0,
        consumer_semaphore_handle: 0,
        bottom_up,
    }))?;
    match socket.receive()? {
        Message::StreamAccepted => Ok(socket),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "viewer rejected stream",
        )),
    }
}

fn connect_pbo_viewer(
    transport: &pbo::Transport,
    surface_id: SurfaceId,
    source_window: isize,
    width: u32,
    height: u32,
) -> std::io::Result<LocalSocket> {
    let session = config::session()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?;
    let mut socket = LocalSocket::connect(&session)?;
    let viewer_pid = match socket.receive()? {
        Message::Hello(hello) if hello.protocol_version == PROTOCOL_VERSION => hello.process_id,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "incompatible viewer",
            ));
        }
    };
    socket.enable_frame_channel(viewer_pid)?;
    let mut image_handles = [0i64; MAX_IMAGE_SLOTS];
    for (destination, slot) in image_handles.iter_mut().zip(transport.ring.slots()) {
        *destination =
            duplicate_handle_into_process(slot.raw_handle() as isize, ProcessId(viewer_pid))?
                as i64;
    }
    socket.send(Message::StreamReady(StreamReady {
        producer_process_id: std::process::id(),
        surface_id,
        adapter_luid: transport.adapter_luid,
        source_window: source_window as i64,
        width,
        height,
        format: DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32,
        image_kind: ExternalImageKind::D3d11SharedResourceNt,
        sync_kind: ExternalSyncKind::D3d11KeyedMutex,
        slot_count: transport.ring.slots().len() as u32,
        image_handles,
        timeline_semaphore_handle: 0,
        consumer_semaphore_handle: 0,
        // glReadPixels starts at the framebuffer's bottom row.
        bottom_up: true,
    }))?;
    match socket.receive()? {
        Message::StreamAccepted => Ok(socket),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "viewer rejected stream",
        )),
    }
}

#[expect(clippy::too_many_arguments)]
fn connect_viewer(
    slots: &[ImageSlot],
    ready_handle: HANDLE,
    consumer_handle: HANDLE,
    adapter_luid: AdapterLuid,
    surface_id: SurfaceId,
    source_window: isize,
    width: u32,
    height: u32,
    bottom_up: bool,
) -> std::io::Result<LocalSocket> {
    let session = snowglass_hook::config::session()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?;
    let mut socket = LocalSocket::connect(&session)?;
    let viewer_pid = match socket.receive()? {
        Message::Hello(hello) if hello.protocol_version == PROTOCOL_VERSION => hello.process_id,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "viewer did not send compatible Hello",
            ));
        }
    };
    socket.enable_frame_channel(viewer_pid)?;
    let mut image_handles = [0i64; MAX_IMAGE_SLOTS];
    for (destination, slot) in image_handles.iter_mut().zip(slots) {
        *destination =
            duplicate_handle_into_process(slot.image.handle.0 as isize, ProcessId(viewer_pid))?
                as i64;
    }
    socket.send(Message::StreamReady(StreamReady {
        producer_process_id: std::process::id(),
        surface_id,
        adapter_luid,
        source_window: source_window as i64,
        width,
        height,
        format: DXGI_FORMAT_R8G8B8A8_UNORM.0 as u32,
        image_kind: ExternalImageKind::OpenGlD3d12Resource,
        sync_kind: ExternalSyncKind::D3d12Fence,
        slot_count: slots.len() as u32,
        image_handles,
        timeline_semaphore_handle: duplicate_handle_into_process(
            ready_handle.0 as isize,
            ProcessId(viewer_pid),
        )? as i64,
        consumer_semaphore_handle: duplicate_handle_into_process(
            consumer_handle.0 as isize,
            ProcessId(viewer_pid),
        )? as i64,
        bottom_up,
    }))?;
    // The viewer acknowledges only accepted streams; a rejection closes the
    // socket instead. Without waiting for the ack, a rejected stream looks
    // alive forever (a stalled ring stops sending the frames that would
    // surface the dead socket) and reconnect retries never trigger.
    match socket.receive()? {
        Message::StreamAccepted => Ok(socket),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "viewer did not acknowledge the stream",
        )),
    }
}
