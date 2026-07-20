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
    IDirectInput8A_Vtbl, IDirectInput8W_Vtbl, IDirectInputDevice8A_Vtbl, IDirectInputDevice8W_Vtbl,
};
use windows_sys::{
    Win32::Devices::HumanInterfaceDevice::DIDEVICEOBJECTDATA,
    core::{GUID, HRESULT},
};

snowglass_hook::vtable! {
    [IDirectInput8A_Vtbl],
    [IDirectInput8W_Vtbl],
    [IDirectInputDevice8A_Vtbl],
    [IDirectInputDevice8W_Vtbl],
}

#[cfg(test)]
pub(crate) const DEVICE_VTABLE_LEN: usize = <IDirectInputDevice8A_Vtbl as Vtable>::SLOTS;

#[cfg(test)]
pub(crate) type GetDeviceStateFn =
    unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> HRESULT;
#[cfg(test)]
pub(crate) type GetDeviceDataFn =
    unsafe extern "system" fn(*mut c_void, u32, *mut DIDEVICEOBJECTDATA, *mut u32, u32) -> HRESULT;
#[cfg(test)]
pub(crate) type SetEventNotificationFn =
    unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT;

snowglass_hook::com_vtable! {
    /// Overlays the ABI-identical ANSI and wide `IDirectInput8` vtables.
    pub(crate) union IDirectInputVtable for IDirectInput8A_Vtbl;
}
const _: () = assert!(size_of::<IDirectInput8A_Vtbl>() == size_of::<IDirectInput8W_Vtbl>());

snowglass_hook::com_vtable! {
    /// Overlays the ABI-identical ANSI and wide `IDirectInputDevice8` vtables.
    pub(crate) union IDirectInputDeviceVtable for IDirectInputDevice8A_Vtbl;
}
const _: () =
    assert!(size_of::<IDirectInputDevice8A_Vtbl>() == size_of::<IDirectInputDevice8W_Vtbl>());

snowglass_hook::impl_com_hook! {
    pub(crate) registry DIRECT_INPUT_HOOKS;
    pub(crate) trait DirectInputHookState;
    impl DirectInputHookState for IDirectInputHook {
        vtable IDirectInputVtable;
        #[release]
        unsafe extern "system" fn direct_input_release(this: *mut c_void) -> u32 {
            context struct {
                runtime: std::sync::Arc<crate::runtime::Runtime>,
            }
        }

        #[vtable = IDirectInput8A_Vtbl]
        #[base = CreateDevice]
        #[fallback = 0x8000_4005u32 as i32]
        #[postfix(crate::runtime::create_device(class, output, result))]
        unsafe extern "system" fn create_device(this: *mut c_void, class: *const GUID, output: *mut *mut c_void, outer: *mut c_void) -> HRESULT {
            context |state| {
                runtime: std::sync::Arc<crate::runtime::Runtime> = state.runtime().clone(),
            }
        }
    }
}

snowglass_hook::impl_com_hook! {
    pub(crate) registry DEVICE_HOOKS;
    pub(crate) trait DeviceHookState;
    impl DeviceHookState for IDirectInputDeviceHook {
        vtable IDirectInputDeviceVtable;
        #[release(destroyed = crate::runtime::device_destroyed)]
        unsafe extern "system" fn device_release(this: *mut c_void) -> u32 {
            context struct {
                runtime: std::sync::Arc<crate::runtime::Runtime>,
                device: snowglass_input::DirectInputDevice,
            }
        }

        #[vtable = IDirectInputDevice8A_Vtbl]
        #[base = Acquire]
        #[fallback = 0x8000_4005u32 as i32]
        unsafe extern "system" fn acquire(this: *mut c_void) -> HRESULT {
            context |_state| {}
        }

        #[vtable = IDirectInputDevice8A_Vtbl]
        #[base = GetDeviceState]
        #[fallback = 0x8000_4005u32 as i32]
        #[infix(crate::runtime::get_device_state)]
        unsafe extern "system" fn get_device_state(this: *mut c_void, size: u32, output: *mut c_void) -> HRESULT {
            context |state| {
                runtime: std::sync::Arc<crate::runtime::Runtime> = state.runtime().clone(),
                device: snowglass_input::DirectInputDevice = *state.device(),
            }
        }

        #[vtable = IDirectInputDevice8A_Vtbl]
        #[base = GetDeviceData]
        #[fallback = 0x8000_4005u32 as i32]
        #[infix(crate::runtime::get_device_data)]
        unsafe extern "system" fn get_device_data(this: *mut c_void, object_size: u32, output: *mut DIDEVICEOBJECTDATA, count: *mut u32, flags: u32) -> HRESULT {
            context |state| {
                runtime: std::sync::Arc<crate::runtime::Runtime> = state.runtime().clone(),
                device: snowglass_input::DirectInputDevice = *state.device(),
            }
        }

        #[vtable = IDirectInputDevice8A_Vtbl]
        #[base = SetDataFormat]
        #[fallback = 0x8000_4005u32 as i32]
        unsafe extern "system" fn set_data_format(this: *mut c_void, format: *const c_void) -> HRESULT {
            context |_state| {}
        }

        #[vtable = IDirectInputDevice8A_Vtbl]
        #[base = SetEventNotification]
        #[fallback = 0x8000_4005u32 as i32]
        #[prefix(crate::runtime::set_event_notification(this, event))]
        unsafe extern "system" fn set_event_notification(this: *mut c_void, event: *mut c_void) -> HRESULT {
            context |state| {
                runtime: std::sync::Arc<crate::runtime::Runtime> = state.runtime().clone(),
                device: snowglass_input::DirectInputDevice = *state.device(),
            }
        }

        #[vtable = IDirectInputDevice8A_Vtbl]
        #[base = SetCooperativeLevel]
        #[fallback = 0x8000_4005u32 as i32]
        unsafe extern "system" fn set_cooperative_level(this: *mut c_void, window: *mut c_void, flags: u32) -> HRESULT {
            context |_state| {}
        }
    }
}
