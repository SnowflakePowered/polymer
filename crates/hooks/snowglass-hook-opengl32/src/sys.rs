//! Injector-facing hook installation entry point.

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallOpenGlHook() -> i32 {
    unsafe { crate::install::install() }
}

#[unsafe(no_mangle)]
pub extern "system" fn SnowglassUninstallOpenGlHook() -> i32 {
    crate::install::uninstall()
}
