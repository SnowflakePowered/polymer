//! Injector-facing hook installation entry point.

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallGdiHook() -> i32 {
    unsafe { crate::install::install() }
}
