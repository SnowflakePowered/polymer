//! Injector-facing hook installation entry point.

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallGdiHook() -> i32 {
    snowglass_hook::telemetry::init_file_tracing(snowglass_hook::config::gdi_log_file());
    unsafe { crate::install::install() }
}
