//! Typed clones of DirectInput vtables with interception applied.
//!
//! Each hooked object receives a heap clone of its vtable with a few slots
//! redirected. The clone is copied as opaque words (a vtable may legally carry
//! null or reserved slots, so whole-struct reads would assert validity the
//! source cannot guarantee), while individual slots are read and patched
//! through the `windows` crate's vtable definitions overlaid by a union.

// The hook chains spell signatures with crate-local, ABI-identical aliases of
// the `windows` types (HRESULT/GUID/HANDLE appear as i32/c_void). The
// transmutes below convert between the two spellings; the slot or return type
// they serve names the target, so per-call annotations would only restate the
// full `windows` signatures.
#![allow(clippy::missing_transmute_annotations)]

use std::ffi::c_void;
use windows::Win32::Devices::HumanInterfaceDevice::{
    IDirectInput8A, IDirectInput8A_Vtbl, IDirectInput8W, IDirectInput8W_Vtbl, IDirectInputDevice8A,
    IDirectInputDevice8A_Vtbl, IDirectInputDevice8W, IDirectInputDevice8W_Vtbl,
};
use windows::core::GUID;
use windows_sys::{Win32::Devices::HumanInterfaceDevice::DIDEVICEOBJECTDATA, core::HRESULT};

polymer_hook::vtable! {
    [IDirectInput8A],
    [IDirectInput8W],
    [IDirectInputDevice8A],
    [IDirectInputDevice8W],
}

polymer_hook::com_vtable! {
    /// Overlays the ABI-identical ANSI and wide `IDirectInput8` vtables.
    pub(crate) union IDirectInputVtable for IDirectInput8A;
}
const _: () = assert!(size_of::<IDirectInput8A_Vtbl>() == size_of::<IDirectInput8W_Vtbl>());

polymer_hook::com_vtable! {
    /// Overlays the ABI-identical ANSI and wide `IDirectInputDevice8` vtables.
    pub(crate) union IDirectInputDeviceVtable for IDirectInputDevice8A;
}
const _: () =
    assert!(size_of::<IDirectInputDevice8A_Vtbl>() == size_of::<IDirectInputDevice8W_Vtbl>());

pub(crate) struct DirectInputContext {
    pub(crate) runtime: std::sync::Arc<crate::runtime::Runtime>,
    pub(crate) wide: bool,
}

pub(crate) struct DeviceContext {
    pub(crate) runtime: std::sync::Arc<crate::runtime::Runtime>,
    pub(crate) device: polymer_protocol::input::DirectInputDevice,
}

polymer_hook::impl_com_proxy! {
    pub(crate) trait DirectInputHookState;
    impl DirectInputHookState for IDirectInputHook {
        #![vtable = IDirectInputVtable]
        #[state = DirectInputContext]
        #[release]
        unsafe extern "system" fn direct_input_release(this: *mut c_void) -> u32;

        #[base = IDirectInput8A::CreateDevice]
        #[fallback = 0x8000_4005u32 as i32]
        #[postfix(crate::runtime::create_device(class, output, result))]
        unsafe extern "system" fn create_device(this: *mut c_void, class: *const GUID, output: *mut *mut c_void, outer: *mut c_void) -> HRESULT;
    }
}

polymer_hook::impl_com_proxy! {
    pub(crate) trait DeviceHookState;
    impl DeviceHookState for IDirectInputDeviceHook {
        #![vtable = IDirectInputDeviceVtable]
        #[state = DeviceContext]
        #[release(destroyed = crate::runtime::device_destroyed)]
        unsafe extern "system" fn device_release(this: *mut c_void) -> u32;

        #[base = IDirectInputDevice8A::Acquire]
        #[fallback = 0x8000_4005u32 as i32]
        #[infix(crate::runtime::acquire)]
        unsafe extern "system" fn acquire(this: *mut c_void) -> HRESULT;

        #[base = IDirectInputDevice8A::GetDeviceState]
        #[fallback = 0x8000_4005u32 as i32]
        #[infix(crate::runtime::get_device_state)]
        unsafe extern "system" fn get_device_state(this: *mut c_void, size: u32, output: *mut c_void) -> HRESULT;

        #[base = IDirectInputDevice8A::GetDeviceData]
        #[fallback = 0x8000_4005u32 as i32]
        #[infix(crate::runtime::get_device_data)]
        unsafe extern "system" fn get_device_data(this: *mut c_void, object_size: u32, output: *mut DIDEVICEOBJECTDATA, count: *mut u32, flags: u32) -> HRESULT;

        #[base = IDirectInputDevice8A::SetDataFormat]
        #[fallback = 0x8000_4005u32 as i32]
        unsafe extern "system" fn set_data_format(this: *mut c_void, format: *const c_void) -> HRESULT;

        #[base = IDirectInputDevice8A::SetEventNotification]
        #[fallback = 0x8000_4005u32 as i32]
        #[prefix(|context| context.state.runtime.set_event_notification(
            this as usize,
            context.state.device,
            event as usize,
        ))]
        unsafe extern "system" fn set_event_notification(this: *mut c_void, event: *mut c_void) -> HRESULT;

        #[base = IDirectInputDevice8A::SetCooperativeLevel]
        #[fallback = 0x8000_4005u32 as i32]
        #[infix(crate::runtime::set_cooperative_level)]
        unsafe extern "system" fn set_cooperative_level(this: *mut c_void, window: *mut c_void, flags: u32) -> HRESULT;
    }
}
