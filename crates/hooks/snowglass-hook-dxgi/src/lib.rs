//! DXGI swapchain interception shared by D3D11 and D3D12.
#![allow(unsafe_code)]

mod hook_chain;
mod hooks;
mod install;
mod runtime;
mod sys;
mod vtable;

pub use runtime::{
    SwapchainCallbacks, install_factory, install_factory_interface, install_factory_interfaces,
    install_swapchain, install_swapchain1,
};

/// Registers producer callbacks with the viewer-injected DXGI hook.
pub fn register_callbacks(callbacks: SwapchainCallbacks) {
    if !unsafe { register_callbacks_with_dxgi_hook(&callbacks) } {
        // Unit tests can link this crate without loading its cdylib. Production
        // injection always loads the central DXGI hook first.
        register_hooks();
        runtime::register_callbacks(callbacks);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassRegisterDxgiCallbacks(callbacks: *const SwapchainCallbacks) {
    let Some(callbacks) = (unsafe { callbacks.as_ref() }) else {
        return;
    };
    runtime::register_callbacks(*callbacks);
}

/// Registers a swapchain created directly by D3D11 with the central hook.
///
/// # Safety
///
/// `swapchain` and `creation_device` follow [`runtime::register_swapchain`].
pub unsafe fn register_swapchain(
    swapchain: *mut std::ffi::c_void,
    creation_device: *mut std::ffi::c_void,
) -> bool {
    type RegisterFn =
        unsafe extern "system" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> bool;
    if let Some(register) =
        unsafe { dxgi_hook_export::<RegisterFn>(c"SnowglassRegisterDxgiSwapchain") }
    {
        return unsafe { register(swapchain, creation_device) };
    }
    register_hooks();
    unsafe { runtime::register_swapchain(swapchain, creation_device) }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassRegisterDxgiSwapchain(
    swapchain: *mut std::ffi::c_void,
    creation_device: *mut std::ffi::c_void,
) -> bool {
    unsafe { runtime::register_swapchain(swapchain, creation_device) }
}

unsafe fn register_callbacks_with_dxgi_hook(callbacks: &SwapchainCallbacks) -> bool {
    type RegisterFn = unsafe extern "system" fn(*const SwapchainCallbacks);
    let Some(register) =
        (unsafe { dxgi_hook_export::<RegisterFn>(c"SnowglassRegisterDxgiCallbacks") })
    else {
        return false;
    };
    unsafe { register(callbacks) };
    true
}

unsafe fn dxgi_hook_export<T: Copy>(name: &std::ffi::CStr) -> Option<T> {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

    let wide = "snowglass_hook_dxgi.dll\0"
        .encode_utf16()
        .collect::<Vec<_>>();
    let module = unsafe { GetModuleHandleW(wide.as_ptr()) };
    if module.is_null() {
        return None;
    }
    let address = unsafe { GetProcAddress(module, name.as_ptr().cast()) }?;
    Some(unsafe { std::mem::transmute_copy(&address) })
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallDxgiHook() -> i32 {
    if unsafe { install_factory_detours() } {
        0
    } else {
        windows_sys::Win32::Foundation::E_FAIL
    }
}

/// Installs process-wide detours over the DXGI factory creation exports so
/// every factory the application creates receives the per-object vtable hooks
/// at creation time.
///
/// # Safety
///
/// The process must have a usable system DXGI runtime, and callers must not
/// concurrently unload the DXGI module while installation is in progress.
pub unsafe fn install_factory_detours() -> bool {
    unsafe { install::install() }
}

pub(crate) fn dxgi_log(message: impl std::fmt::Display) {
    use std::io::Write;
    eprintln!("{message}");
    let Some(path) = snowglass_hook::config::dxgi_log_file() else {
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

snowglass_hook::register_hooks! {
    hooks::register_create_factory -> runtime::created_factory;
    hooks::register_create_factory1 -> runtime::created_factory;
    hooks::register_create_factory2 -> runtime::created_factory;
    hooks::register_query_interface -> runtime::query_interface;
    hooks::register_factory_create_swapchain -> runtime::factory_created_swapchain;
    hooks::register_factory_create_swapchain_for_hwnd -> runtime::factory_created_swapchain;
    hooks::register_factory_create_swapchain_for_core_window -> runtime::factory_created_swapchain;
    hooks::register_factory_create_swapchain_for_composition -> runtime::factory_created_swapchain;
    hooks::register_present -> runtime::present;
    hooks::register_present1 -> runtime::present1;
    hooks::register_resize_buffers -> runtime::resize_buffers;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vtable::Vtable;
    use std::ffi::c_void;
    use windows::{
        Win32::Graphics::Dxgi::{IDXGISwapChain_Vtbl, IDXGISwapChain1_Vtbl},
        core::IUnknown_Vtbl,
    };

    const SLOT_SIZE: usize = size_of::<usize>();
    const SWAPCHAIN_VTABLE_LEN: usize = <IDXGISwapChain_Vtbl as Vtable>::SLOTS;
    const SWAPCHAIN1_VTABLE_LEN: usize = <IDXGISwapChain1_Vtbl as Vtable>::SLOTS;
    const QUERY_INTERFACE_INDEX: usize =
        std::mem::offset_of!(IUnknown_Vtbl, QueryInterface) / SLOT_SIZE;
    const RELEASE_INDEX: usize = std::mem::offset_of!(IUnknown_Vtbl, Release) / SLOT_SIZE;
    const PRESENT_INDEX: usize = std::mem::offset_of!(IDXGISwapChain_Vtbl, Present) / SLOT_SIZE;
    const RESIZE_BUFFERS_INDEX: usize =
        std::mem::offset_of!(IDXGISwapChain_Vtbl, ResizeBuffers) / SLOT_SIZE;
    const PRESENT1_INDEX: usize = std::mem::offset_of!(IDXGISwapChain1_Vtbl, Present1) / SLOT_SIZE;

    unsafe extern "system" fn release(_this: *mut c_void) -> u32 {
        0
    }

    unsafe extern "system" fn query_interface(
        _this: *mut c_void,
        _interface_id: *const c_void,
        _output: *mut *mut c_void,
    ) -> i32 {
        0x8000_4002u32 as i32
    }

    unsafe extern "system" fn present(_this: *mut c_void, sync: u32, flags: u32) -> i32 {
        (sync + flags) as i32
    }

    unsafe extern "system" fn resize(
        _this: *mut c_void,
        buffers: u32,
        width: u32,
        height: u32,
        format: u32,
        flags: u32,
    ) -> i32 {
        (buffers + width + height + format + flags) as i32
    }

    #[test]
    fn patches_and_dispatches_swapchain_vtable() {
        let mut original = [0usize; SWAPCHAIN_VTABLE_LEN];
        original[QUERY_INTERFACE_INDEX] = query_interface as *const () as usize;
        original[RELEASE_INDEX] = release as *const () as usize;
        original[PRESENT_INDEX] = present as *const () as usize;
        original[RESIZE_BUFFERS_INDEX] = resize as *const () as usize;
        let mut object_vtable = original.as_ptr();
        let object = (&raw mut object_vtable).cast::<c_void>();

        assert!(unsafe { install_swapchain(object) });
        assert_ne!(object_vtable, original.as_ptr());

        let patched_present = unsafe {
            std::mem::transmute::<usize, crate::vtable::PresentFn>(
                *object_vtable.add(PRESENT_INDEX),
            )
        };
        let patched_resize = unsafe {
            std::mem::transmute::<usize, crate::vtable::ResizeBuffersFn>(
                *object_vtable.add(RESIZE_BUFFERS_INDEX),
            )
        };
        let patched_release = unsafe {
            std::mem::transmute::<usize, crate::vtable::ReleaseFn>(
                *object_vtable.add(RELEASE_INDEX),
            )
        };
        assert_eq!(unsafe { patched_present(object, 2, 3) }, 5);
        assert_eq!(unsafe { patched_resize(object, 1, 2, 3, 4, 5) }, 15);
        assert_eq!(unsafe { patched_release(object) }, 0);
    }

    unsafe extern "system" fn present1(
        _this: *mut c_void,
        sync: u32,
        flags: u32,
        _parameters: *const c_void,
    ) -> i32 {
        (sync + flags + 7) as i32
    }

    #[test]
    fn upgrades_and_dispatches_swapchain1_vtable() {
        let mut original = [0usize; SWAPCHAIN1_VTABLE_LEN];
        original[QUERY_INTERFACE_INDEX] = query_interface as *const () as usize;
        original[RELEASE_INDEX] = release as *const () as usize;
        original[PRESENT_INDEX] = present as *const () as usize;
        original[RESIZE_BUFFERS_INDEX] = resize as *const () as usize;
        original[PRESENT1_INDEX] = present1 as *const () as usize;
        let mut object_vtable = original.as_ptr();
        let object = (&raw mut object_vtable).cast::<c_void>();

        assert!(unsafe { install_swapchain(object) });
        assert!(unsafe { install_swapchain1(object) });
        let patched_present1 = unsafe {
            std::mem::transmute::<usize, crate::vtable::Present1Fn>(
                *object_vtable.add(PRESENT1_INDEX),
            )
        };
        assert_eq!(
            unsafe { patched_present1(object, 2, 3, std::ptr::null()) },
            12
        );
    }
}
