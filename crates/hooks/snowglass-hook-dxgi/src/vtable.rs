#![allow(clippy::missing_transmute_annotations)]

use std::ffi::c_void;
use windows::{
    Win32::Graphics::Dxgi::{
        IDXGIFactory_Vtbl, IDXGIFactory1_Vtbl, IDXGIFactory2_Vtbl, IDXGIFactory3_Vtbl,
        IDXGIFactory4_Vtbl, IDXGIFactory5_Vtbl, IDXGIFactory6_Vtbl, IDXGIFactory7_Vtbl,
        IDXGISwapChain_Vtbl, IDXGISwapChain1_Vtbl, IDXGISwapChain2_Vtbl, IDXGISwapChain3_Vtbl,
        IDXGISwapChain4_Vtbl,
    },
    core::IUnknown_Vtbl,
};

#[cfg(test)]
pub(crate) type PresentFn = unsafe extern "system" fn(*mut c_void, u32, u32) -> i32;
#[cfg(test)]
pub(crate) type ResizeBuffersFn =
    unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32, u32) -> i32;
#[cfg(test)]
pub(crate) type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;
#[cfg(test)]
pub(crate) type Present1Fn = unsafe extern "system" fn(*mut c_void, u32, u32, *const c_void) -> i32;

snowglass_hook::vtable! {
    [
        IDXGISwapChain_Vtbl,
        IDXGISwapChain1_Vtbl,
        IDXGISwapChain2_Vtbl,
        IDXGISwapChain3_Vtbl,
        IDXGISwapChain4_Vtbl,
    ],
    [
        IDXGIFactory_Vtbl,
        IDXGIFactory1_Vtbl,
        IDXGIFactory2_Vtbl,
        IDXGIFactory3_Vtbl,
        IDXGIFactory4_Vtbl,
        IDXGIFactory5_Vtbl,
        IDXGIFactory6_Vtbl,
        IDXGIFactory7_Vtbl,
    ],
}

snowglass_hook::com_vtable! {
    pub(crate) union IDXGISwapchainVtable for IDXGISwapChain4_Vtbl;
}

snowglass_hook::com_vtable! {
    /// Overlays every factory interface generation on one allocation, mirroring
    /// [`IDXGISwapchainVtable`].
    pub(crate) union IDXGIFactoryVtable for IDXGIFactory7_Vtbl;
}

snowglass_hook::impl_com_hook! {
    pub(crate) trait SwapchainHook;
    impl SwapchainHook for IDXGISwapchainHook {
        #![vtable = IDXGISwapchainVtable]
        #[registry] pub(crate) static SWAPCHAINS;
        #[context]
        pub(crate) struct SwapchainContext {
            pub(crate) owner: std::sync::RwLock<
                Option<std::sync::Arc<crate::runtime::SwapchainOwnerHandle>>,
            >,
        }
        #[release(destroyed = crate::runtime::swapchain_destroyed)]
        unsafe extern "system" fn swapchain_release(this: *mut c_void) -> u32;

        #[vtable = IUnknown_Vtbl]
        #[base = QueryInterface]
        #[fallback = 0x8000_4002u32 as i32]
        #[infix(crate::runtime::swapchain_query_interface)]
        unsafe extern "system" fn swapchain_query_interface(this: *mut c_void, interface_id: *const c_void, output: *mut *mut c_void) -> i32 {
            #[context] |state| {
                owner: Option<std::sync::Arc<crate::runtime::SwapchainOwnerHandle>> = state.common().owner.read().unwrap_or_else(|error| error.into_inner()).clone(),
            }
        }

        #[vtable = IDXGISwapChain_Vtbl]
        #[base = Present]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[prefix(|context| unsafe {
            crate::runtime::before_present(this, context.owner)
        })]
        unsafe extern "system" fn present(this: *mut c_void, sync_interval: u32, flags: u32) -> i32 {
            #[context] |state| {
                owner: Option<std::sync::Arc<crate::runtime::SwapchainOwnerHandle>> = state.common().owner.read().unwrap_or_else(|error| error.into_inner()).clone(),
            }
        }

        #[vtable = IDXGISwapChain_Vtbl]
        #[base = ResizeBuffers]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[infix(crate::runtime::resize_buffers)]
        unsafe extern "system" fn resize_buffers(this: *mut c_void, buffer_count: u32, width: u32, height: u32, format: u32, flags: u32) -> i32 {
            #[context] |state| {
                owner: Option<std::sync::Arc<crate::runtime::SwapchainOwnerHandle>> = state.common().owner.read().unwrap_or_else(|error| error.into_inner()).clone(),
            }
        }

        #[vtable = IDXGISwapChain1_Vtbl]
        #[base = Present1]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[prefix(|context| unsafe {
            crate::runtime::before_present(this, context.owner)
        })]
        unsafe extern "system" fn present1(this: *mut c_void, sync_interval: u32, flags: u32, parameters: *const c_void) -> i32 {
            #[context] |state| {
                owner: Option<std::sync::Arc<crate::runtime::SwapchainOwnerHandle>> = state.common().owner.read().unwrap_or_else(|error| error.into_inner()).clone(),
            }
        }
    }
}

snowglass_hook::impl_com_hook! {
    pub(crate) trait FactoryHook;
    impl FactoryHook for IDXGIFactoryHook {
        #![vtable = IDXGIFactoryVtable]
        #[registry] pub(crate) static FACTORIES;
        #[release]
        unsafe extern "system" fn factory_release(this: *mut c_void) -> u32;

        #[vtable = IUnknown_Vtbl]
        #[base = QueryInterface]
        #[fallback = 0x8000_4002u32 as i32]
        #[infix(crate::runtime::factory_query_interface)]
        unsafe extern "system" fn factory_query_interface(this: *mut c_void, interface_id: *const c_void, output: *mut *mut c_void) -> i32;

        #[vtable = IDXGIFactory_Vtbl]
        #[base = CreateSwapChain]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[postfix(crate::runtime::factory_created_swapchain(device, swapchain, result))]
        unsafe extern "system" fn factory_create_swapchain(this: *mut c_void, device: *mut c_void, desc: *const c_void, swapchain: *mut *mut c_void) -> i32;

        #[vtable = IDXGIFactory2_Vtbl]
        #[base = CreateSwapChainForHwnd]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[postfix(crate::runtime::factory_created_swapchain(device, swapchain, result))]
        unsafe extern "system" fn factory_create_swapchain_for_hwnd(this: *mut c_void, device: *mut c_void, window: *mut c_void, desc: *const c_void, fullscreen_desc: *const c_void, restrict_to_output: *mut c_void, swapchain: *mut *mut c_void) -> i32;

        #[vtable = IDXGIFactory2_Vtbl]
        #[base = CreateSwapChainForCoreWindow]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[postfix(crate::runtime::factory_created_swapchain(device, swapchain, result))]
        unsafe extern "system" fn factory_create_swapchain_for_core_window(this: *mut c_void, device: *mut c_void, window: *mut c_void, desc: *const c_void, restrict_to_output: *mut c_void, swapchain: *mut *mut c_void) -> i32;

        #[vtable = IDXGIFactory2_Vtbl]
        #[base = CreateSwapChainForComposition]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[postfix(crate::runtime::factory_created_swapchain(device, swapchain, result))]
        unsafe extern "system" fn factory_create_swapchain_for_composition(this: *mut c_void, device: *mut c_void, desc: *const c_void, restrict_to_output: *mut c_void, swapchain: *mut *mut c_void) -> i32;
    }
}
