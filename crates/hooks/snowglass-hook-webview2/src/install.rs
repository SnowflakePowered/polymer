//! Detour ownership and lifecycle.

use windows_sys::{Win32::Foundation::E_FAIL, core::HRESULT};

pub(crate) unsafe fn install() -> HRESULT {
    crate::hooks::HookInstallation::install().map_or(E_FAIL, |()| 0)
}

pub(crate) fn uninstall() -> HRESULT {
    crate::hooks::HookInstallation::uninstall().map_or(E_FAIL, |()| 0)
}
