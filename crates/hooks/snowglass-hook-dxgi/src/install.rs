//! Early-process detours for the DXGI factory creation exports.

use retour::GenericDetour;
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
    pub(crate) create_factory: GenericDetour<CreateDxgiFactoryFn>,
    pub(crate) create_factory1: GenericDetour<CreateDxgiFactoryFn>,
    pub(crate) create_factory2: GenericDetour<CreateDxgiFactory2Fn>,
}

pub(crate) static DETOURS: OnceLock<DxgiDetours> = OnceLock::new();

pub(crate) unsafe fn install() -> bool {
    if let Some(detours) = DETOURS.get() {
        return detours.create_factory.is_enabled()
            && detours.create_factory1.is_enabled()
            && detours.create_factory2.is_enabled();
    }
    let Ok(create_factory) = (unsafe {
        GenericDetour::new(
            system_create_factory as CreateDxgiFactoryFn,
            crate::sys::create_factory,
        )
    }) else {
        return false;
    };
    let Ok(create_factory1) = (unsafe {
        GenericDetour::new(
            system_create_factory1 as CreateDxgiFactoryFn,
            crate::sys::create_factory1,
        )
    }) else {
        return false;
    };
    let Ok(create_factory2) = (unsafe {
        GenericDetour::new(
            system_create_factory2 as CreateDxgiFactory2Fn,
            crate::sys::create_factory2,
        )
    }) else {
        return false;
    };
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
    let enabled = unsafe {
        detours
            .create_factory
            .enable()
            .and_then(|()| detours.create_factory1.enable())
            .and_then(|()| detours.create_factory2.enable())
    };
    if enabled.is_err() {
        let _ = unsafe { detours.create_factory2.disable() };
        let _ = unsafe { detours.create_factory1.disable() };
        let _ = unsafe { detours.create_factory.disable() };
        return false;
    }
    true
}
