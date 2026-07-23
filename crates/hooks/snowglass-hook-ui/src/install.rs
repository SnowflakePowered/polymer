pub(crate) unsafe fn install() -> i32 {
    match crate::modal::ModalHookInstallation::install() {
        Ok(()) => {
            let _ = crate::messagebox::MessageBoxHookInstallation::install();
            crate::gtk::install();
            crate::qt::install();
            0
        }
        Err(_) => windows_sys::Win32::Foundation::E_FAIL,
    }
}
