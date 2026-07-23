use windows_sys::Win32::Foundation::E_FAIL;

pub(crate) unsafe fn install() -> i32 {
    match crate::messagebox::MessageBoxHookInstallation::install() {
        Ok(()) => {
            crate::qt::install();
            0
        }
        Err(_) => E_FAIL,
    }
}
