#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallD3d12Hook() -> i32 {
    snowglass_hook::telemetry::init_file_tracing(snowglass_hook::config::d3d12_log_file());
    unsafe { crate::install::install() }
}

#[unsafe(no_mangle)]
pub extern "system" fn SnowglassUninstallD3d12Hook() -> i32 {
    crate::install::uninstall()
}
