//! Detour ownership and lifecycle.

use std::sync::{Arc, Mutex};
use windows_sys::{Win32::Foundation::E_FAIL, core::HRESULT};

type Installation =
    snowglass_hook::TerminalInstallation<dyn crate::hooks::DirectInput8CreateTerminal>;

static INSTALLATION: Mutex<Option<Installation>> = Mutex::new(None);
static RUNTIME: Mutex<Option<Arc<crate::runtime::Runtime>>> = Mutex::new(None);

pub(crate) fn runtime() -> Arc<crate::runtime::Runtime> {
    RUNTIME
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .unwrap_or_default()
}

pub(crate) unsafe fn install() -> HRESULT {
    let mut installation = INSTALLATION
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if installation.is_some() {
        runtime().start_input_receiver();
        return 0;
    }
    let runtime = Arc::new(crate::runtime::Runtime::default());
    *RUNTIME.lock().unwrap_or_else(|error| error.into_inner()) = Some(Arc::clone(&runtime));
    match crate::hooks::install_direct_input_8_create_terminal() {
        Ok(installed) => {
            runtime.start_input_receiver();
            *installation = Some(installed);
            0
        }
        Err(_) => {
            *RUNTIME.lock().unwrap_or_else(|error| error.into_inner()) = None;
            E_FAIL
        }
    }
}

pub(crate) fn uninstall() -> HRESULT {
    let mut installation = INSTALLATION
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(installed) = installation.as_ref() else {
        return 0;
    };
    match installed.uninstall() {
        Ok(()) => {
            *installation = None;
            *RUNTIME.lock().unwrap_or_else(|error| error.into_inner()) = None;
            0
        }
        Err(_) => E_FAIL,
    }
}
