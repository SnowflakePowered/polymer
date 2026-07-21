//! Injector-facing D3D11 hook installation entry point.

/// Installs the process-wide D3D11 export detours.
///
/// # Safety
///
/// The process must have a usable system D3D11 runtime, and callers must not
/// concurrently unload the D3D11 module while installation is in progress.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallD3d11Hook() -> i32 {
    unsafe { crate::install::install() }
}

/// Uninstalls the process-wide D3D11 export detours.
#[unsafe(no_mangle)]
pub extern "system" fn SnowglassUninstallD3d11Hook() -> i32 {
    crate::install::uninstall()
}
