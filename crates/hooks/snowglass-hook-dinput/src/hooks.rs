//! Concrete DirectInput hook-chain declarations.

use crate::hook_chain::{__resolve_observer_terminal, chain_hooks};
use std::ffi::c_void;
use windows_sys::{
    Win32::Foundation::{E_FAIL, HINSTANCE},
    core::{GUID, HRESULT},
};

chain_hooks! {
    #![order(reverse)]

    #[trampoline]
    #[terminal(detour = crate::install::direct_input_detour, fallback = E_FAIL)]
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
    ) -> HRESULT {
        context struct {
            runtime: std::sync::Arc<crate::runtime::Runtime> = crate::install::runtime(),
        }
    }
}
