use crate::hook_chain::{__resolve_terminal, chain_hooks};
use std::ffi::c_void;

chain_hooks! {
    #![order(reverse)]

    #[trampoline]
    #[terminal(
        detour = crate::install::DETOURS.create_factory,
        fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0,
    )]
    #[postfix(crate::runtime::created_factory(interface_id, factory, result))]
    pub(crate) unsafe extern "system" fn create_factory(
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;

    #[trampoline]
    #[terminal(
        detour = crate::install::DETOURS.create_factory1,
        fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0,
    )]
    #[postfix(crate::runtime::created_factory(interface_id, factory, result))]
    pub(crate) unsafe extern "system" fn create_factory1(
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;

    #[trampoline]
    #[terminal(
        detour = crate::install::DETOURS.create_factory2,
        fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0,
    )]
    #[postfix(crate::runtime::created_factory(interface_id, factory, result))]
    pub(crate) unsafe extern "system" fn create_factory2(
        flags: u32,
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;
}
