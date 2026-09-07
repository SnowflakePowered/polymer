mod context;
mod ext;
mod lifecycle;
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
    },
    timeline::Timeline,
};
use parking_lot::Mutex;
use polymer_hook::config;
use polymer_platform_windows::{NativeWindow, performance_counter, window_title};
use polymer_presentation::producer::ExportDescriptor;
use polymer_presentation::producer::ProducerSession;
use polymer_protocol::{AdapterLuid, ExternalImageKind, ExternalSyncKind, StreamId, SurfaceId};
use std::{
    ffi::c_void,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use windows::Win32::Graphics::{
    Direct3D12::ID3D12Device, Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM,
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
    functions: GlFunctions,
    framebuffer: u32,
    slots: Vec<ImageSlot>,
    _device: ID3D12Device,
    ready: Timeline,
    consumer: Timeline,
    ipc: Option<ProducerSession>,
    scheduler: polymer_presentation::scheduling::SlotScheduler,
    sync_value: u64,
}

unsafe impl Send for ExtCapture {}

#[derive(Default)]
struct WindowCaptures {
    capture: Option<Capture>,
    hwnd: isize,
    first_swap: u64,
    last_swap: u64,
}

static CAPTURES: OnceLock<CaptureStore> = OnceLock::new();
static PRESENTATION_LOCK: Mutex<()> = Mutex::new(());
static SWAP_CLOCK: AtomicU64 = AtomicU64::new(0);

/// # Safety
/// Called on the context-deletion thread before the real WGL call.
pub unsafe fn before_delete_context(context: usize) {
    let Some(store) = CAPTURES.get() else {
        return;
    };
    let _presentation = PRESENTATION_LOCK.lock();
    let devices = store
        .entries
        .iter()
        .filter_map(|entry| (entry.key().1 == context).then_some(entry.key().0))
        .collect::<Vec<_>>();
    let Some(_binding) = (unsafe { context::ContextBinding::acquire(context, devices) }) else {
        return;
    };
    unsafe { store.retire_context(context) };
}

/// # Safety
/// The native context must have been successfully deleted.
pub unsafe fn after_delete_context(context: usize) {
    let _presentation = PRESENTATION_LOCK.lock();
    if let Some(store) = CAPTURES.get() {
        store.context_destroyed(context);
    }
}

/// Copies the presenting window's default framebuffer before its native swap.
/// # Safety
/// The application's current OpenGL context and its device context must be live.
pub unsafe fn before_swap_buffers(hdc: isize) {
    if !config::capture_enabled() {
        return;
    }
    let context = unsafe { current_context() };
    if context.is_null() || unsafe { crate::interop::wglGetCurrentDC() } as isize != hdc {
        return;
    }
    let hwnd = unsafe { WindowFromDC(hdc as Hdc) };
    let mut rect = Rect::default();
    if hwnd.is_null() || unsafe { GetClientRect(hwnd, &mut rect) } == 0 {
        return;
    }
    let width = (rect.right - rect.left).max(0) as u32;
    let height = (rect.bottom - rect.top).max(0) as u32;
    if width == 0 || height == 0 {
        return;
    }
    let _presentation = PRESENTATION_LOCK.lock();
    let store = CAPTURES.get_or_init(Default::default);
    unsafe {
        store.present(
            hdc as usize,
            context as usize,
            hwnd as isize,
            polymer_presentation::Extent2D { width, height },
        )
    };
}
