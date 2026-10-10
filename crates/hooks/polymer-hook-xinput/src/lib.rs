//! Synthetic XInput pads backed by Polymer's projected controllers.
#![allow(unsafe_code)]

mod hooks;
mod runtime;
mod sys;

pub use sys::{PolymerInstallXinputHook, PolymerUninstallXinputHook};
use windows_sys::{Win32::Foundation::E_FAIL, core::HRESULT};

pub(crate) fn install() -> HRESULT {
    match crate::hooks::HookInstallation::install() {
        Ok(_) => {
            tracing::debug!("installed XInput virtual gamepad hooks");
            0
        }
        Err(_) => {
            tracing::debug!("failed to enable XInput detours");
            E_FAIL
        }
    }
}

pub(crate) fn uninstall() -> HRESULT {
    crate::hooks::HookInstallation::uninstall().map_or(E_FAIL, |()| 0)
}
