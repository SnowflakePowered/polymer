use crate::hook_chain::{
    __resolve_around_terminal, __resolve_terminal, around_chain, chain, post_chain,
};
use std::ffi::c_void;

chain! {
    pub(crate) dispatch_query_interface, register_query_interface;
    around QueryInterface;
    chain QUERY_INTERFACE_CHAIN, context QueryInterfaceContext;
    hook QueryInterfaceHook(this: *mut c_void, interface_id: *const c_void, output: *mut *mut c_void) -> i32;
    resolve with query, query_interface -> 0x8000_4002u32 as i32;
}

chain! {
    pub(crate) dispatch_present, register_present;
    around Present;
    chain PRESENT_CHAIN, context PresentContext;
    hook PresentHook(this: *mut c_void, sync_interval: u32, flags: u32) -> i32;
    resolve with swapchain, present -> windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0;
}

chain! {
    pub(crate) dispatch_present1, register_present1;
    around Present1;
    chain PRESENT1_CHAIN, context Present1Context;
    hook Present1Hook(this: *mut c_void, sync_interval: u32, flags: u32, parameters: *const c_void) -> i32;
    resolve with swapchain1, present1 -> windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0;
}

chain! {
    pub(crate) dispatch_factory_create_swapchain, register_factory_create_swapchain;
    after CreateSwapChain;
    chain FACTORY_CREATE_SWAPCHAIN_CHAIN, context FactoryCreateSwapchainContext;
    hook FactoryCreateSwapchainHook(swapchain: *mut *mut c_void) -> result: i32;
    abi (this: *mut c_void, device: *mut c_void, desc: *const c_void, swapchain: *mut *mut c_void);
    resolve with factory, create_swapchain -> windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0;
}

chain! {
    pub(crate) dispatch_factory_create_swapchain_for_hwnd, register_factory_create_swapchain_for_hwnd;
    after CreateSwapChainForHwnd;
    chain FACTORY_CREATE_SWAPCHAIN_FOR_HWND_CHAIN, context FactoryCreateSwapchainForHwndContext;
    hook FactoryCreateSwapchainForHwndHook(swapchain: *mut *mut c_void) -> result: i32;
    abi (this: *mut c_void, device: *mut c_void, window: *mut c_void, desc: *const c_void, fullscreen_desc: *const c_void, restrict_to_output: *mut c_void, swapchain: *mut *mut c_void);
    resolve with factory, create_swapchain_for_hwnd -> windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0;
}

chain! {
    pub(crate) dispatch_factory_create_swapchain_for_core_window, register_factory_create_swapchain_for_core_window;
    after CreateSwapChainForCoreWindow;
    chain FACTORY_CREATE_SWAPCHAIN_FOR_CORE_WINDOW_CHAIN, context FactoryCreateSwapchainForCoreWindowContext;
    hook FactoryCreateSwapchainForCoreWindowHook(swapchain: *mut *mut c_void) -> result: i32;
    abi (this: *mut c_void, device: *mut c_void, window: *mut c_void, desc: *const c_void, restrict_to_output: *mut c_void, swapchain: *mut *mut c_void);
    resolve with factory, create_swapchain_for_core_window -> windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0;
}

chain! {
    pub(crate) dispatch_factory_create_swapchain_for_composition, register_factory_create_swapchain_for_composition;
    after CreateSwapChainForComposition;
    chain FACTORY_CREATE_SWAPCHAIN_FOR_COMPOSITION_CHAIN, context FactoryCreateSwapchainForCompositionContext;
    hook FactoryCreateSwapchainForCompositionHook(swapchain: *mut *mut c_void) -> result: i32;
    abi (this: *mut c_void, device: *mut c_void, desc: *const c_void, restrict_to_output: *mut c_void, swapchain: *mut *mut c_void);
    resolve with factory, create_swapchain_for_composition -> windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0;
}

chain! {
    pub(crate) dispatch_resize_buffers, register_resize_buffers;
    around ResizeBuffers;
    chain RESIZE_BUFFERS_CHAIN, context ResizeBuffersContext;
    hook ResizeBuffersHook(this: *mut c_void, buffer_count: u32, width: u32, height: u32, format: u32, flags: u32) -> i32;
    resolve with swapchain, resize_buffers -> windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0;
}

chain! {
    pub(crate) dispatch_release, register_release;
    around Release;
    chain RELEASE_CHAIN, context ReleaseContext;
    hook ReleaseHook(this: *mut c_void) -> u32;
    resolve with crate::runtime::release;
}
