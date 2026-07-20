//! Detour ownership and lifecycle.

use crate::DirectInput8CreateFn;
use snowglass_platform_windows::InlineDetour;
use std::sync::{Arc, OnceLock};
use windows_sys::{
    Win32::{Devices::HumanInterfaceDevice::DirectInput8Create, Foundation::E_FAIL},
    core::HRESULT,
};

struct DirectInputDetour {
    detour: InlineDetour<DirectInput8CreateFn>,
    runtime: Arc<crate::runtime::Runtime>,
}

static DIRECT_INPUT_8_CREATE: OnceLock<DirectInputDetour> = OnceLock::new();

pub(crate) fn runtime() -> Arc<crate::runtime::Runtime> {
    DIRECT_INPUT_8_CREATE
        .get()
        .map(|installation| installation.runtime.clone())
        .unwrap_or_default()
}

pub(crate) fn direct_input_detour() -> Option<&'static InlineDetour<DirectInput8CreateFn>> {
    DIRECT_INPUT_8_CREATE
        .get()
        .map(|installation| &installation.detour)
}

pub(crate) unsafe fn install() -> HRESULT {
    if let Some(installation) = DIRECT_INPUT_8_CREATE.get() {
        return if installation.detour.is_enabled() {
            installation.runtime.start_input_receiver();
            0
        } else {
            E_FAIL
        };
    }
    let target = DirectInput8Create as DirectInput8CreateFn;
    let detour = InlineDetour::new(target, crate::hooks::direct_input_8_create);
    if DIRECT_INPUT_8_CREATE
        .set(DirectInputDetour {
            detour,
            runtime: Arc::default(),
        })
        .is_err()
    {
        return E_FAIL;
    }
    match DIRECT_INPUT_8_CREATE
        .get()
        .expect("detour was set")
        .detour
        .enable()
    {
        Ok(()) => {
            DIRECT_INPUT_8_CREATE
                .get()
                .expect("detour was set")
                .runtime
                .start_input_receiver();
            0
        }
        Err(_) => E_FAIL,
    }
}
