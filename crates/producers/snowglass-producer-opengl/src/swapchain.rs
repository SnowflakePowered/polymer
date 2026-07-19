//! Per-context capture lifecycle and framebuffer presentation.

use crate::{
    interop::{
        ExternalImage, GL_COLOR_ATTACHMENT0, GL_COLOR_BUFFER_BIT, GL_DEVICE_LUID_EXT,
        GL_DRAW_FRAMEBUFFER, GL_DRAW_FRAMEBUFFER_BINDING, GL_NEAREST, GL_READ_FRAMEBUFFER,
        GL_READ_FRAMEBUFFER_BINDING, GL_SCISSOR_TEST, GL_TEXTURE_2D, GL_TEXTURE_2D_ARRAY,
        GlFunctions, Hdc, create_device, create_image, current_context, destroy_image, glDisable,
        glEnable, glGetIntegerv, glIsEnabled,
    },
    log,
    timeline::Timeline,
    tracking::{DrawCandidate, QuadBounds, take_draw_candidates, tracked_render_target_count},
};
use snowglass_ipc::{LocalSocket, Message, StreamReady};
use snowglass_platform_windows::{ProcessId, duplicate_handle_into_process, performance_counter};
use snowglass_protocol::{
    AdapterLuid, ExternalImageKind, ExternalSyncKind, FrameReady, MAX_IMAGE_SLOTS,
    PROTOCOL_VERSION, StreamId,
};
use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{Mutex, OnceLock},
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

struct Capture {
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
    stream_window: isize,
    next_slot: usize,
    sync_value: u64,
    frame_number: u64,
  
    presents: u64,
    deferred_frames: u64,
}

unsafe impl Send for Capture {}


#[derive(Default)]
struct WindowCaptures {
    streams: Vec<Capture>,
    tracked_sources: Vec<DrawCandidate>,
    untracked_frames: u32,
    pending_layout: Vec<(u32, u32)>,
    pending_frames: u32,
}

static CAPTURES: OnceLock<Mutex<HashMap<(usize, usize), WindowCaptures>>> = OnceLock::new();

fn captures() -> &'static Mutex<HashMap<(usize, usize), WindowCaptures>> {
    CAPTURES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The window identity published for a stream. Extra streams get offset
/// identities so the viewer treats each tracked screen as its own surface
/// instead of replacing the window's previous stream; the offsets are odd,
/// which no real window handle can collide with.
fn stream_window(hwnd: isize, index: usize) -> isize {
    if index == 0 {
        hwnd
    } else {
        hwnd + (index as isize * 2 - 1)
    }
}

/// Called immediately before the real GDI `SwapBuffers`.
pub unsafe fn before_swap_buffers(hdc: isize) {
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
    let (observed, trusted) = take_draw_candidates(context as usize);
    let key = (hdc as usize, context as usize);
    let mut map = captures().lock().unwrap_or_else(|error| error.into_inner());
    let entry = map.entry(key).or_default();
    // A trusted graph selection is complete: padding it with slots from an
    // earlier frame would pin stale page-flip faces alongside live ones.
    let tracked = if trusted {
        observed
    } else {
        merge_tracked_sources(
            &entry.tracked_sources,
            observed,
            tracked_render_target_count().unwrap_or(1),
        )
    };
    entry.tracked_sources.clone_from(&tracked);
    // One stream per tracked source, or the classic whole-window stream when
    // nothing resolves. Tracked layouts ride out short selection gaps and
    // hold off layout changes until they persist, rather than churning
    // through transport rebuilds on scene transitions.
    let window_extent = (window_width, window_height);
    let current = entry
        .streams
        .iter()
        .map(|capture| (capture.width, capture.height))
        .collect::<Vec<_>>();
    let tracked_layout = current.len() > 1 || current.first().is_some_and(|first| *first != window_extent);
    let proposed = tracked
        .iter()
        .map(|source| (source.width, source.height))
        .collect::<Vec<_>>();
    let desired = if !proposed.is_empty() {
        entry.untracked_frames = 0;
        if proposed == current || !tracked_layout {
            entry.pending_frames = 0;
            proposed
        } else {
            if entry.pending_layout == proposed {
                entry.pending_frames += 1;
            } else {
                entry.pending_layout = proposed.clone();
                entry.pending_frames = 1;
            }
            if entry.pending_frames >= 60 {
                entry.pending_frames = 0;
                proposed
            } else {
                current.clone()
            }
        }
    } else if tracked_layout && entry.untracked_frames < 120 {
        entry.untracked_frames += 1;
        current.clone()
    } else {
        entry.untracked_frames = 0;
        vec![window_extent]
    };
    if desired.iter().any(|(width, height)| *width == 0 || *height == 0) {
        return;
    }
    if desired != current && !tracked.is_empty() {
        log(format!(
            "snowglass: selected OpenGL render targets {}",
            tracked
                .iter()
                .map(|source| format!(
                    "texture {} layer {} ({:#x}, {}x{})",
                    source.texture, source.layer, source.target, source.width, source.height
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // Window geometry feeds the capture-rect metadata; a resized window
    // rebuilds every stream.
    if entry
        .streams
        .first()
        .is_some_and(|capture| (capture.window_width, capture.window_height) != window_extent)
    {
        entry.streams.clear();
    }
    while entry.streams.len() > desired.len() {
        entry.streams.pop();
    }
    for (index, &(width, height)) in desired.iter().enumerate() {
        let up_to_date = entry
            .streams
            .get(index)
            .is_some_and(|capture| (capture.width, capture.height) == (width, height));
        if up_to_date {
            continue;
        }
        match unsafe {
            Capture::new(
                stream_window(hwnd as isize, index),
                width,
                height,
                window_width,
                window_height,
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
                log(format!(
                    "snowglass: OpenGL EXT capture creation failed: {error}"
                ));
                return;
            }
        }
    }
    // While a layout change is being dampened the source list can differ
    // from the stream list; feed each stream the source matching its extent
    // and let unmatched streams idle a frame.
    let mut source_used = vec![false; tracked.len()];
    for (index, capture) in entry.streams.iter_mut().enumerate() {
        capture.presents += 1;
        // A stream can be rejected while the viewer's surfaces are
        // transiently full; retry the connection periodically.
        if capture.ipc.is_none() && capture.presents.is_multiple_of(120) {
            capture.reconnect();
        }
        let source = tracked.iter().enumerate().find_map(|(source_index, source)| {
            (!source_used[source_index]
                && (source.width, source.height) == (capture.width, capture.height))
                .then_some(source_index)
        });
        if let Some(source_index) = source {
            source_used[source_index] = true;
        }
        let source = source.map(|source_index| &tracked[source_index]);
        // During a selection gap a tracked stream has no source this frame;
        // the whole-window fallback only makes sense at window extent.
        if source.is_none() && (capture.width, capture.height) != window_extent {
            continue;
        }
        let stream_id = StreamId((key.1 as u64) ^ ((index as u64) << 56));
        if let Err(error) = unsafe { capture.copy_frame(stream_id, source) } {
            capture.deferred_frames = capture.deferred_frames.wrapping_add(1);
            if capture.deferred_frames == 1 || capture.deferred_frames % 600 == 0 {
                log(format!(
                    "snowglass: OpenGL EXT capture deferred: {error} (count={})",
                    capture.deferred_frames
                ));
            }
        }
    }
}

impl Capture {
    unsafe fn new(
        hwnd: isize,
        width: u32,
        height: u32,
        window_width: u32,
        window_height: u32,
    ) -> Result<Self, String> {
        let functions = unsafe { GlFunctions::load() }
            .ok_or("GL_EXT_memory_object_win32 and GL_EXT_semaphore_win32 are unavailable")?;
        let mut luid_bytes = [0u8; 8];
        unsafe { (functions.get_unsigned_byte)(GL_DEVICE_LUID_EXT, luid_bytes.as_mut_ptr()) };
        let adapter_luid = AdapterLuid {
            low_part: u32::from_le_bytes(luid_bytes[..4].try_into().expect("four bytes")),
            high_part: i32::from_le_bytes(luid_bytes[4..].try_into().expect("four bytes")),
        };
        let (device, adapter) = unsafe { create_device(adapter_luid) }?;
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
                image: unsafe { create_image(&device, &functions, width, height) }?,
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
            hwnd,
            width,
            height,
        ) {
            Ok(socket) => Some(socket),
            Err(error) => {
                log(format!(
                    "snowglass: OpenGL viewer connection failed: {error}"
                ));
                None
            }
        };
        log(format!(
            "snowglass: attached OpenGL EXT external-object ring {width}x{height} (window {window_width}x{window_height})"
        ));
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
            stream_window: hwnd,
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
            self.stream_window,
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
                        self.height as i32,
                        self.width as i32,
                        0,
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
                        self.height as i32,
                        source.width as i32,
                        0,
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
        let bounds = source.map_or(
            QuadBounds {
                x: 0,
                y: 0,
                width: self.window_width,
                height: self.window_height,
            },
            |value| QuadBounds {
                x: value.bounds.x,
                y: self
                    .window_height
                    .saturating_sub(value.bounds.y.saturating_add(value.bounds.height)),
                width: value.bounds.width,
                height: value.bounds.height,
            },
        );
        let message = Message::FrameReady(FrameReady {
            stream_id,
            slot: slot_index as u32,
            render_width: self.width,
            render_height: self.height,
            capture_x: bounds.x,
            capture_y: bounds.y,
            capture_width: bounds.width,
            capture_height: bounds.height,
            frame_number: self.frame_number,
            sync_value,
            timestamp_qpc: performance_counter().unwrap_or(0),
        });
        if self.frame_number <= self.slots.len() as u64 || self.frame_number % 120 == 0 {
            log(format!(
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
            ));
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

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            (self.functions.delete_framebuffers)(1, &self.framebuffer);
            (self.functions.delete_framebuffers)(1, &self.read_framebuffer);
            self.ready.destroy(&self.functions);
            self.consumer.destroy(&self.functions);
            for slot in &self.slots {
                destroy_image(&self.functions, &slot.image);
            }
        }
    }
}

fn connect_viewer(
    slots: &[ImageSlot],
    ready_handle: HANDLE,
    consumer_handle: HANDLE,
    adapter_luid: AdapterLuid,
    hwnd: isize,
    width: u32,
    height: u32,
) -> std::io::Result<LocalSocket> {
    let session = std::env::var("SNOWGLASS_SESSION")
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
    let mut image_handles = [0i64; MAX_IMAGE_SLOTS];
    for (destination, slot) in image_handles.iter_mut().zip(slots) {
        *destination =
            duplicate_handle_into_process(slot.image.handle.0 as isize, ProcessId(viewer_pid))?
                as i64;
    }
    socket.send(Message::StreamReady(StreamReady {
        producer_process_id: std::process::id(),
        adapter_luid,
        source_window: hwnd as i64,
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
