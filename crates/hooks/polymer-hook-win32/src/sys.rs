//! Injector-facing Win32 hook installation entry point.

use windows_sys::core::HRESULT;

#[unsafe(no_mangle)]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "system" fn PolymerInstallWin32Hook() -> HRESULT {
    unsafe { crate::install::install() }
}

#[unsafe(no_mangle)]
pub extern "system" fn PolymerUninstallWin32Hook() -> HRESULT {
    crate::install::uninstall()
}
