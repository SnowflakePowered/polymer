use std::ffi::c_void;
use windows_sys::core::GUID;

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallD3d12Hook() -> i32 {
    unsafe { crate::install::install() }
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn create_device(
        adapter: *mut c_void,
        minimum_feature_level: i32,
        interface_id: *const GUID,
        device: *mut *mut c_void,
    ) -> i32 => crate::hooks::dispatch_create_device;
}
