//! Injector-facing hook installation entry point.

use windows_sys::core::HRESULT;

#[unsafe(no_mangle)]
pub unsafe extern "system" fn PolymerInstallDinputHook() -> HRESULT {
    unsafe { crate::install::install() }
}

#[unsafe(no_mangle)]
pub extern "system" fn PolymerUninstallDinputHook() -> HRESULT {
    crate::install::uninstall()
}
