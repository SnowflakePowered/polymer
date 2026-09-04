//! Early-process detours for the DXGI factory creation exports.

use std::ffi::c_void;

#[link(name = "dxgi")]
unsafe extern "system" {
    #[link_name = "CreateDXGIFactory"]
    pub(crate) fn system_create_factory(
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;

    #[link_name = "CreateDXGIFactory1"]
    pub(crate) fn system_create_factory1(
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;

    #[link_name = "CreateDXGIFactory2"]
    pub(crate) fn system_create_factory2(
        flags: u32,
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;
}

pub(crate) unsafe fn install() -> bool {
    crate::hooks::HookInstallation::install().is_ok()
}

pub(crate) fn uninstall() -> bool {
    crate::hooks::HookInstallation::uninstall().is_ok()
}
