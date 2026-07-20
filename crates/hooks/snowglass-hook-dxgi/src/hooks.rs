use crate::hook_chain::{__resolve_terminal, chain, post_chain};
use std::ffi::c_void;

chain! {
    pub(crate) dispatch_create_factory, register_create_factory;
    after CreateDXGIFactory;
    chain CREATE_FACTORY_CHAIN, context CreateFactoryContext;
    hook CreateFactoryHook(interface_id: *const c_void, factory: *mut *mut c_void) -> result: i32;
    resolve with detour crate::install::DETOURS, create_factory -> windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0;
}

chain! {
    pub(crate) dispatch_create_factory1, register_create_factory1;
    after CreateDXGIFactory1;
    chain CREATE_FACTORY1_CHAIN, context CreateFactory1Context;
    hook CreateFactory1Hook(interface_id: *const c_void, factory: *mut *mut c_void) -> result: i32;
    resolve with detour crate::install::DETOURS, create_factory1 -> windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0;
}

chain! {
    pub(crate) dispatch_create_factory2, register_create_factory2;
    after CreateDXGIFactory2;
    chain CREATE_FACTORY2_CHAIN, context CreateFactory2Context;
    hook CreateFactory2Hook(interface_id: *const c_void, factory: *mut *mut c_void) -> result: i32;
    abi (flags: u32, interface_id: *const c_void, factory: *mut *mut c_void);
    resolve with detour crate::install::DETOURS, create_factory2 -> windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0;
}
