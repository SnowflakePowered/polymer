#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallUiHook() -> i32 {
    unsafe { crate::install::install() }
}
