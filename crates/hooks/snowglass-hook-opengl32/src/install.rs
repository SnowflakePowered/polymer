//! Process-wide detour installation and original-procedure routing.

use crate::hooks::Hdc;
use snowglass_platform_windows::InlineDetour;
use snowglass_producer_opengl::Tracking;
use std::{
    ffi::c_void,
    sync::{Arc, Mutex, OnceLock},
};
use windows_sys::Win32::Foundation::E_FAIL;

type SwapBuffersFn = unsafe extern "system" fn(Hdc) -> i32;
type SwapLayerBuffersFn = unsafe extern "system" fn(Hdc, u32) -> i32;
type WglGetProcAddressFn = unsafe extern "system" fn(*const u8) -> *const c_void;
type GlDrawArraysFn = unsafe extern "system" fn(u32, i32, i32);
type GlDrawElementsFn = unsafe extern "system" fn(u32, i32, u32, *const c_void);
type GlDrawElementsBaseVertexFn = unsafe extern "system" fn(u32, i32, u32, *const c_void, i32);
type GlBlitFramebufferFn =
    unsafe extern "system" fn(i32, i32, i32, i32, i32, i32, i32, i32, u32, u32);
type GlCopyImageSubDataFn = unsafe extern "system" fn(
    u32,
    u32,
    i32,
    i32,
    i32,
    i32,
    u32,
    u32,
    i32,
    i32,
    i32,
    i32,
    i32,
    i32,
    i32,
);

#[link(name = "gdi32")]
unsafe extern "system" {
    #[link_name = "SwapBuffers"]
    fn system_swap_buffers(hdc: Hdc) -> i32;
}

#[link(name = "opengl32")]
unsafe extern "system" {
    #[link_name = "wglGetProcAddress"]
    fn system_wgl_get_proc_address(name: *const u8) -> *const c_void;
    #[link_name = "wglSwapLayerBuffers"]
    fn system_swap_layer_buffers(hdc: Hdc, planes: u32) -> i32;
    #[link_name = "glDrawArrays"]
    fn system_draw_arrays(mode: u32, first: i32, count: i32);
    #[link_name = "glDrawElements"]
    fn system_draw_elements(mode: u32, count: i32, element_type: u32, indices: *const c_void);
}

struct OpenGlRuntime {
    tracking: Arc<Tracking>,
}

struct DrawArraysTerminal(InlineDetour<GlDrawArraysFn>);

impl snowglass_hook::TerminalLifecycle for DrawArraysTerminal {
    fn enable(&self) -> Result<(), i32> {
        self.0.enable()
    }
    fn disable(&self) -> Result<(), i32> {
        self.0.disable()
    }
}

impl crate::hooks::DrawArraysTerminal for DrawArraysTerminal {
    unsafe fn call(&self, mode: u32, first: i32, count: i32) {
        if let Some(original) = DRAW_ARRAYS.get() {
            unsafe { original(mode, first, count) };
        } else {
            unsafe { (self.0.trampoline())(mode, first, count) };
        }
    }
}

struct DrawElementsTerminal(InlineDetour<GlDrawElementsFn>);

impl snowglass_hook::TerminalLifecycle for DrawElementsTerminal {
    fn enable(&self) -> Result<(), i32> {
        self.0.enable()
    }
    fn disable(&self) -> Result<(), i32> {
        self.0.disable()
    }
}

impl crate::hooks::DrawElementsTerminal for DrawElementsTerminal {
    unsafe fn call(&self, mode: u32, count: i32, element_type: u32, indices: *const c_void) {
        if let Some(original) = DRAW_ELEMENTS.get() {
            unsafe { original(mode, count, element_type, indices) };
        } else {
            unsafe { (self.0.trampoline())(mode, count, element_type, indices) };
        }
    }
}

type SwapBuffersInstallation =
    snowglass_hook::TerminalInstallation<dyn crate::hooks::SwapBuffersTerminal>;
type SwapLayerBuffersInstallation =
    snowglass_hook::TerminalInstallation<dyn crate::hooks::SwapLayerBuffersTerminal>;
type WglGetProcAddressInstallation =
    snowglass_hook::TerminalInstallation<dyn crate::hooks::WglGetProcAddressTerminal>;
type DrawArraysInstallation =
    snowglass_hook::TerminalInstallation<dyn crate::hooks::DrawArraysTerminal>;
type DrawElementsInstallation =
    snowglass_hook::TerminalInstallation<dyn crate::hooks::DrawElementsTerminal>;

struct OpenGlInstallation {
    swap_buffers: Option<SwapBuffersInstallation>,
    swap_layer_buffers: Option<SwapLayerBuffersInstallation>,
    wgl_get_proc_address: Option<WglGetProcAddressInstallation>,
    draw_arrays: Option<DrawArraysInstallation>,
    draw_elements: Option<DrawElementsInstallation>,
    complete: bool,
}

static INSTALLATION: Mutex<Option<OpenGlInstallation>> = Mutex::new(None);
static RUNTIME: Mutex<Option<Arc<OpenGlRuntime>>> = Mutex::new(None);
static DRAW_ARRAYS: OnceLock<GlDrawArraysFn> = OnceLock::new();
static DRAW_ELEMENTS: OnceLock<GlDrawElementsFn> = OnceLock::new();
static DRAW_ELEMENTS_BASE_VERTEX: OnceLock<GlDrawElementsBaseVertexFn> = OnceLock::new();
static BLIT_FRAMEBUFFER: OnceLock<GlBlitFramebufferFn> = OnceLock::new();
static COPY_IMAGE_SUB_DATA: OnceLock<GlCopyImageSubDataFn> = OnceLock::new();

pub(crate) fn tracking() -> Arc<Tracking> {
    RUNTIME
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .map(|runtime| runtime.tracking.clone())
        .unwrap_or_default()
}

pub(crate) fn remember_draw_arrays(address: *const c_void) {
    if address != system_draw_arrays as *const c_void {
        let function = unsafe { std::mem::transmute::<*const c_void, GlDrawArraysFn>(address) };
        let _ = DRAW_ARRAYS.set(function);
    }
}

pub(crate) fn remember_draw_elements(address: *const c_void) {
    if address != system_draw_elements as *const c_void {
        let function = unsafe { std::mem::transmute::<*const c_void, GlDrawElementsFn>(address) };
        let _ = DRAW_ELEMENTS.set(function);
    }
}

pub(crate) fn remember_draw_elements_base_vertex(address: *const c_void) {
    let function =
        unsafe { std::mem::transmute::<*const c_void, GlDrawElementsBaseVertexFn>(address) };
    let _ = DRAW_ELEMENTS_BASE_VERTEX.set(function);
}

pub(crate) fn remember_blit_framebuffer(address: *const c_void) {
    let function = unsafe { std::mem::transmute::<*const c_void, GlBlitFramebufferFn>(address) };
    let _ = BLIT_FRAMEBUFFER.set(function);
}

pub(crate) fn remember_copy_image_sub_data(address: *const c_void) {
    let function = unsafe { std::mem::transmute::<*const c_void, GlCopyImageSubDataFn>(address) };
    let _ = COPY_IMAGE_SUB_DATA.set(function);
}

pub(crate) unsafe fn call_draw_elements_base_vertex(
    mode: u32,
    count: i32,
    element_type: u32,
    indices: *const c_void,
    base_vertex: i32,
) {
    if let Some(original) = DRAW_ELEMENTS_BASE_VERTEX.get() {
        unsafe { original(mode, count, element_type, indices, base_vertex) };
    }
}

pub(crate) unsafe fn call_blit_framebuffer(
    src_x0: i32,
    src_y0: i32,
    src_x1: i32,
    src_y1: i32,
    dst_x0: i32,
    dst_y0: i32,
    dst_x1: i32,
    dst_y1: i32,
    mask: u32,
    filter: u32,
) {
    if let Some(original) = BLIT_FRAMEBUFFER.get() {
        unsafe {
            original(
                src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, mask, filter,
            )
        };
    }
}

#[expect(clippy::too_many_arguments)]
pub(crate) unsafe fn call_copy_image_sub_data(
    src_name: u32,
    src_target: u32,
    src_level: i32,
    src_x: i32,
    src_y: i32,
    src_z: i32,
    dst_name: u32,
    dst_target: u32,
    dst_level: i32,
    dst_x: i32,
    dst_y: i32,
    dst_z: i32,
    width: i32,
    height: i32,
    depth: i32,
) {
    if let Some(original) = COPY_IMAGE_SUB_DATA.get() {
        unsafe {
            original(
                src_name, src_target, src_level, src_x, src_y, src_z, dst_name, dst_target,
                dst_level, dst_x, dst_y, dst_z, width, height, depth,
            )
        };
    }
}

pub(crate) unsafe fn install() -> i32 {
    let mut installation = INSTALLATION
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(installed) = installation.as_mut() {
        if installed.complete {
            return 0;
        }
        if !uninstall_group(installed) {
            return E_FAIL;
        }
        *installation = None;
        *RUNTIME.lock().unwrap_or_else(|error| error.into_inner()) = None;
    }

    let runtime = Arc::new(OpenGlRuntime {
        tracking: Arc::default(),
    });
    *RUNTIME.lock().unwrap_or_else(|error| error.into_inner()) = Some(runtime);
    let mut group = OpenGlInstallation {
        swap_buffers: None,
        swap_layer_buffers: None,
        wgl_get_proc_address: None,
        draw_arrays: None,
        draw_elements: None,
        complete: false,
    };

    macro_rules! install_or_rollback {
        ($field:ident, $install:expr) => {
            match $install {
                Ok(guard) => group.$field = Some(guard),
                Err(_) => {
                    if !uninstall_group(&mut group) {
                        *installation = Some(group);
                    } else {
                        *RUNTIME.lock().unwrap_or_else(|error| error.into_inner()) = None;
                    }
                    return E_FAIL;
                }
            }
        };
    }

    install_or_rollback!(
        wgl_get_proc_address,
        crate::hooks::install_wgl_get_proc_address_terminal(InlineDetour::new(
            system_wgl_get_proc_address as WglGetProcAddressFn,
            crate::hooks::wgl_get_proc_address
        ))
    );
    install_or_rollback!(
        draw_arrays,
        crate::hooks::install_draw_arrays_terminal(DrawArraysTerminal(InlineDetour::new(
            system_draw_arrays as GlDrawArraysFn,
            crate::hooks::draw_arrays
        )))
    );
    install_or_rollback!(
        draw_elements,
        crate::hooks::install_draw_elements_terminal(DrawElementsTerminal(InlineDetour::new(
            system_draw_elements as GlDrawElementsFn,
            crate::hooks::draw_elements
        )))
    );
    install_or_rollback!(
        swap_layer_buffers,
        crate::hooks::install_swap_layer_buffers_terminal(InlineDetour::new(
            system_swap_layer_buffers as SwapLayerBuffersFn,
            crate::hooks::swap_layer_buffers
        ))
    );
    install_or_rollback!(
        swap_buffers,
        crate::hooks::install_swap_buffers_terminal(InlineDetour::new(
            system_swap_buffers as SwapBuffersFn,
            crate::hooks::swap_buffers
        ))
    );

    group.complete = true;
    *installation = Some(group);
    0
}

pub(crate) fn uninstall() -> i32 {
    let mut installation = INSTALLATION
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(installed) = installation.as_mut() else {
        return 0;
    };
    if !uninstall_group(installed) {
        installed.complete = false;
        return E_FAIL;
    }
    *RUNTIME.lock().unwrap_or_else(|error| error.into_inner()) = None;
    *installation = None;
    0
}

fn uninstall_group(installed: &mut OpenGlInstallation) -> bool {
    macro_rules! uninstall {
        ($field:ident) => {
            if installed
                .$field
                .as_ref()
                .is_some_and(|guard| guard.uninstall().is_ok())
            {
                installed.$field = None;
            }
        };
    }
    uninstall!(swap_buffers);
    uninstall!(swap_layer_buffers);
    uninstall!(draw_elements);
    uninstall!(draw_arrays);
    uninstall!(wgl_get_proc_address);
    installed.swap_buffers.is_none()
        && installed.swap_layer_buffers.is_none()
        && installed.draw_elements.is_none()
        && installed.draw_arrays.is_none()
        && installed.wgl_get_proc_address.is_none()
}
