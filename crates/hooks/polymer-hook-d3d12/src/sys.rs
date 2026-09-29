#[unsafe(no_mangle)]
pub unsafe extern "system" fn PolymerInstallD3d12Hook() -> i32 {
    polymer_hook::telemetry::init_file_tracing(polymer_hook::config::log_file(
        polymer_hook::config::D3D12_LOG_FILE,
    ));
    if let Err(error) = polymer_hook::config::require_swapchain() {
        tracing::error!(%error, "D3D12 presentation configuration rejected");
        return windows::Win32::Foundation::E_INVALIDARG.0;
    }
    polymer_hook_dxgi::register_virtual_claim(
        polymer_producer_d3d12::swapchain::creation::D3d12Creation::claim,
    );
    0
}

#[unsafe(no_mangle)]
pub extern "system" fn PolymerUninstallD3d12Hook() -> i32 {
    0
}
