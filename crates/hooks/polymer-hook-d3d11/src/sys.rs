//! Injector-facing D3D11 hook installation entry point.

/// Installs the process-wide D3D11 export detours.
///
/// # Safety
///
/// The process must have a usable system D3D11 runtime, and callers must not
/// concurrently unload the D3D11 module while installation is in progress.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn PolymerInstallD3d11Hook() -> i32 {
    polymer_hook::telemetry::init_file_tracing(polymer_hook::config::d3d11_log_file());
    unsafe { crate::install::install() }
}

/// Uninstalls the process-wide D3D11 export detours.
#[unsafe(no_mangle)]
pub extern "system" fn PolymerUninstallD3d11Hook() -> i32 {
    crate::install::uninstall()
}
