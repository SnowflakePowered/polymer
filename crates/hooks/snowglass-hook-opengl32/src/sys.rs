//! Injector-facing hook installation entry point.

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallOpenGlHook() -> i32 {
    snowglass_hook::telemetry::init_file_tracing(snowglass_hook::config::opengl_log_file());
    unsafe { crate::install::install() }
}

#[unsafe(no_mangle)]
pub extern "system" fn SnowglassUninstallOpenGlHook() -> i32 {
    crate::install::uninstall()
}
