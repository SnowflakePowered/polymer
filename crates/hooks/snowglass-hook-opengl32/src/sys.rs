//! Injector-facing hook installation entry point.

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallOpenGlHook() -> i32 {
    unsafe { crate::install::install() }
}
