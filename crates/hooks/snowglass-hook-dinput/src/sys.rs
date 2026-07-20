//! Injector-facing hook installation entry point.

use windows_sys::core::HRESULT;

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallDinputHook() -> HRESULT {
    unsafe { crate::install::install() }
}
