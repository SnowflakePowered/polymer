//! Concrete DirectInput hook-chain declarations.

use std::ffi::c_void;
use windows_sys::{
    Win32::Foundation::{E_FAIL, HINSTANCE},
    core::{GUID, HRESULT},
};

snowglass_hook::__chain_hooks! {
    #![order(reverse)]
    #[context]
    pub(crate) struct DirectInputHookContext {
        pub(crate) runtime: std::sync::Arc<crate::runtime::Runtime> = crate::install::runtime(),
    }

    #[trampoline]
    #[terminal(installed, fallback = E_FAIL)]
    #[postfix(crate::runtime::direct_input_8_create(
        instance,
        version,
        interface_id,
        output,
        outer,
        result
    ))]
    pub unsafe extern "system" fn direct_input_8_create(
        instance: HINSTANCE,
        version: u32,
        interface_id: *const GUID,
        output: *mut *mut c_void,
        outer: *mut c_void,
    ) -> HRESULT;
}
