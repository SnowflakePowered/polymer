//! Injector-facing Win32 hook installation entry point.

use windows_sys::core::HRESULT;

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallWin32Hook() -> HRESULT {
    unsafe { crate::install::install() }
}

#[unsafe(no_mangle)]
pub extern "system" fn SnowglassUninstallWin32Hook() -> HRESULT {
    crate::install::uninstall()
}
