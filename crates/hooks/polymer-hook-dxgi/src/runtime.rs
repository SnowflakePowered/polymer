//! Install factory creation hooks; native swapchains are not captured.
use crate::vtable::IDXGIFactoryHook;
use std::ffi::c_void;
use windows::{Win32::Graphics::Dxgi::IDXGIFactory, core::GUID};

/// # Safety
///
/// `factory` must be null or point to a live `IDXGIFactory` interface.
pub unsafe fn install_factory(factory: *mut c_void) -> bool {
    unsafe { IDXGIFactoryHook::install_object::<IDXGIFactory>(factory) }
}

/// Installs per-object hooks for every factory interface supported by a DXGI
/// factory.
///
/// # Safety
///
/// `factory` must be null or point to a live `IDXGIFactory` interface for the
/// duration of this call.
pub unsafe fn install_factory_interfaces(factory: *mut c_void) -> bool {
    unsafe { IDXGIFactoryHook::install_all_interfaces(factory) }
}

/// # Safety
///
/// `factory` must be null or point to a live COM interface identified by
/// `interface_id`, and `interface_id` must be null or point to a valid GUID.
pub unsafe fn install_factory_interface(factory: *mut c_void, interface_id: *const c_void) -> bool {
    if interface_id.is_null() {
        return false;
    }
    let interface_id = unsafe { &*interface_id.cast::<GUID>() };
    unsafe { IDXGIFactoryHook::install_interface(factory, interface_id) }
}

pub(crate) unsafe fn created_factory(
    _interface_id: *const c_void,
    factory: *mut *mut c_void,
    result: i32,
) {
    if result < 0 || factory.is_null() {
        return;
    }
    let factory = unsafe { *factory };
    if factory.is_null() {
        return;
    }
    unsafe { install_factory_interfaces(factory) };
}

pub(crate) unsafe fn factory_query_interface(
    this: *mut c_void,
    interface_id: *const c_void,
    output: *mut *mut c_void,
    mut next: crate::vtable::FactoryQueryInterfaceContext<'_>,
) -> i32 {
    let result = polymer_hook::next!(next, this, interface_id, output);
    if result >= 0 && !interface_id.is_null() && !output.is_null() {
        let output = unsafe { *output };
        if !output.is_null() {
            unsafe { install_factory_interface(output, interface_id) };
        }
    }
    result
}
