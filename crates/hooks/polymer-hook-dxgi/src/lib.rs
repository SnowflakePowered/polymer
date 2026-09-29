//! DXGI swapchain interception shared by D3D11 and D3D12.
#![allow(unsafe_code)]

mod hooks;
mod install;
mod runtime;
mod vtable;

pub use runtime::{install_factory, install_factory_interface, install_factory_interfaces};

unsafe fn dxgi_hook_export<T: Copy>(name: &std::ffi::CStr) -> Option<T> {
    let wide = "polymer_hook_dxgi.dll\0".encode_utf16().collect::<Vec<_>>();
    let module = polymer_hook::module::loaded(&wide)?;
    unsafe { polymer_hook::module::symbol(module, name) }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn PolymerInstallDxgiHook() -> i32 {
    polymer_hook::telemetry::init_file_tracing(polymer_hook::config::log_file(
        polymer_hook::config::DXGI_LOG_FILE,
    ));
    if let Err(error) = polymer_hook::config::require_swapchain() {
        tracing::error!(%error, "DXGI presentation configuration rejected");
        return windows_sys::Win32::Foundation::E_INVALIDARG;
    }
    // These detours only see factories created after they are installed, so
    // record whether DXGI was already running when this process was hooked.
    // A process injected after it built its factory reports a loaded module
    // here and then never logs a created factory, which is the signature of
    // arriving too late rather than of the detours failing.
    let already_loaded = unsafe { dxgi_module_loaded() };
    tracing::debug!(
        "install requested in process {} (dxgi already loaded: {already_loaded})",
        std::process::id()
    );
    if unsafe { install::install() } {
        tracing::debug!("factory creation detours installed");
        0
    } else {
        tracing::debug!("factory creation detours failed to install");
        windows_sys::Win32::Foundation::E_FAIL
    }
}

/// Whether `dxgi.dll` is already mapped, meaning the process may well have
/// created its factory and swapchain before this hook arrived.
unsafe fn dxgi_module_loaded() -> bool {
    let wide = "dxgi.dll\0".encode_utf16().collect::<Vec<_>>();
    polymer_hook::module::loaded(&wide).is_some()
}

#[unsafe(no_mangle)]
pub extern "system" fn PolymerUninstallDxgiHook() -> i32 {
    if install::uninstall() {
        0
    } else {
        windows_sys::Win32::Foundation::E_FAIL
    }
}

pub mod swapchain;
pub use swapchain::{SwapchainCreateInfo, VirtualSwapchainClaim};

pub fn register_virtual_claim(claim: VirtualSwapchainClaim) {
    type Register = unsafe extern "system" fn(VirtualSwapchainClaim);
    if let Some(register) =
        unsafe { dxgi_hook_export::<Register>(c"PolymerRegisterVirtualDxgiClaim") }
    {
        unsafe {
            register(claim);
        }
    } else {
        swapchain::CreationDispatch::register(claim);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn PolymerRegisterVirtualDxgiClaim(claim: VirtualSwapchainClaim) {
    swapchain::CreationDispatch::register(claim);
}
