//! DXGI swapchain interception shared by D3D11 and D3D12.
#![allow(unsafe_code)]

mod hooks;
mod install;
mod runtime;
mod vtable;

pub use runtime::{
    SwapchainClaim, SwapchainOwner, SwapchainOwnerHandle, install_factory,
    install_factory_interface, install_factory_interfaces,
};

/// Registers a backend's swapchain claim with the viewer-injected DXGI hook.
pub fn register_claim(claim: SwapchainClaim) {
    if !unsafe { register_claim_with_dxgi_hook(claim) } {
        // Unit tests can link this crate without loading its cdylib. Production
        // injection always loads the central DXGI hook first.
        runtime::register_claim(claim);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassRegisterDxgiClaim(claim: SwapchainClaim) {
    runtime::register_claim(claim);
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
    unsafe { runtime::register_swapchain(swapchain, creation_device) }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassRegisterDxgiSwapchain(
    swapchain: *mut std::ffi::c_void,
    creation_device: *mut std::ffi::c_void,
) -> bool {
    unsafe { runtime::register_swapchain(swapchain, creation_device) }
}

unsafe fn register_claim_with_dxgi_hook(claim: SwapchainClaim) -> bool {
    type RegisterFn = unsafe extern "system" fn(SwapchainClaim);
    let Some(register) = (unsafe { dxgi_hook_export::<RegisterFn>(c"SnowglassRegisterDxgiClaim") })
    else {
        return false;
    };
    unsafe { register(claim) };
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
    // These detours only see factories created after they are installed, so
    // record whether DXGI was already running when this process was hooked.
    // A process injected after it built its factory reports a loaded module
    // here and then never logs a created factory, which is the signature of
    // arriving too late rather than of the detours failing.
    let already_loaded = unsafe { dxgi_module_loaded() };
    dxgi_log(format!(
        "install requested in process {} (dxgi already loaded: {already_loaded})",
        std::process::id()
    ));
    if unsafe { install_factory_detours() } {
        dxgi_log("factory creation detours installed");
        0
    } else {
        dxgi_log("factory creation detours failed to install");
        windows_sys::Win32::Foundation::E_FAIL
    }
}

/// Whether `dxgi.dll` is already mapped, meaning the process may well have
/// created its factory and swapchain before this hook arrived.
unsafe fn dxgi_module_loaded() -> bool {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    let wide = "dxgi.dll\0".encode_utf16().collect::<Vec<_>>();
    !unsafe { GetModuleHandleW(wide.as_ptr()) }.is_null()
}

#[unsafe(no_mangle)]
pub extern "system" fn SnowglassUninstallDxgiHook() -> i32 {
    if install::uninstall() {
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

/// Removes the process-wide DXGI factory detours.
///
/// A later call to [`install_factory_detours`] installs a fresh terminal
/// generation and enables all three detours again.
pub fn uninstall_factory_detours() -> bool {
    install::uninstall()
}

#[cfg(test)]
mod tests {
    use crate::vtable::Vtable;
    use std::ffi::c_void;
    use windows::{
        Win32::Graphics::Dxgi::{IDXGISwapChain, IDXGISwapChain_Vtbl},
        core::IUnknown_Vtbl,
    };

    const SLOT_SIZE: usize = size_of::<usize>();
    const SWAPCHAIN_VTABLE_LEN: usize = <IDXGISwapChain as Vtable>::SLOTS;
    const QUERY_INTERFACE_INDEX: usize =
        std::mem::offset_of!(IUnknown_Vtbl, QueryInterface) / SLOT_SIZE;
    const RELEASE_INDEX: usize = std::mem::offset_of!(IUnknown_Vtbl, Release) / SLOT_SIZE;
    const PRESENT_INDEX: usize = std::mem::offset_of!(IDXGISwapChain_Vtbl, Present) / SLOT_SIZE;
    const RESIZE_BUFFERS_INDEX: usize =
        std::mem::offset_of!(IDXGISwapChain_Vtbl, ResizeBuffers) / SLOT_SIZE;

    // Stand-ins for the slots the hook trampolines through. They are never
    // called here, so they return nothing meaningful: this test is about which
    // vtable the object points at, not about what the hook forwards.
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

    unsafe extern "system" fn present(_this: *mut c_void, _sync: u32, _flags: u32) -> i32 {
        0
    }

    unsafe extern "system" fn resize(
        _this: *mut c_void,
        _buffers: u32,
        _width: u32,
        _height: u32,
        _format: u32,
        _flags: u32,
    ) -> i32 {
        0
    }

    /// Installing the hook must move the object onto a vtable of the hook's
    /// own, leaving the original untouched. Everything downstream depends on
    /// that swap: if the object still points at the application's vtable, no
    /// present is ever intercepted.
    #[test]
    fn install_moves_the_object_onto_a_patched_vtable() {
        let mut original = [0usize; SWAPCHAIN_VTABLE_LEN];
        original[QUERY_INTERFACE_INDEX] = query_interface as *const () as usize;
        original[RELEASE_INDEX] = release as *const () as usize;
        original[PRESENT_INDEX] = present as *const () as usize;
        original[RESIZE_BUFFERS_INDEX] = resize as *const () as usize;
        let mut object_vtable = original.as_ptr();
        let object = (&raw mut object_vtable).cast::<c_void>();

        // SAFETY: `object` points at a vtable pointer whose table is fully
        // sized for the interface being installed.
        assert!(unsafe {
            crate::vtable::IDXGISwapchainHook::install_object::<IDXGISwapChain>(
                object,
                crate::vtable::SwapchainContext::default(),
            )
        });
        assert_ne!(
            object_vtable,
            original.as_ptr(),
            "the object should point at the hook's vtable, not the original"
        );
        assert_eq!(
            original[PRESENT_INDEX], present as *const () as usize,
            "the application's own vtable should be left alone"
        );
    }
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
