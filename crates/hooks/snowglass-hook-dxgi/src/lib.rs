//! DXGI swapchain interception shared by D3D11 and D3D12.
#![allow(unsafe_code)]

mod detour;
mod hook_chain;
mod hooks;
mod runtime;
mod sys;
mod vtable;

pub use runtime::{
    SwapchainCallbacks, install_factory, install_factory_detours, install_factory_interface,
    install_factory_interfaces, install_swapchain, install_swapchain1, register_callbacks,
    register_swapchain,
};

pub(crate) fn dxgi_log(message: impl std::fmt::Display) {
    use std::io::Write;
    eprintln!("{message}");
    let Ok(path) = std::env::var("SNOWGLASS_D3D11_LOG_FILE") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{message}");
    }
}

pub(crate) fn register_hooks() {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(runtime::register_runtime_hooks);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vtable::{
        PRESENT_INDEX, PRESENT1_INDEX, QUERY_INTERFACE_INDEX, RELEASE_INDEX, RESIZE_BUFFERS_INDEX,
        SWAPCHAIN_VTABLE_LEN, SWAPCHAIN1_VTABLE_LEN,
    };
    use std::ffi::c_void;

    unsafe extern "system" fn release(_this: *mut c_void) -> u32 {
        0
    }

    unsafe extern "system" fn query_interface(
        _this: *mut c_void,
        _interface_id: *const c_void,
        _output: *mut *mut c_void,
    ) -> i32 {
        0x8000_4002u32 as i32
    }

    unsafe extern "system" fn present(_this: *mut c_void, sync: u32, flags: u32) -> i32 {
        (sync + flags) as i32
    }

    unsafe extern "system" fn resize(
        _this: *mut c_void,
        buffers: u32,
        width: u32,
        height: u32,
        format: u32,
        flags: u32,
    ) -> i32 {
        (buffers + width + height + format + flags) as i32
    }

    #[test]
    fn patches_and_dispatches_swapchain_vtable() {
        let mut original = [0usize; SWAPCHAIN_VTABLE_LEN];
        original[QUERY_INTERFACE_INDEX] = query_interface as *const () as usize;
        original[RELEASE_INDEX] = release as *const () as usize;
        original[PRESENT_INDEX] = present as *const () as usize;
        original[RESIZE_BUFFERS_INDEX] = resize as *const () as usize;
        let mut object_vtable = original.as_ptr();
        let object = (&raw mut object_vtable).cast::<c_void>();

        assert!(unsafe { install_swapchain(object) });
        assert_ne!(object_vtable, original.as_ptr());

        let patched_present = unsafe {
            std::mem::transmute::<usize, crate::vtable::PresentFn>(
                *object_vtable.add(PRESENT_INDEX),
            )
        };
        let patched_resize = unsafe {
            std::mem::transmute::<usize, crate::vtable::ResizeBuffersFn>(
                *object_vtable.add(RESIZE_BUFFERS_INDEX),
            )
        };
        let patched_release = unsafe {
            std::mem::transmute::<usize, crate::vtable::ReleaseFn>(
                *object_vtable.add(RELEASE_INDEX),
            )
        };
        assert_eq!(unsafe { patched_present(object, 2, 3) }, 5);
        assert_eq!(unsafe { patched_resize(object, 1, 2, 3, 4, 5) }, 15);
        assert_eq!(unsafe { patched_release(object) }, 0);
    }

    unsafe extern "system" fn present1(
        _this: *mut c_void,
        sync: u32,
        flags: u32,
        _parameters: *const c_void,
    ) -> i32 {
        (sync + flags + 7) as i32
    }

    #[test]
    fn upgrades_and_dispatches_swapchain1_vtable() {
        let mut original = [0usize; SWAPCHAIN1_VTABLE_LEN];
        original[QUERY_INTERFACE_INDEX] = query_interface as *const () as usize;
        original[RELEASE_INDEX] = release as *const () as usize;
        original[PRESENT_INDEX] = present as *const () as usize;
        original[RESIZE_BUFFERS_INDEX] = resize as *const () as usize;
        original[PRESENT1_INDEX] = present1 as *const () as usize;
        let mut object_vtable = original.as_ptr();
        let object = (&raw mut object_vtable).cast::<c_void>();

        assert!(unsafe { install_swapchain(object) });
        assert!(unsafe { install_swapchain1(object) });
        let patched_present1 = unsafe {
            std::mem::transmute::<usize, crate::vtable::Present1Fn>(
                *object_vtable.add(PRESENT1_INDEX),
            )
        };
        assert_eq!(
            unsafe { patched_present1(object, 2, 3, std::ptr::null()) },
            12
        );
    }
}
