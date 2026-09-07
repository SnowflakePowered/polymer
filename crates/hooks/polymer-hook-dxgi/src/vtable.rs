#![allow(clippy::missing_transmute_annotations)]
use std::ffi::c_void;
use windows::{
    Win32::Graphics::Dxgi::{
        IDXGIFactory, IDXGIFactory1, IDXGIFactory2, IDXGIFactory3, IDXGIFactory4, IDXGIFactory5,
        IDXGIFactory6, IDXGIFactory7,
    },
    core::IUnknown,
};
polymer_hook::vtable! {
    [IDXGIFactory, IDXGIFactory1, IDXGIFactory2, IDXGIFactory3,
     IDXGIFactory4, IDXGIFactory5, IDXGIFactory6, IDXGIFactory7],
}
polymer_hook::com_vtable! {
    pub(crate) union IDXGIFactoryVtable for IDXGIFactory7;
}

polymer_hook::impl_com_hook! {
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
        #[infix(crate::virtual_swapchain::create_legacy)]
        unsafe extern "system" fn factory_create_swapchain(this: *mut c_void, device: *mut c_void, desc: *const c_void, swapchain: *mut *mut c_void) -> i32;

        #[base = IDXGIFactory2::CreateSwapChainForHwnd]
        #[fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0]
        #[infix(crate::virtual_swapchain::create_for_hwnd)]
        unsafe extern "system" fn factory_create_swapchain_for_hwnd(this: *mut c_void, device: *mut c_void, window: *mut c_void, desc: *const c_void, fullscreen_desc: *const c_void, restrict_to_output: *mut c_void, swapchain: *mut *mut c_void) -> i32;

    }
}
