//! Detour ownership and lifecycle.

use crate::DirectInput8CreateFn;
use retour::GenericDetour;
use std::{
    ffi::c_void,
    sync::{Arc, OnceLock},
};
use windows_sys::{
    Win32::{
        Devices::HumanInterfaceDevice::DirectInput8Create,
        Foundation::{E_FAIL, HINSTANCE},
    },
    core::{GUID, HRESULT},
};

struct DirectInputDetour {
    detour: GenericDetour<DirectInput8CreateFn>,
    runtime: Arc<crate::runtime::Runtime>,
}

static DIRECT_INPUT_8_CREATE: OnceLock<DirectInputDetour> = OnceLock::new();

pub(crate) fn runtime() -> Arc<crate::runtime::Runtime> {
    DIRECT_INPUT_8_CREATE
        .get()
        .map(|installation| installation.runtime.clone())
        .unwrap_or_default()
}

pub(crate) unsafe fn install() -> HRESULT {
    if let Some(installation) = DIRECT_INPUT_8_CREATE.get() {
        return if installation.detour.is_enabled() {
            0
        } else {
            E_FAIL
        };
    }
    let target = DirectInput8Create as DirectInput8CreateFn;
    let detour = match unsafe { GenericDetour::new(target, crate::sys::direct_input_8_create) } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
    };
    if DIRECT_INPUT_8_CREATE
        .set(DirectInputDetour {
            detour,
            runtime: Arc::default(),
        })
        .is_err()
    {
        return E_FAIL;
    }
    match unsafe {
        DIRECT_INPUT_8_CREATE
            .get()
            .expect("detour was set")
            .detour
            .enable()
    } {
        Ok(()) => 0,
        Err(_) => E_FAIL,
    }
}

pub(crate) unsafe extern "system" fn call_original(
    instance: HINSTANCE,
    version: u32,
    interface_id: *const GUID,
    output: *mut *mut c_void,
    outer: *mut c_void,
) -> HRESULT {
    snowglass_platform_windows::call_original(
        DIRECT_INPUT_8_CREATE
            .get()
            .map(|installation| &installation.detour),
        E_FAIL,
        |detour| unsafe { detour.call(instance, version, interface_id, output, outer) },
    )
}
