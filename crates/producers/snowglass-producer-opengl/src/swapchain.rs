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
    tracking::{DrawCandidate, QuadBounds, take_draw_candidate},
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
    next_slot: usize,
    sync_value: u64,
    frame_number: u64,
    deferred_frames: u64,
}

unsafe impl Send for Capture {}

static CAPTURES: OnceLock<Mutex<HashMap<(usize, usize), Capture>>> = OnceLock::new();

fn captures() -> &'static Mutex<HashMap<(usize, usize), Capture>> {
    CAPTURES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Called immediately before the real GDI `SwapBuffers`.
pub unsafe fn before_swap_buffers(hdc: isize) {
    if snowglass_platform_windows::vulkan_capture_active() {
        return;
    }
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
    let tracked = take_draw_candidate(context as usize);
    let (width, height) = tracked.map_or((window_width, window_height), |value| {
        (value.width, value.height)
    });
    if width == 0 || height == 0 {
        return;
    }
    let key = (hdc as usize, context as usize);
    let mut map = captures().lock().unwrap_or_else(|error| error.into_inner());
    let recreate = map.get(&key).is_none_or(|capture| {
        capture.width != width
            || capture.height != height
            || capture.window_width != window_width
            || capture.window_height != window_height
    });
    if recreate {
        map.remove(&key);
        match unsafe { Capture::new(hwnd as isize, width, height, window_width, window_height) } {
            Ok(capture) => {
                map.insert(key, capture);
            }
            Err(error) => {
                log(format!(
                    "snowglass: OpenGL EXT capture creation failed: {error}"
                ));
                return;
            }
        }
    }
    if let Some(capture) = map.get_mut(&key) {
        if let Err(error) = unsafe { capture.copy_frame(StreamId(key.1 as u64), tracked) } {
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
            next_slot: 0,
            sync_value: 0,
            frame_number: 0,
            deferred_frames: 0,
        })
    }

    unsafe fn copy_frame(
        &mut self,
        stream_id: StreamId,
        source: Option<DrawCandidate>,
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
            if let Some(source) = source {
                (self.functions.bind_framebuffer)(GL_READ_FRAMEBUFFER, self.read_framebuffer);
                if source.target == GL_TEXTURE_2D_ARRAY {
                    (self.functions.framebuffer_texture_layer)(
                        GL_READ_FRAMEBUFFER,
                        GL_COLOR_ATTACHMENT0,
                        source.texture,
                        0,
                        0,
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
                "snowglass: submitted OpenGL EXT frame {} from {}x{} into slot {} (quad {},{} {}x{})",
                self.frame_number,
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
    Ok(socket)
}
