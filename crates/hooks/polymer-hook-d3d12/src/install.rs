use std::ffi::c_void;
use windows_sys::{Win32::Foundation::E_FAIL, core::GUID};

#[link(name = "d3d12")]
unsafe extern "system" {
    #[link_name = "D3D12CreateDevice"]
    pub(crate) fn system_create_device(
        adapter: *mut c_void,
        minimum_feature_level: i32,
        interface_id: *const GUID,
        device: *mut *mut c_void,
    ) -> i32;
}

pub(crate) unsafe fn install() -> i32 {
    polymer_hook_dxgi::register_claim(crate::claim_swapchain);
    polymer_hook_dxgi::register_virtual_claim(
        polymer_producer_d3d12::virtual_swapchain::creation::D3d12Creation::claim,
    );
    crate::hooks::HookInstallation::install().map_or(E_FAIL, |()| 0)
}

pub(crate) fn uninstall() -> i32 {
    crate::hooks::HookInstallation::uninstall().map_or(E_FAIL, |()| 0)
}
