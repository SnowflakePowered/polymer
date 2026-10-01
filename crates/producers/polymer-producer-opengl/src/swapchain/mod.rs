mod context;
mod ext;
mod lifecycle;
mod storage;
pub mod wgl;
use lifecycle::Capture;
use storage::CaptureStore;
// Per-context capture lifecycle and framebuffer presentation.

use crate::{
    interop::{
        GL_COLOR_ATTACHMENT0, GL_COLOR_BUFFER_BIT, GL_DRAW_FRAMEBUFFER,
        GL_DRAW_FRAMEBUFFER_BINDING, GL_NEAREST, GL_READ_FRAMEBUFFER, GL_READ_FRAMEBUFFER_BINDING,
        GL_SCISSOR_TEST, GL_TEXTURE_2D, glDisable, glEnable, glGetIntegerv, glIsEnabled,
        memory_object_win32::{ExternalImage, GL_DEVICE_LUID_EXT, GlFunctions},
    },
    timeline::Timeline,
};
use parking_lot::Mutex;
use polymer_hook::config;
use polymer_presentation::producer::ExportDescriptor;
use polymer_presentation::producer::ProducerSession;
use polymer_protocol::{AdapterLuid, ExternalImageKind, FrameReady, GraphicsHook, SurfaceId};
use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};
use windows::Win32::Graphics::{
    Direct3D12::ID3D12Device, Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM,
};

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

static CAPTURES: OnceLock<CaptureStore> = OnceLock::new();
static PRESENTATION_LOCK: Mutex<()> = Mutex::new(());

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
