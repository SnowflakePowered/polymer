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

pub(crate) unsafe extern "system" fn draw_indexed(
    this: *mut std::ffi::c_void,
    index_count: u32,
    start_index: u32,
    base_vertex: i32,
) {
    if let Some(original) = crate::tracking::draw_indexed_original(this) {
        unsafe { original(this, index_count, start_index, base_vertex) };
    }
    crate::tracking::record_draw(this);
}

pub(crate) unsafe extern "system" fn draw(
    this: *mut std::ffi::c_void,
    vertex_count: u32,
    start_vertex: u32,
) {
    if let Some(original) = crate::tracking::draw_original(this) {
        unsafe { original(this, vertex_count, start_vertex) };
    }
    crate::tracking::record_draw(this);
}
