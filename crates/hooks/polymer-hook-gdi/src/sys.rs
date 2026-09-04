//! Injector-facing hook installation entry point.

#[unsafe(no_mangle)]
pub unsafe extern "system" fn PolymerInstallGdiHook() -> i32 {
    polymer_hook::telemetry::init_file_tracing(polymer_hook::config::gdi_log_file());
    unsafe { crate::install::install() }
}
