//! OpenGL capture through EXT external memory objects and semaphores.
#![allow(unsafe_code)]

use snowglass_ipc::{LocalSocket, Message, StreamReady};
use snowglass_platform_windows::{ProcessId, duplicate_handle_into_process, performance_counter};
use snowglass_protocol::{
    AdapterLuid, ExternalImageKind, ExternalSyncKind, FrameReady, MAX_IMAGE_SLOTS,
    PROTOCOL_VERSION, StreamId,
};
use std::{
    collections::HashMap,
    ffi::c_void,
    io::Write,
    mem,
    sync::{Mutex, OnceLock},
};
use windows::Win32::{
    Foundation::{CloseHandle, GENERIC_ALL, HANDLE},
    Graphics::{
        Direct3D::D3D_FEATURE_LEVEL_11_0,
        Direct3D12::{
            D3D12_CPU_PAGE_PROPERTY_UNKNOWN, D3D12_FENCE_FLAG_SHARED, D3D12_HEAP_FLAG_SHARED,
            D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT, D3D12_MEMORY_POOL_UNKNOWN,
            D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
            D3D12_RESOURCE_STATE_COMMON, D3D12_TEXTURE_LAYOUT_UNKNOWN, D3D12CreateDevice,
            ID3D12Device, ID3D12Fence, ID3D12Resource,
        },
        Dxgi::{
            Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC},
            CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4,
        },
    },
};

pub const BACKEND_NAME: &str = "opengl";

type Hdc = *mut c_void;
type Hglrc = *mut c_void;
type Gluint = u32;
type Glenum = u32;
type Glint = i32;
type Glsizei = i32;
type Gluint64 = u64;

const GL_TEXTURE_2D: Glenum = 0x0DE1;
const GL_TEXTURE_2D_ARRAY: Glenum = 0x8C1A;
const GL_TEXTURE_BINDING_2D: Glenum = 0x8069;
const GL_TEXTURE_BINDING_2D_ARRAY: Glenum = 0x8C1D;
const GL_TEXTURE_WIDTH: Glenum = 0x1000;
const GL_TEXTURE_HEIGHT: Glenum = 0x1001;
const GL_RGBA8: Glenum = 0x8058;
const GL_READ_FRAMEBUFFER: Glenum = 0x8CA8;
const GL_DRAW_FRAMEBUFFER: Glenum = 0x8CA9;
const GL_READ_FRAMEBUFFER_BINDING: Glenum = 0x8CAA;
const GL_DRAW_FRAMEBUFFER_BINDING: Glenum = 0x8CA6;
const GL_COLOR_ATTACHMENT0: Glenum = 0x8CE0;
const GL_COLOR_BUFFER_BIT: u32 = 0x0000_4000;
const GL_NEAREST: Glenum = 0x2600;
const GL_VIEWPORT: Glenum = 0x0BA2;
const GL_SCISSOR_TEST: Glenum = 0x0C11;
const GL_SCISSOR_BOX: Glenum = 0x0C10;
const GL_ACTIVE_TEXTURE: Glenum = 0x84E0;
const GL_TEXTURE0: Glenum = 0x84C0;
const GL_DEVICE_LUID_EXT: Glenum = 0x9599;
const GL_DEDICATED_MEMORY_OBJECT_EXT: Glenum = 0x9581;
const GL_HANDLE_TYPE_D3D12_RESOURCE_EXT: Glenum = 0x958A;
const GL_HANDLE_TYPE_D3D12_FENCE_EXT: Glenum = 0x9594;
const GL_D3D12_FENCE_VALUE_EXT: Glenum = 0x9595;
const GL_LAYOUT_GENERAL_EXT: Glenum = 0x958D;

#[repr(C)]
#[derive(Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "opengl32")]
unsafe extern "system" {
    fn wglGetCurrentContext() -> Hglrc;
    fn wglGetProcAddress(name: *const u8) -> *const c_void;
    fn glGenTextures(count: Glsizei, textures: *mut Gluint);
    fn glDeleteTextures(count: Glsizei, textures: *const Gluint);
    fn glBindTexture(target: Glenum, texture: Gluint);
    fn glGetIntegerv(name: Glenum, value: *mut Glint);
    fn glGetTexLevelParameteriv(target: Glenum, level: Glint, name: Glenum, value: *mut Glint);
    fn glIsEnabled(capability: Glenum) -> u8;
    fn glEnable(capability: Glenum);
    fn glDisable(capability: Glenum);
}

#[link(name = "user32")]
unsafe extern "system" {
    fn WindowFromDC(hdc: Hdc) -> *mut c_void;
    fn GetClientRect(hwnd: *mut c_void, rect: *mut Rect) -> i32;
}

type GlGetUnsignedBytev = unsafe extern "system" fn(Glenum, *mut u8);
type GlActiveTexture = unsafe extern "system" fn(Glenum);
type GlGenFramebuffers = unsafe extern "system" fn(Glsizei, *mut Gluint);
type GlDeleteFramebuffers = unsafe extern "system" fn(Glsizei, *const Gluint);
type GlBindFramebuffer = unsafe extern "system" fn(Glenum, Gluint);
type GlFramebufferTexture2D = unsafe extern "system" fn(Glenum, Glenum, Glenum, Gluint, Glint);
type GlFramebufferTextureLayer = unsafe extern "system" fn(Glenum, Glenum, Gluint, Glint, Glint);
type GlBlitFramebuffer =
    unsafe extern "system" fn(Glint, Glint, Glint, Glint, Glint, Glint, Glint, Glint, u32, Glenum);
type GlCreateMemoryObjects = unsafe extern "system" fn(Glsizei, *mut Gluint);
type GlDeleteMemoryObjects = unsafe extern "system" fn(Glsizei, *const Gluint);
type GlMemoryObjectParameteriv = unsafe extern "system" fn(Gluint, Glenum, *const Glint);
type GlImportMemoryWin32Handle = unsafe extern "system" fn(Gluint, Gluint64, Glenum, *mut c_void);
type GlTexStorageMem2D =
    unsafe extern "system" fn(Glenum, Glsizei, Glenum, Glsizei, Glsizei, Gluint, Gluint64);
type GlGenSemaphores = unsafe extern "system" fn(Glsizei, *mut Gluint);
type GlDeleteSemaphores = unsafe extern "system" fn(Glsizei, *const Gluint);
type GlImportSemaphoreWin32Handle = unsafe extern "system" fn(Gluint, Glenum, *mut c_void);
type GlSemaphoreParameterui64v = unsafe extern "system" fn(Gluint, Glenum, *const Gluint64);
type GlSignalSemaphore =
    unsafe extern "system" fn(Gluint, Gluint, *const Gluint, Gluint, *const Gluint, *const Glenum);

#[derive(Clone, Copy)]
struct GlFunctions {
    get_unsigned_byte: GlGetUnsignedBytev,
    gen_framebuffers: GlGenFramebuffers,
    delete_framebuffers: GlDeleteFramebuffers,
    bind_framebuffer: GlBindFramebuffer,
    framebuffer_texture_2d: GlFramebufferTexture2D,
    framebuffer_texture_layer: GlFramebufferTextureLayer,
    blit_framebuffer: GlBlitFramebuffer,
    create_memory_objects: GlCreateMemoryObjects,
    delete_memory_objects: GlDeleteMemoryObjects,
    memory_object_parameter: GlMemoryObjectParameteriv,
    import_memory: GlImportMemoryWin32Handle,
    tex_storage_mem_2d: GlTexStorageMem2D,
    gen_semaphores: GlGenSemaphores,
    delete_semaphores: GlDeleteSemaphores,
    import_semaphore: GlImportSemaphoreWin32Handle,
    semaphore_parameter: GlSemaphoreParameterui64v,
    signal_semaphore: GlSignalSemaphore,
}

unsafe fn proc<T: Copy>(name: &'static [u8]) -> Option<T> {
    let address = unsafe { wglGetProcAddress(name.as_ptr()) };
    (!address.is_null()).then(|| unsafe { mem::transmute_copy(&address) })
}

impl GlFunctions {
    unsafe fn load() -> Option<Self> {
        Some(Self {
            get_unsigned_byte: unsafe { proc(b"glGetUnsignedBytevEXT\0")? },
            gen_framebuffers: unsafe { proc(b"glGenFramebuffers\0")? },
            delete_framebuffers: unsafe { proc(b"glDeleteFramebuffers\0")? },
            bind_framebuffer: unsafe { proc(b"glBindFramebuffer\0")? },
            framebuffer_texture_2d: unsafe { proc(b"glFramebufferTexture2D\0")? },
            framebuffer_texture_layer: unsafe { proc(b"glFramebufferTextureLayer\0")? },
            blit_framebuffer: unsafe { proc(b"glBlitFramebuffer\0")? },
            create_memory_objects: unsafe { proc(b"glCreateMemoryObjectsEXT\0")? },
            delete_memory_objects: unsafe { proc(b"glDeleteMemoryObjectsEXT\0")? },
            memory_object_parameter: unsafe { proc(b"glMemoryObjectParameterivEXT\0")? },
            import_memory: unsafe { proc(b"glImportMemoryWin32HandleEXT\0")? },
            tex_storage_mem_2d: unsafe { proc(b"glTexStorageMem2DEXT\0")? },
            gen_semaphores: unsafe { proc(b"glGenSemaphoresEXT\0")? },
            delete_semaphores: unsafe { proc(b"glDeleteSemaphoresEXT\0")? },
            import_semaphore: unsafe { proc(b"glImportSemaphoreWin32HandleEXT\0")? },
            semaphore_parameter: unsafe { proc(b"glSemaphoreParameterui64vEXT\0")? },
            signal_semaphore: unsafe { proc(b"glSignalSemaphoreEXT\0")? },
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct QuadBounds {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy)]
struct DrawCandidate {
    texture: Gluint,
    target: Glenum,
    width: u32,
    height: u32,
    bounds: QuadBounds,
    score: u64,
}

static DRAW_CANDIDATES: OnceLock<Mutex<HashMap<usize, DrawCandidate>>> = OnceLock::new();
static ACTIVE_TEXTURE: OnceLock<GlActiveTexture> = OnceLock::new();

fn draw_candidates() -> &'static Mutex<HashMap<usize, DrawCandidate>> {
    DRAW_CANDIDATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn intersect(a: [i32; 4], b: [i32; 4]) -> Option<[i32; 4]> {
    let left = a[0].max(b[0]);
    let bottom = a[1].max(b[1]);
    let right = a[0].saturating_add(a[2]).min(b[0].saturating_add(b[2]));
    let top = a[1].saturating_add(a[3]).min(b[1].saturating_add(b[3]));
    (right > left && top > bottom).then_some([left, bottom, right - left, top - bottom])
}

/// Samples textures and the actual raster bounds for a draw to the default framebuffer.
pub unsafe fn record_draw_candidate() {
    if std::env::var_os("SNOWGLASS_TRACK_RENDER_TARGET").is_none() {
        return;
    }
    let context = unsafe { wglGetCurrentContext() };
    if context.is_null() {
        return;
    }
    let mut draw_framebuffer = 0;
    unsafe { glGetIntegerv(GL_DRAW_FRAMEBUFFER_BINDING, &mut draw_framebuffer) };
    if draw_framebuffer != 0 {
        return;
    }
    let active_texture = if let Some(function) = ACTIVE_TEXTURE.get() {
        *function
    } else {
        let Some(function): Option<GlActiveTexture> = (unsafe { proc(b"glActiveTexture\0") })
        else {
            return;
        };
        let _ = ACTIVE_TEXTURE.set(function);
        function
    };
    let mut viewport = [0; 4];
    let mut old_active = 0;
    unsafe {
        glGetIntegerv(GL_VIEWPORT, viewport.as_mut_ptr());
        glGetIntegerv(GL_ACTIVE_TEXTURE, &mut old_active);
    }
    let raster = if unsafe { glIsEnabled(GL_SCISSOR_TEST) } != 0 {
        let mut scissor = [0; 4];
        unsafe { glGetIntegerv(GL_SCISSOR_BOX, scissor.as_mut_ptr()) };
        intersect(viewport, scissor)
    } else {
        (viewport[2] > 0 && viewport[3] > 0).then_some(viewport)
    };
    let Some(raster) = raster else {
        return;
    };
    let bounds = QuadBounds {
        x: raster[0].max(0) as u32,
        y: raster[1].max(0) as u32,
        width: raster[2] as u32,
        height: raster[3] as u32,
    };
    let raster_area = u64::from(bounds.width) * u64::from(bounds.height);
    let mut best: Option<DrawCandidate> = None;
    let active_unit = (old_active as u32).saturating_sub(GL_TEXTURE0);
    for unit in [active_unit, 0]
        .into_iter()
        .enumerate()
        .filter_map(|(index, unit)| (index == 0 || unit != active_unit).then_some(unit))
    {
        unsafe { active_texture(GL_TEXTURE0 + unit) };
        for (target, binding) in [
            (GL_TEXTURE_2D, GL_TEXTURE_BINDING_2D),
            (GL_TEXTURE_2D_ARRAY, GL_TEXTURE_BINDING_2D_ARRAY),
        ] {
            let mut texture = 0;
            let mut width = 0;
            let mut height = 0;
            unsafe {
                glGetIntegerv(binding, &mut texture);
                if texture != 0 {
                    glGetTexLevelParameteriv(target, 0, GL_TEXTURE_WIDTH, &mut width);
                    glGetTexLevelParameteriv(target, 0, GL_TEXTURE_HEIGHT, &mut height);
                }
            }
            if width > 1 && height > 1 {
                // A stale large texture may remain bound while UI is drawn later. Favor draws
                // whose destination shape matches the texture being sampled; this naturally
                // selects a letterboxed/pillarboxed game quad over a full-window UI pass.
                let cross_a = width as u64 * u64::from(bounds.height);
                let cross_b = height as u64 * u64::from(bounds.width);
                let aspect_match = cross_a
                    .min(cross_b)
                    .saturating_mul(1024)
                    .checked_div(cross_a.max(cross_b))
                    .unwrap_or(0);
                let candidate = DrawCandidate {
                    texture: texture as u32,
                    target,
                    width: width as u32,
                    height: height as u32,
                    bounds,
                    score: raster_area
                        .saturating_mul(width as u64 * height as u64)
                        .saturating_mul(aspect_match),
                };
                if best.is_none_or(|current| candidate.score > current.score) {
                    best = Some(candidate);
                }
            }
        }
    }
    unsafe { active_texture(old_active as u32) };
    if let Some(candidate) = best {
        let mut candidates = draw_candidates()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if candidates
            .get(&(context as usize))
            .is_none_or(|current| candidate.score > current.score)
        {
            candidates.insert(context as usize, candidate);
        }
    }
}

struct ExternalSlot {
    _resource: ID3D12Resource,
    handle: HANDLE,
    memory: Gluint,
    texture: Gluint,
    release_value: u64,
}

unsafe impl Send for ExternalSlot {}

struct Capture {
    width: u32,
    height: u32,
    window_width: u32,
    window_height: u32,
    functions: GlFunctions,
    framebuffer: Gluint,
    read_framebuffer: Gluint,
    slots: Vec<ExternalSlot>,
    _device: ID3D12Device,
    _ready_fence: ID3D12Fence,
    ready_handle: HANDLE,
    ready_semaphore: Gluint,
    _consumer_fence: ID3D12Fence,
    consumer_handle: HANDLE,
    consumer_semaphore: Gluint,
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
    let context = unsafe { wglGetCurrentContext() };
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
    let tracked = if std::env::var_os("SNOWGLASS_TRACK_RENDER_TARGET").is_some() {
        draw_candidates()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(context as usize))
    } else {
        None
    };
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
        match unsafe { capture.copy_frame(StreamId(key.1 as u64), tracked) } {
            Ok(()) => {}
            Err(error) => {
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
            slots.push(unsafe { create_slot(&device, &functions, width, height) }?);
        }
        let (ready_fence, ready_handle, ready_semaphore) =
            unsafe { create_fence(&device, &functions) }?;
        let (consumer_fence, consumer_handle, consumer_semaphore) =
            unsafe { create_fence(&device, &functions) }?;
        let ipc = match connect_viewer(
            &slots,
            ready_handle,
            consumer_handle,
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
            _ready_fence: ready_fence,
            ready_handle,
            ready_semaphore,
            _consumer_fence: consumer_fence,
            consumer_handle,
            consumer_semaphore,
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
        let completed = unsafe { self._consumer_fence.GetCompletedValue() };
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
                slot.texture,
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
        let texture = slot.texture;
        let layout = GL_LAYOUT_GENERAL_EXT;
        unsafe {
            (self.functions.semaphore_parameter)(
                self.ready_semaphore,
                GL_D3D12_FENCE_VALUE_EXT,
                &sync_value,
            );
            (self.functions.signal_semaphore)(
                self.ready_semaphore,
                0,
                std::ptr::null(),
                1,
                &texture,
                &layout,
            );
        }
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
            (self.functions.delete_semaphores)(1, &self.ready_semaphore);
            (self.functions.delete_semaphores)(1, &self.consumer_semaphore);
            for slot in &self.slots {
                glDeleteTextures(1, &slot.texture);
                (self.functions.delete_memory_objects)(1, &slot.memory);
                let _ = CloseHandle(slot.handle);
            }
            let _ = CloseHandle(self.ready_handle);
            let _ = CloseHandle(self.consumer_handle);
        }
    }
}

unsafe fn create_device(luid: AdapterLuid) -> Result<(ID3D12Device, IDXGIAdapter1), String> {
    let factory = unsafe { CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0)) }
        .map_err(|error| format!("CreateDXGIFactory2: {error}"))?;
    for index in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        let desc = unsafe { adapter.GetDesc1() }
            .map_err(|error| format!("IDXGIAdapter1::GetDesc1: {error}"))?;
        if desc.AdapterLuid.LowPart != luid.low_part || desc.AdapterLuid.HighPart != luid.high_part
        {
            continue;
        }
        let mut device = None;
        unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device) }
            .map_err(|error| format!("D3D12CreateDevice: {error}"))?;
        return device
            .map(|device| (device, adapter))
            .ok_or_else(|| "D3D12CreateDevice returned no device".into());
    }
    Err(format!(
        "no DXGI adapter matched OpenGL LUID {:08x}:{:08x}",
        luid.high_part as u32, luid.low_part
    ))
}

unsafe fn create_slot(
    device: &ID3D12Device,
    functions: &GlFunctions,
    width: u32,
    height: u32,
) -> Result<ExternalSlot, String> {
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
        MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
        CreationNodeMask: 1,
        VisibleNodeMask: 1,
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: width as u64,
        Height: height,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET
            | D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
    };
    let mut resource = None;
    unsafe {
        device.CreateCommittedResource(
            &heap,
            D3D12_HEAP_FLAG_SHARED,
            &desc,
            D3D12_RESOURCE_STATE_COMMON,
            None,
            &mut resource,
        )
    }
    .map_err(|error| format!("ID3D12Device::CreateCommittedResource: {error}"))?;
    let resource = resource.ok_or("CreateCommittedResource returned no resource")?;
    let handle = unsafe { device.CreateSharedHandle(&resource, None, GENERIC_ALL.0, None) }
        .map_err(|error| format!("CreateSharedHandle(resource): {error}"))?;
    let mut memory = 0;
    let mut texture = 0;
    let dedicated = 1;
    let mut old_texture = 0;
    unsafe {
        (functions.create_memory_objects)(1, &mut memory);
        (functions.memory_object_parameter)(memory, GL_DEDICATED_MEMORY_OBJECT_EXT, &dedicated);
        (functions.import_memory)(memory, 0, GL_HANDLE_TYPE_D3D12_RESOURCE_EXT, handle.0);
        glGetIntegerv(GL_TEXTURE_BINDING_2D, &mut old_texture);
        glGenTextures(1, &mut texture);
        glBindTexture(GL_TEXTURE_2D, texture);
        (functions.tex_storage_mem_2d)(
            GL_TEXTURE_2D,
            1,
            GL_RGBA8,
            width as i32,
            height as i32,
            memory,
            0,
        );
        glBindTexture(GL_TEXTURE_2D, old_texture as u32);
    }
    Ok(ExternalSlot {
        _resource: resource,
        handle,
        memory,
        texture,
        release_value: 0,
    })
}

unsafe fn create_fence(
    device: &ID3D12Device,
    functions: &GlFunctions,
) -> Result<(ID3D12Fence, HANDLE, Gluint), String> {
    let fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_SHARED) }
        .map_err(|error| format!("ID3D12Device::CreateFence: {error}"))?;
    let handle = unsafe { device.CreateSharedHandle(&fence, None, GENERIC_ALL.0, None) }
        .map_err(|error| format!("CreateSharedHandle(fence): {error}"))?;
    let mut semaphore = 0;
    unsafe {
        (functions.gen_semaphores)(1, &mut semaphore);
        (functions.import_semaphore)(semaphore, GL_HANDLE_TYPE_D3D12_FENCE_EXT, handle.0);
    }
    Ok((fence, handle, semaphore))
}

fn connect_viewer(
    slots: &[ExternalSlot],
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
            duplicate_handle_into_process(slot.handle.0 as isize, ProcessId(viewer_pid))? as i64;
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

fn log(message: impl std::fmt::Display) {
    eprintln!("{message}");
    let Ok(path) = std::env::var("SNOWGLASS_OPENGL_LOG_FILE") else {
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

#[cfg(test)]
mod tests {
    use super::intersect;

    #[test]
    fn scissor_is_intersected_with_viewport() {
        assert_eq!(
            intersect([100, 50, 800, 600], [0, 120, 1920, 300]),
            Some([100, 120, 800, 300])
        );
        assert_eq!(intersect([0, 0, 100, 100], [200, 200, 10, 10]), None);
    }
}
