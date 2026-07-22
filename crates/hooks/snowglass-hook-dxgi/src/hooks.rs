use std::ffi::c_void;

snowglass_hook::__chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer]
    pub(crate) struct HookInstallation;

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_create_factory,
    ), fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0)]
    #[postfix(crate::runtime::created_factory(interface_id, factory, result))]
    pub(crate) unsafe extern "system" fn create_factory(
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_create_factory1,
    ), fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0)]
    #[postfix(crate::runtime::created_factory(interface_id, factory, result))]
    pub(crate) unsafe extern "system" fn create_factory1(
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_create_factory2,
    ), fallback = windows::Win32::Graphics::Dxgi::DXGI_ERROR_INVALID_CALL.0)]
    #[postfix(crate::runtime::created_factory(interface_id, factory, result))]
    pub(crate) unsafe extern "system" fn create_factory2(
        flags: u32,
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;
}
