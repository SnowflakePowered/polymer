use windows_sys::core::HRESULT;

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallBluetoothApiWin32Hook() -> HRESULT {
    unsafe { crate::install::install() }
}

#[unsafe(no_mangle)]
pub extern "system" fn SnowglassUninstallBluetoothApiWin32Hook() -> HRESULT {
    crate::install::uninstall()
}
