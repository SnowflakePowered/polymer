//! Detour ownership and lifecycle.

use windows_sys::{Win32::Foundation::E_FAIL, core::HRESULT};

pub(crate) unsafe fn install() -> HRESULT {
    match crate::hooks::HookInstallation::install() {
        Ok(()) => {
            crate::hooks::HookInstallation::active_state().start_input_receiver();
            0
        }
        Err(_) => E_FAIL,
    }
}

pub(crate) fn uninstall() -> HRESULT {
    crate::hooks::HookInstallation::uninstall().map_or(E_FAIL, |()| 0)
}
