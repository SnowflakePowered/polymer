use windows_sys::core::HRESULT;

#[unsafe(no_mangle)]
pub extern "system" fn PolymerInstallXinputHook() -> HRESULT {
    crate::install()
}

#[unsafe(no_mangle)]
pub extern "system" fn PolymerUninstallXinputHook() -> HRESULT {
    crate::uninstall()
}
