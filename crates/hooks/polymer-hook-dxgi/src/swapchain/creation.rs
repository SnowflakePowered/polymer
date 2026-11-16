use parking_lot::RwLock;
use std::{ffi::c_void, sync::LazyLock};
use windows::Win32::Graphics::Dxgi::*;

/// Borrowed factory creation metadata. All pointers remain live only for the
/// callback. A backend retains canonical COM references before returning.
#[repr(C)]
pub struct SwapchainCreateInfo {
    pub factory: *mut c_void,
    pub device_or_queue: *mut c_void,
    pub window: *mut c_void,
    pub description: DXGI_SWAP_CHAIN_DESC1,
    pub fullscreen: *const DXGI_SWAP_CHAIN_FULLSCREEN_DESC,
    pub restrict_to_output: *mut c_void,
    pub legacy: bool,
}

/// S_FALSE declines without changing output, S_OK returns an owned swapchain,
/// and a failure HRESULT rejects a matched creation before any native buffers
/// become visible. Invoked only in explicit swapchain mode.
pub type VirtualSwapchainClaim =
    unsafe extern "system" fn(*const SwapchainCreateInfo, *mut *mut c_void) -> i32;
static CLAIMS: LazyLock<RwLock<Vec<VirtualSwapchainClaim>>> = LazyLock::new(Default::default);

pub(crate) struct CreationDispatch;
impl CreationDispatch {
    pub fn register(claim: VirtualSwapchainClaim) {
        let mut claims = CLAIMS.write();
        if !claims
            .iter()
            .any(|existing| *existing as usize == claim as usize)
        {
            claims.push(claim);
        }
    }
    pub unsafe fn create(info: &SwapchainCreateInfo, output: *mut *mut c_void) -> i32 {
        if let Err(error) = polymer_hook::config::require_swapchain() {
            tracing::error!(%error, "invalid DXGI presentation configuration");
            return windows::Win32::Foundation::E_INVALIDARG.0;
        }
        if output.is_null() {
            return windows::Win32::Foundation::E_POINTER.0;
        }
        unsafe {
            output.write(std::ptr::null_mut());
        }
        // No registry lock survives a backend call (driver calls can reenter).
        let claims = CLAIMS.read().clone();
        for claim in claims {
            let mut created = std::ptr::null_mut();
            let result = unsafe { claim(info, &mut created) };
            if result == 1 {
                continue;
            }
            if result == 0 && created.is_null() {
                tracing::error!("virtual DXGI backend returned success without an object");
                return windows::Win32::Foundation::E_FAIL.0;
            }
            unsafe {
                output.write(created);
            }
            return result;
        }
        tracing::error!(window = ?info.window, "virtual DXGI creation unclaimed");
        DXGI_ERROR_UNSUPPORTED.0
    }
}

pub(crate) unsafe fn create_legacy(
    factory: *mut c_void,
    device: *mut c_void,
    desc: *const c_void,
    output: *mut *mut c_void,
    _next: crate::vtable::FactoryCreateSwapchainContext<'_>,
) -> i32 {
    if desc.is_null() {
        return windows::Win32::Foundation::E_POINTER.0;
    }
    let native = unsafe { &*desc.cast::<DXGI_SWAP_CHAIN_DESC>() };
    let fullscreen = DXGI_SWAP_CHAIN_FULLSCREEN_DESC {
        RefreshRate: native.BufferDesc.RefreshRate,
        ScanlineOrdering: native.BufferDesc.ScanlineOrdering,
        Scaling: native.BufferDesc.Scaling,
        Windowed: native.Windowed,
    };
    let info = SwapchainCreateInfo {
        factory,
        device_or_queue: device,
        window: native.OutputWindow.0,
        description: DXGI_SWAP_CHAIN_DESC1 {
            Width: native.BufferDesc.Width,
            Height: native.BufferDesc.Height,
            Format: native.BufferDesc.Format,
            Stereo: windows::core::BOOL(0),
            SampleDesc: native.SampleDesc,
            BufferUsage: native.BufferUsage,
            BufferCount: native.BufferCount,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: native.SwapEffect,
            AlphaMode: Common::DXGI_ALPHA_MODE_IGNORE,
            Flags: native.Flags,
        },
        fullscreen: &fullscreen,
        restrict_to_output: std::ptr::null_mut(),
        legacy: true,
    };
    unsafe { CreationDispatch::create(&info, output) }
}

#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn create_for_hwnd(
    factory: *mut c_void,
    device: *mut c_void,
    window: *mut c_void,
    desc: *const c_void,
    fullscreen: *const c_void,
    restrict_to_output: *mut c_void,
    output: *mut *mut c_void,
    _next: crate::vtable::FactoryCreateSwapchainForHwndContext<'_>,
) -> i32 {
    if desc.is_null() {
        return windows::Win32::Foundation::E_POINTER.0;
    }
    let info = SwapchainCreateInfo {
        factory,
        device_or_queue: device,
        window,
        description: unsafe { *desc.cast::<DXGI_SWAP_CHAIN_DESC1>() },
        fullscreen: fullscreen.cast(),
        restrict_to_output,
        legacy: false,
    };
    unsafe { CreationDispatch::create(&info, output) }
}
