//! Injector-facing hook installation entry point.

#[unsafe(no_mangle)]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "system" fn PolymerInstallOpenGlHook() -> i32 {
    polymer_hook::telemetry::init_file_tracing(polymer_hook::config::log_file(
        polymer_hook::config::OPENGL_LOG_FILE,
    ));
    unsafe { crate::install::install() }
}

#[unsafe(no_mangle)]
pub extern "system" fn PolymerUninstallOpenGlHook() -> i32 {
    crate::install::uninstall()
}
