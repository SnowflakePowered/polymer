//! Early-process detours for the DXGI factory creation exports.

use snowglass_platform_windows::InlineDetour;
use std::{ffi::c_void, sync::OnceLock};

pub(crate) type CreateDxgiFactoryFn =
    unsafe extern "system" fn(*const c_void, *mut *mut c_void) -> i32;

pub(crate) type CreateDxgiFactory2Fn =
    unsafe extern "system" fn(u32, *const c_void, *mut *mut c_void) -> i32;

#[link(name = "dxgi")]
unsafe extern "system" {
    #[link_name = "CreateDXGIFactory"]
    pub(crate) fn system_create_factory(
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;

    #[link_name = "CreateDXGIFactory1"]
    pub(crate) fn system_create_factory1(
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;

    #[link_name = "CreateDXGIFactory2"]
    pub(crate) fn system_create_factory2(
        flags: u32,
        interface_id: *const c_void,
        factory: *mut *mut c_void,
    ) -> i32;
}

pub(crate) struct DxgiDetours {
    pub(crate) create_factory: InlineDetour<CreateDxgiFactoryFn>,
    pub(crate) create_factory1: InlineDetour<CreateDxgiFactoryFn>,
    pub(crate) create_factory2: InlineDetour<CreateDxgiFactory2Fn>,
}

pub(crate) static DETOURS: OnceLock<DxgiDetours> = OnceLock::new();

pub(crate) unsafe fn install() -> bool {
    if let Some(detours) = DETOURS.get() {
        return detours.create_factory.is_enabled()
            && detours.create_factory1.is_enabled()
            && detours.create_factory2.is_enabled();
    }
    let create_factory = InlineDetour::new(
        system_create_factory as CreateDxgiFactoryFn,
        crate::hooks::create_factory,
    );
    let create_factory1 = InlineDetour::new(
        system_create_factory1 as CreateDxgiFactoryFn,
        crate::hooks::create_factory1,
    );
    let create_factory2 = InlineDetour::new(
        system_create_factory2 as CreateDxgiFactory2Fn,
        crate::hooks::create_factory2,
    );
    if DETOURS
        .set(DxgiDetours {
            create_factory,
            create_factory1,
            create_factory2,
        })
        .is_err()
    {
        return DETOURS.get().is_some_and(|detours| {
            detours.create_factory.is_enabled()
                && detours.create_factory1.is_enabled()
                && detours.create_factory2.is_enabled()
        });
    }
    let detours = DETOURS.get().expect("detours were just initialized");
    let enabled = detours
        .create_factory
        .enable()
        .and_then(|()| detours.create_factory1.enable())
        .and_then(|()| detours.create_factory2.enable());
    if enabled.is_err() {
        let _ = detours.create_factory2.disable();
        let _ = detours.create_factory1.disable();
        let _ = detours.create_factory.disable();
        return false;
    }
    true
}
