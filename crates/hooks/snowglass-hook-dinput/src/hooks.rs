//! Concrete DirectInput hook-chain declarations.

use std::ffi::c_void;
use windows_sys::{
    Win32::Foundation::{E_FAIL, HINSTANCE},
    core::{GUID, HRESULT},
};

snowglass_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer(state = crate::runtime::Runtime)]
    pub(crate) struct HookInstallation;

    #[trampoline]
    #[terminal(installed(
        target = windows_sys::Win32::Devices::HumanInterfaceDevice::DirectInput8Create,
    ), fallback = E_FAIL)]
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
