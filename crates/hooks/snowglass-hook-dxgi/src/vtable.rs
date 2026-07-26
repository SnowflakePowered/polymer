#![allow(clippy::missing_transmute_annotations)]

use std::ffi::c_void;
use windows::{
    Win32::Graphics::Dxgi::{
        IDXGIFactory, IDXGIFactory1, IDXGIFactory2, IDXGIFactory3, IDXGIFactory4, IDXGIFactory5,
        IDXGIFactory6, IDXGIFactory7, IDXGIFactoryMedia, IDXGISwapChain, IDXGISwapChain1,
        IDXGISwapChain2, IDXGISwapChain3, IDXGISwapChain4,
    },
    core::IUnknown,
};

snowglass_hook::vtable! {
    [
        IDXGISwapChain,
        IDXGISwapChain1,
        IDXGISwapChain2,
        IDXGISwapChain3,
        IDXGISwapChain4,
    ],
    [
        IDXGIFactory,
        IDXGIFactory1,
        IDXGIFactory2,
        IDXGIFactory3,
        IDXGIFactory4,
        IDXGIFactory5,
        IDXGIFactory6,
        IDXGIFactory7,
    ],
    // Not part of the `IDXGIFactory` chain: it is queried off a factory and
    // carries its own vtable, so it needs a hierarchy of its own.
    [IDXGIFactoryMedia],
}

snowglass_hook::com_vtable! {
    pub(crate) union IDXGISwapchainVtable for IDXGISwapChain4;
}

snowglass_hook::com_vtable! {
    /// Overlays every factory interface generation on one allocation, mirroring
    /// [`IDXGISwapchainVtable`].
    pub(crate) union IDXGIFactoryVtable for IDXGIFactory7;
}

snowglass_hook::com_vtable! {
    /// `IDXGIFactoryMedia` is reached by querying a factory rather than by
    /// deriving from one, so its swapchain creation needs a separate vtable.
    pub(crate) union IDXGIFactoryMediaVtable for IDXGIFactoryMedia;
}

pub(crate) struct SwapchainContext {
    pub(crate) owner: std::sync::Arc<
        std::sync::RwLock<Option<std::sync::Arc<crate::runtime::SwapchainOwnerHandle>>>,
    >,
    pub(crate) allow_tearing: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

snowglass_hook::impl_com_hook! {
    pub(crate) trait SwapchainHook;
    impl SwapchainHook for IDXGISwapchainHook {
        #![vtable = IDXGISwapchainVtable]
        #[interfaces] {
            IDXGISwapChain4,
            IDXGISwapChain3,
            IDXGISwapChain2,
            IDXGISwapChain1,
            IDXGISwapChain,
        }
        #[state = SwapchainContext]
        #[release(destroyed = crate::runtime::swapchain_destroyed)]
        unsafe extern "system" fn swapchain_release(this: *mut c_void) -> u32;

        #[base = IUnknown::QueryInterface]
        #[fallback = 0x8000_4002u32 as i32]
        #[infix(crate::runtime::swapchain_query_interface)]
        unsafe extern "system" fn swapchain_query_interface(this: *mut c_void, interface_id: *const c_void, output: *mut *mut c_void) -> i32;

        #[base = IDXGISwapChain::Present]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[infix(crate::runtime::present)]
        unsafe extern "system" fn present(this: *mut c_void, sync_interval: u32, flags: u32) -> i32 {
            #[context] |state| {
                owner: Option<std::sync::Arc<crate::runtime::SwapchainOwnerHandle>> = state.state().owner.read().unwrap_or_else(|error| error.into_inner()).clone(),
                allow_tearing: bool = state.state().allow_tearing.load(std::sync::atomic::Ordering::Relaxed),
            }
        }

        #[base = IDXGISwapChain::ResizeBuffers]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[infix(crate::runtime::resize_buffers)]
        unsafe extern "system" fn resize_buffers(this: *mut c_void, buffer_count: u32, width: u32, height: u32, format: u32, flags: u32) -> i32 {
            #[context] |state| {
                owner: Option<std::sync::Arc<crate::runtime::SwapchainOwnerHandle>> = state.state().owner.read().unwrap_or_else(|error| error.into_inner()).clone(),
                allow_tearing: std::sync::Arc<std::sync::atomic::AtomicBool> = state.state().allow_tearing.clone(),
            }
        }

        #[base = IDXGISwapChain1::Present1]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[infix(crate::runtime::present1)]
        unsafe extern "system" fn present1(this: *mut c_void, sync_interval: u32, flags: u32, parameters: *const c_void) -> i32 {
            #[context] |state| {
                owner: Option<std::sync::Arc<crate::runtime::SwapchainOwnerHandle>> = state.state().owner.read().unwrap_or_else(|error| error.into_inner()).clone(),
                allow_tearing: bool = state.state().allow_tearing.load(std::sync::atomic::Ordering::Relaxed),
            }
        }
    }
}

impl Clone for SwapchainContext {
    fn clone(&self) -> Self {
        Self {
            owner: self.owner.clone(),
            allow_tearing: self.allow_tearing.clone(),
        }
    }
}

impl Default for SwapchainContext {
    fn default() -> Self {
        Self {
            owner: std::sync::Arc::new(std::sync::RwLock::new(None)),
            allow_tearing: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

impl SwapchainContext {
    pub(crate) fn new(allow_tearing: bool) -> Self {
        Self {
            allow_tearing: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(allow_tearing)),
            ..Self::default()
        }
    }

    pub(crate) fn set_owner(
        &self,
        owner: Option<std::sync::Arc<crate::runtime::SwapchainOwnerHandle>>,
    ) {
        *self
            .owner
            .write()
            .unwrap_or_else(|error| error.into_inner()) = owner;
    }
}

snowglass_hook::impl_com_hook! {
    pub(crate) trait FactoryHook;
    impl FactoryHook for IDXGIFactoryHook {
        #![vtable = IDXGIFactoryVtable]
        #[interfaces] {
            IDXGIFactory7,
            IDXGIFactory6,
            IDXGIFactory5,
            IDXGIFactory4,
            IDXGIFactory3,
            IDXGIFactory2,
            IDXGIFactory1,
            IDXGIFactory,
        }
        #[release]
        unsafe extern "system" fn factory_release(this: *mut c_void) -> u32;

        #[base = IUnknown::QueryInterface]
        #[fallback = 0x8000_4002u32 as i32]
        #[infix(crate::runtime::factory_query_interface)]
        unsafe extern "system" fn factory_query_interface(this: *mut c_void, interface_id: *const c_void, output: *mut *mut c_void) -> i32;

        #[base = IDXGIFactory::CreateSwapChain]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[postfix(crate::runtime::factory_created_swapchain(device, swapchain, result))]
        unsafe extern "system" fn factory_create_swapchain(this: *mut c_void, device: *mut c_void, desc: *const c_void, swapchain: *mut *mut c_void) -> i32;

        #[base = IDXGIFactory2::CreateSwapChainForHwnd]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[postfix(crate::runtime::factory_created_swapchain(device, swapchain, result))]
        unsafe extern "system" fn factory_create_swapchain_for_hwnd(this: *mut c_void, device: *mut c_void, window: *mut c_void, desc: *const c_void, fullscreen_desc: *const c_void, restrict_to_output: *mut c_void, swapchain: *mut *mut c_void) -> i32;

        #[base = IDXGIFactory2::CreateSwapChainForCoreWindow]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[postfix(crate::runtime::factory_created_swapchain(device, swapchain, result))]
        unsafe extern "system" fn factory_create_swapchain_for_core_window(this: *mut c_void, device: *mut c_void, window: *mut c_void, desc: *const c_void, restrict_to_output: *mut c_void, swapchain: *mut *mut c_void) -> i32;

        #[base = IDXGIFactory2::CreateSwapChainForComposition]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[postfix(crate::runtime::factory_created_swapchain(device, swapchain, result))]
        unsafe extern "system" fn factory_create_swapchain_for_composition(this: *mut c_void, device: *mut c_void, desc: *const c_void, restrict_to_output: *mut c_void, swapchain: *mut *mut c_void) -> i32;
    }
}

// Chromium, and anything else presenting through DirectComposition surface
// handles, creates its swapchain through `IDXGIFactoryMedia` rather than any of
// the `IDXGIFactory2` entry points. Without this hook the swapchain is never
// seen and such a process appears to present nothing at all.
snowglass_hook::impl_com_hook! {
    pub(crate) trait FactoryMediaHook;
    impl FactoryMediaHook for IDXGIFactoryMediaHook {
        #![vtable = IDXGIFactoryMediaVtable]
        #[interfaces] {
            IDXGIFactoryMedia,
        }
        #[release]
        unsafe extern "system" fn factory_media_release(this: *mut c_void) -> u32;

        #[base = IDXGIFactoryMedia::CreateSwapChainForCompositionSurfaceHandle]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[postfix(crate::runtime::factory_created_swapchain(device, swapchain, result))]
        unsafe extern "system" fn factory_media_create_swapchain_for_composition_surface_handle(this: *mut c_void, device: *mut c_void, surface: *mut c_void, desc: *const c_void, restrict_to_output: *mut c_void, swapchain: *mut *mut c_void) -> i32;
    }
}
