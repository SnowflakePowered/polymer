#[unsafe(no_mangle)]
pub unsafe extern "system" fn PolymerInstallUiHook() -> i32 {
    unsafe { crate::install::install() }
}
