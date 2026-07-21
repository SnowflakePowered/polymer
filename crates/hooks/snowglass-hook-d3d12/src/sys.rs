#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallD3d12Hook() -> i32 {
    unsafe { crate::install::install() }
}

#[unsafe(no_mangle)]
pub extern "system" fn SnowglassUninstallD3d12Hook() -> i32 {
    crate::install::uninstall()
}
