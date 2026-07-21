use std::ffi::c_void;

snowglass_hook::__chain_hooks! {
    #![order(reverse)]

    #[trampoline]
    #[terminal(installed, fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0)]
    #[postfix(crate::runtime::created_factory(interface_id, factory, result))]
    pub(crate) unsafe extern "system" fn create_factory(
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;

    #[trampoline]
    #[terminal(installed, fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0)]
    #[postfix(crate::runtime::created_factory(interface_id, factory, result))]
    pub(crate) unsafe extern "system" fn create_factory1(
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;

    #[trampoline]
    #[terminal(installed, fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0)]
    #[postfix(crate::runtime::created_factory(interface_id, factory, result))]
    pub(crate) unsafe extern "system" fn create_factory2(
        flags: u32,
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;
}
