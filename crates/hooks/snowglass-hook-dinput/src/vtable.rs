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
use windows::core::IUnknown_Vtbl;
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

/// A continuation resolved from a hooked object, together with the vtable
/// pointers needed to unhook the object across the downstream call.
pub(crate) struct OriginalMethod<T> {
    pub(crate) original_vtable: *const usize,
    pub(crate) intercepted_vtable: *const usize,
    pub(crate) function: T,
}

pub(crate) const DIRECT_INPUT_VTABLE_LEN: usize = <IDirectInput8A_Vtbl as Vtable>::SLOTS;
pub(crate) const DEVICE_VTABLE_LEN: usize = <IDirectInputDevice8A_Vtbl as Vtable>::SLOTS;

pub(crate) type CreateDeviceFn =
    unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void, *mut c_void) -> HRESULT;
pub(crate) type GetDeviceStateFn =
    unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> HRESULT;
pub(crate) type AcquireFn = unsafe extern "system" fn(*mut c_void) -> HRESULT;
pub(crate) type SetDataFormatFn = unsafe extern "system" fn(*mut c_void, *const c_void) -> HRESULT;
pub(crate) type SetCooperativeLevelFn =
    unsafe extern "system" fn(*mut c_void, *mut c_void, u32) -> HRESULT;
pub(crate) type GetDeviceDataFn =
    unsafe extern "system" fn(*mut c_void, u32, *mut DIDEVICEOBJECTDATA, *mut u32, u32) -> HRESULT;
pub(crate) type SetEventNotificationFn =
    unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT;
pub(crate) type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;

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
    impl IDirectInputHook {
        vtable IDirectInputVtable;
        slots DIRECT_INPUT_VTABLE_LEN;
        patch |patched| {
        // SAFETY: the slots written below were copied above, so each place
        // projection targets an initialized entry within the clone.
        unsafe {
            let table = patched.as_mut_ptr::<IDirectInput8A_Vtbl>();
            (*table).base__.Release = crate::sys::direct_input_release;
            (*table).CreateDevice =
                std::mem::transmute(crate::sys::create_device as CreateDeviceFn);
        }
        }

        #[vtable = IUnknown_Vtbl]
        #[base = Release]
        fn release(&self) -> ReleaseFn;
        #[vtable = IDirectInput8A_Vtbl]
        #[base = CreateDevice]
        fn create_device(&self) -> CreateDeviceFn;
    }
}

snowglass_hook::impl_com_hook! {
    impl IDirectInputDeviceHook {
        vtable IDirectInputDeviceVtable;
        slots DEVICE_VTABLE_LEN;
        patch |patched| {
        // SAFETY: the slots written below were copied above, so each place
        // projection targets an initialized entry within the clone.
        unsafe {
            let table = patched.as_mut_ptr::<IDirectInputDevice8A_Vtbl>();
            (*table).base__.Release = crate::sys::device_release;
            (*table).Acquire = std::mem::transmute(crate::sys::acquire as AcquireFn);
            (*table).GetDeviceState =
                std::mem::transmute(crate::sys::get_device_state as GetDeviceStateFn);
            (*table).GetDeviceData =
                std::mem::transmute(crate::sys::get_device_data as GetDeviceDataFn);
            (*table).SetDataFormat =
                std::mem::transmute(crate::sys::set_data_format as SetDataFormatFn);
            (*table).SetEventNotification =
                std::mem::transmute(crate::sys::set_event_notification as SetEventNotificationFn);
            (*table).SetCooperativeLevel =
                std::mem::transmute(crate::sys::set_cooperative_level as SetCooperativeLevelFn);
        }
        }

        #[vtable = IUnknown_Vtbl]
        #[base = Release]
        fn release(&self) -> ReleaseFn;
        #[vtable = IDirectInputDevice8A_Vtbl]
        #[base = Acquire]
        fn acquire(&self) -> AcquireFn;
        #[vtable = IDirectInputDevice8A_Vtbl]
        #[base = GetDeviceState]
        fn get_device_state(&self) -> GetDeviceStateFn;
        #[vtable = IDirectInputDevice8A_Vtbl]
        #[base = GetDeviceData]
        fn get_device_data(&self) -> GetDeviceDataFn;
        #[vtable = IDirectInputDevice8A_Vtbl]
        #[base = SetDataFormat]
        fn set_data_format(&self) -> SetDataFormatFn;
        #[vtable = IDirectInputDevice8A_Vtbl]
        #[base = SetEventNotification]
        fn set_event_notification(&self) -> SetEventNotificationFn;
        #[vtable = IDirectInputDevice8A_Vtbl]
        #[base = SetCooperativeLevel]
        fn set_cooperative_level(&self) -> SetCooperativeLevelFn;
    }
}
