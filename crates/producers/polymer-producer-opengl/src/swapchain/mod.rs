mod context;
mod ext;
mod lifecycle;
mod nv;
mod presentation;
mod readback;
mod source_attachment;
mod storage;
use lifecycle::Capture;
use storage::CaptureStore;
// Per-context capture lifecycle and framebuffer presentation.

use crate::{
    interop::{
        GL_COLOR_ATTACHMENT0, GL_COLOR_BUFFER_BIT, GL_DRAW_FRAMEBUFFER,
        GL_DRAW_FRAMEBUFFER_BINDING, GL_NEAREST, GL_READ_FRAMEBUFFER, GL_READ_FRAMEBUFFER_BINDING,
        GL_SCISSOR_TEST, GL_TEXTURE_2D, Hdc, current_context, glDisable, glEnable, glGetIntegerv,
        glIsEnabled,
        memory_object_win32::{ExternalImage, GL_DEVICE_LUID_EXT, GlFunctions},
        nv_dx, pbo,
    },
    timeline::Timeline,
    tracking::{DrawCandidate, QuadBounds, Tracking},
};
use parking_lot::Mutex;
use polymer_hook::config;
use polymer_platform_windows::{NativeWindow, performance_counter, window_title};
use polymer_presentation::producer::ExportDescriptor;
use polymer_presentation::producer::ProducerSession;
use polymer_protocol::{AdapterLuid, ExternalImageKind, ExternalSyncKind, StreamId, SurfaceId};
use std::{
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
    release_value: AtomicU64,
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
    ipc: Option<ProducerSession>,
    scheduler: polymer_presentation::scheduling::SlotScheduler,
    sync_value: u64,
}

unsafe impl Send for ExtCapture {}

enum NativeCapture {
    Ext(Box<ExtCapture>),
    Nv(NvCapture),
    Pbo(PboCapture),
}

impl NativeCapture {
    #[expect(clippy::too_many_arguments)]
    unsafe fn new(
        surface_id: SurfaceId,
        source_window: isize,
        width: u32,
        height: u32,
        window_width: u32,
        window_height: u32,
        bottom_up: bool,
        connection: ProducerSession,
    ) -> Result<Self, String> {
        let mut connection = Some(connection);
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
                    &mut connection,
                )
            }
            .map(|capture| Self::Ext(Box::new(capture)));
        }
        let attempt = unsafe {
            NvCapture::new(
                surface_id,
                source_window,
                width,
                height,
                window_width,
                window_height,
                bottom_up,
                &mut connection,
            )
        };
        match attempt {
            Ok(capture) => return Ok(Self::Nv(capture)),
            Err(error) if connection.is_none() => return Err(error),
            Err(_) => {}
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
                &mut connection,
            )
        }
        .map(Self::Pbo)
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
    fn connected(&self) -> bool {
        match self {
            Self::Ext(v) => v.ipc.as_ref().is_some_and(ProducerSession::connected),
            Self::Nv(v) => v.ipc.as_ref().is_some_and(ProducerSession::connected),
            Self::Pbo(v) => v.ipc.as_ref().is_some_and(ProducerSession::connected),
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
    ipc: Option<ProducerSession>,
    scheduler: polymer_presentation::scheduling::SlotScheduler,
    frame_number: u64,
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
    framebuffer_renderbuffer: crate::interop::GlFramebufferRenderbuffer,
    delete_framebuffers: crate::interop::GlDeleteFramebuffers,
    transport: pbo::Transport,
    ipc: Option<ProducerSession>,
    frame_number: u64,
}

unsafe impl Send for PboCapture {}

unsafe impl Send for NvCapture {}

struct WindowCaptures {
    owner_id: u64,
    streams: Vec<Capture>,
    tracked_sources: Vec<DrawCandidate>,
    source_slots: Vec<usize>,
    history: polymer_presentation::history::ResourceHistory<DrawCandidate>,
    /// The real HWND this capture presents to; every stream routes input here.
    hwnd: isize,
    /// Swap-clock stamp of the last presentation. Idle windows drop out of
    /// the cross-window selection instead of pinning stale entries.
    last_swap: u64,
}

impl WindowCaptures {
    fn registry_sources(
        &self,
        context: usize,
    ) -> Vec<polymer_presentation::registry::RegistrySource> {
        self.tracked_sources
            .iter()
            .enumerate()
            .map(
                |(logical_slot, source)| polymer_presentation::registry::RegistrySource {
                    logical_slot: self
                        .source_slots
                        .get(logical_slot)
                        .copied()
                        .unwrap_or(logical_slot),
                    identity: polymer_presentation::registry::SourceIdentity {
                        domain: context as u64,
                        resource: source.incarnation,
                        subresource: source.mip as u32 | ((source.layer as u32) << 8),
                    },
                    extent: polymer_presentation::Extent2D {
                        width: source.width,
                        height: source.height,
                    },
                    placement: None,
                },
            )
            .collect()
    }
}

impl Default for WindowCaptures {
    fn default() -> Self {
        static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);
        Self {
            owner_id: NEXT_OWNER.fetch_add(1, Ordering::Relaxed),
            streams: Vec::new(),
            tracked_sources: Vec::new(),
            source_slots: Vec::new(),
            history: Default::default(),
            hwnd: 0,
            last_swap: 0,
        }
    }
}

static CAPTURES: OnceLock<CaptureStore> = OnceLock::new();
// Presentation assignment spans windows. Serialize that transaction while
// native resources remain behind per-entry locks and context-affine cleanup.
static PRESENTATION_LOCK: Mutex<()> = Mutex::new(());
static REGISTRY: OnceLock<polymer_presentation::registry::PresentationRegistry> = OnceLock::new();

fn captures() -> &'static CaptureStore {
    CAPTURES.get_or_init(Default::default)
}

/// Global swap counter used as the staleness clock for the cross-window
/// selection.
static SWAP_CLOCK: AtomicU64 = AtomicU64::new(0);

/// Swaps (across all windows) a window may go without presenting before its
/// tracked sources leave the cross-window selection.
const SELECTION_HORIZON: u64 =
    polymer_presentation::registry::PresentationRegistry::OWNERSHIP_HORIZON;

/// Graph selections are indexed independently of the window that presents
/// them, exactly like the Vulkan producer. Keeping this identity stable
/// preserves viewer ordering and CLI rotation across windows and swaps.
fn graph_surface_id(index: usize) -> SurfaceId {
    SurfaceId(polymer_presentation::registry::PresentationRegistry::logical_surface_id(index))
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
/// Releases captures before WGL deletes a context, restoring the caller's binding.
///
/// # Safety
/// Called on the application's context-deletion thread before the real WGL call.
pub unsafe fn before_delete_context(context: usize) {
    if context == 0 {
        return;
    }
    let _presentation = PRESENTATION_LOCK.lock();
    let devices = captures()
        .entries
        .iter()
        .filter_map(|entry| (entry.key().1 == context).then_some(entry.key().0))
        .collect::<Vec<_>>();
    let Some(binding) = (unsafe { context::ContextBinding::acquire(context, devices) }) else {
        return;
    };
    let registry = REGISTRY.get_or_init(Default::default);
    unsafe { captures().retire_context(context, registry) };
    drop(binding);
}

/// Called only after the native context deletion succeeds.
/// # Safety
/// The context must have been successfully deleted and must no longer be
/// current on any thread.
pub unsafe fn after_delete_context(context: usize) {
    let _presentation = PRESENTATION_LOCK.lock();
    captures().context_destroyed(context, REGISTRY.get_or_init(Default::default));
}

/// Called immediately before the real GDI `SwapBuffers`.
///
/// # Safety
/// The application's current OpenGL context and its device context must be live.
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
    let (observed, trusted) = tracking.take_draw_candidates(
        context as usize,
        Some(DrawCandidate::backbuffer(
            window_width,
            window_height,
            hdc as u64,
        )),
    );
    let present_quads = tracking.take_present_quads(context as usize);
    let key = (hdc as usize, context as usize);
    let now = SWAP_CLOCK.fetch_add(1, Ordering::Relaxed) + 1;
    let _presentation = PRESENTATION_LOCK.lock();
    let store = captures();
    let map = &store.entries;
    let registry = REGISTRY.get_or_init(Default::default);
    // Other contexts may be current on other threads. Only this context's
    // idle streams can release their GL objects during this presentation.
    unsafe { store.retire_idle(key.1, now, registry) };
    let changes;
    {
        let stored = map.entry(key).or_default();
        let mut entry = stored.lock();
        let reconciled = entry.history.reconcile(observed, trusted, |source| {
            let Some(refreshed) = tracking
                .refresh_sources(std::slice::from_ref(source))
                .first()
                .copied()
            else {
                return false;
            };
            *source = refreshed;
            true
        });
        changes = reconciled.changes;
        entry.source_slots = reconciled.slots;
        entry.tracked_sources = reconciled.surfaces;
        entry.hwnd = hwnd as isize;
        entry.last_swap = now;
    }
    // The requested indices select from the deterministic cross-window
    // selection — the union of every live window's tracked sources, tallest/
    // widest first — so `--track-render-targets 0,1` means the same screens
    // whether an application presents them in one window or several. Each
    // window then streams only the selected sources it presents itself,
    // which makes the stream's input HWND simply the presenting window.
    let selection = map
        .iter()
        .flat_map(|stored| {
            let captures = stored.lock();
            captures
                .tracked_sources
                .iter()
                .enumerate()
                .filter(|_| captures.last_swap.saturating_add(SELECTION_HORIZON) >= now)
                .map(|(index, _source)| ((captures.owner_id, index), captures.hwnd))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let titles = selection
        .iter()
        .filter_map(|&(_, hwnd)| window_title(NativeWindow(hwnd)).map(|title| (hwnd, title)))
        .collect::<HashMap<_, _>>();
    let requested = config::tracked_render_target_indices().unwrap_or_default();
    for stored in map.iter() {
        let context = stored.key().1;
        let captures = stored.lock();
        registry.observe(
            captures.owner_id,
            captures.hwnd,
            captures.registry_sources(context),
            captures.last_swap,
        );
    }
    // Entries from other contexts can outlive their HWND until their own
    // context can retire native resources. Exclude their destroyed windows
    // after importing observations and before assigning output slots.
    polymer_presentation::producer::ProducerWindows::refresh(registry);
    let mut resolved = Vec::new();
    for stored in map.iter() {
        let captures = stored.lock();
        for (slot, index) in registry.requests(captures.owner_id, &requested, now, |slot, hwnd| {
            if !config::tracked_render_target_has_surface_pattern(slot) {
                return None;
            }
            Some(titles.get(&hwnd).is_some_and(|title| {
                config::tracked_render_target_surface_matches(slot, title) == Some(true)
            }))
        }) {
            if captures.tracked_sources.get(index).is_some() {
                resolved.push((slot, (captures.owner_id, index)));
            }
        }
    }
    let fallback_slots =
        config::tracked_render_target_surface_slots(&registry.ordered_windows(now), |window| {
            window_title(NativeWindow(window))
        });
    let tracked_slot = fallback_slots
        .iter()
        .position(|window| *window == Some(hwnd as isize));
    if config::tracking_enabled() {
        let mut allowed = HashMap::<isize, Vec<SurfaceId>>::new();
        if resolved.is_empty() {
            for (slot, window) in fallback_slots.into_iter().enumerate() {
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
        for stored in map.iter() {
            let capture_context = stored.key().1;
            if capture_context != key.1 {
                continue;
            }
            let mut captures = stored.lock();
            let window = captures.hwnd;
            captures
                .streams
                .retain(|capture| surface_is_selected(&allowed, window, capture.surface_id()));
        }
    }
    let stored = map.get(&key).expect("window captures entry inserted above");
    let mut entry = stored.lock();
    let mut chosen = entry
        .tracked_sources
        .iter()
        .enumerate()
        .filter_map(|(index, source)| {
            let identity = (entry.owner_id, index);
            let slot = resolved
                .iter()
                .find_map(|&(slot, selected)| (selected == identity).then_some(slot))?;
            Some((slot, *source))
        })
        .collect::<Vec<_>>();
    chosen.sort_unstable_by_key(|&(slot, _)| slot);
    let presentation_extent = polymer_presentation::Extent2D {
        width: window_width,
        height: window_height,
    };
    let pairings = polymer_presentation::registry::PresentationRegistry::pair_placements(
        chosen
            .iter()
            .map(|(_, source)| polymer_presentation::history::HistorySurface::identity(source)),
        &present_quads,
    );
    let registered = entry.registry_sources(context as usize);
    let placement_sources = chosen
        .iter()
        .zip(pairings)
        .filter_map(|(&(slot, source), pairing)| {
            let index = resolved.iter().find_map(|&(requested, (owner, index))| {
                (requested == slot && owner == entry.owner_id).then_some(index)
            })?;
            let mut registered = *registered.get(index)?;
            registered.placement = if source.window_bounds {
                source.bounds.presentation_rect(presentation_extent)
            } else {
                pairing.and_then(|bounds| bounds.presentation_rect(presentation_extent))
            };
            Some(registered)
        })
        .collect::<Vec<_>>();
    let placements =
        registry.remember_placements(entry.owner_id, presentation_extent, &placement_sources, now);
    for ((_, source), placement) in chosen.iter_mut().zip(placements) {
        if !source.window_bounds
            && let Some(bounds) = placement
                .and_then(|bounds| QuadBounds::from_presentation_rect(bounds, presentation_extent))
        {
            source.bounds = bounds;
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
    let proposed = chosen
        .iter()
        .map(|&(slot, source)| (source.width, source.height, graph_surface_id(slot)))
        .collect::<Vec<_>>();
    let fallback = if config::tracking_enabled() && (!resolved.is_empty() || tracked_slot.is_none())
    {
        Vec::new()
    } else {
        vec![(window_width, window_height, window_surface)]
    };
    let desired = if proposed.is_empty() {
        fallback
    } else {
        proposed
    };
    if desired
        .iter()
        .any(|&(width, height, _)| width == 0 || height == 0)
    {
        return;
    }
    let mut old_streams = std::mem::take(&mut entry.streams)
        .into_iter()
        .map(|capture| (capture.surface_id(), capture))
        .collect::<HashMap<_, _>>();
    for (width, height, surface) in desired {
        let bottom_up = (chosen.is_empty() && (width, height) == window_extent)
            || chosen.iter().any(|(slot, source)| {
                graph_surface_id(*slot) == surface && source.target == 0 && source.texture == 0
            });
        let existing = old_streams.remove(&surface);
        let mut capture = if let Some(capture) = existing {
            capture
        } else {
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
                Ok(capture) => capture,
                Err(error) => {
                    tracing::debug!("polymer: OpenGL capture creation failed: {error}");
                    continue;
                }
            }
        };
        capture.set_requirements(width, height, bottom_up, hwnd as isize);
        let source_slot = resolved.iter().find_map(|&(slot, (owner, index))| {
            (owner == entry.owner_id && graph_surface_id(slot) == surface)
                .then(|| entry.source_slots.get(index).copied())
                .flatten()
        });
        if source_slot.is_some_and(|source_slot| changes.iter().any(|change| matches!(change,
            polymer_presentation::history::SurfaceChange::Add { slot }
            | polymer_presentation::history::SurfaceChange::Replace { slot } if *slot == source_slot))) {
            capture.source_changed();
        }
        capture.set_window_extent(window_width, window_height);
        entry.streams.push(capture);
    }
    drop(old_streams);
    for capture in &mut entry.streams {
        unsafe { capture.tick() };
        let source = chosen.iter().find_map(|(slot, source)| {
            (graph_surface_id(*slot) == capture.surface_id()).then_some(source)
        });
        // During a selection gap a tracked stream has no source this frame;
        // the whole-window fallback only makes sense at window extent.
        if source.is_none() && (capture.width(), capture.height()) != window_extent {
            continue;
        }
        let stream_id = StreamId((key.1 as u64) ^ capture.surface_id().0.rotate_left(17));
        let _ = unsafe { capture.copy_frame(stream_id, source) };
    }
}

#[cfg(test)]
mod tests;
