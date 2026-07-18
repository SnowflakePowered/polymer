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
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use windows::Win32::Devices::HumanInterfaceDevice::{
    IDirectInput8A_Vtbl, IDirectInput8W_Vtbl, IDirectInputDevice8A_Vtbl, IDirectInputDevice8W_Vtbl,
};
use windows_sys::{
    Win32::Devices::HumanInterfaceDevice::DIDEVICEOBJECTDATA,
    core::{GUID, HRESULT},
};

snowglass_hook::vtable! {
    IDirectInput8A_Vtbl,
    IDirectInput8W_Vtbl,
    IDirectInputDevice8A_Vtbl,
    IDirectInputDevice8W_Vtbl,
}

/// A continuation resolved from a hooked object, together with the vtable
/// pointers needed to unhook the object across the downstream call.
pub(crate) struct OriginalMethod<T> {
    pub(crate) original_vtable: *const usize,
    pub(crate) intercepted_vtable: *const usize,
    pub(crate) function: T,
}

const SLOT_SIZE: usize = size_of::<usize>();
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

/// Overlays the ANSI and wide `IDirectInput8` vtables on one allocation. The
/// two variants differ only in string payload types, never in slot layout, so
/// either view describes objects of both variants; `raw` spans the layout and
/// is the untyped view used for copying and for the pointer installed into
/// hooked objects.
#[repr(C)]
pub(crate) union IDirectInputVtable {
    raw: [usize; DIRECT_INPUT_VTABLE_LEN],
    ansi: ManuallyDrop<IDirectInput8A_Vtbl>,
    wide: ManuallyDrop<IDirectInput8W_Vtbl>,
}

const _: () = assert!(size_of::<IDirectInputVtable>() == DIRECT_INPUT_VTABLE_LEN * SLOT_SIZE);
const _: () = assert!(size_of::<IDirectInput8A_Vtbl>() == size_of::<IDirectInput8W_Vtbl>());

impl IDirectInputVtable {
    /// # Safety
    ///
    /// `original` must point to a vtable spanning at least
    /// [`DIRECT_INPUT_VTABLE_LEN`] entries.
    unsafe fn read(original: *const usize) -> Self {
        let mut slots = Self {
            raw: [0; DIRECT_INPUT_VTABLE_LEN],
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                original,
                (&raw mut slots.raw).cast::<usize>(),
                DIRECT_INPUT_VTABLE_LEN,
            );
        }
        slots
    }

    fn table(&self) -> *const IDirectInput8A_Vtbl {
        (&raw const self.ansi).cast()
    }

    fn table_mut(&mut self) -> *mut IDirectInput8A_Vtbl {
        (&raw mut self.ansi).cast()
    }
}

/// Overlays the ANSI and wide `IDirectInputDevice8` vtables on one allocation,
/// mirroring [`DirectInputSlots`].
#[repr(C)]
pub(crate) union IDirectInputDeviceVtable {
    raw: [usize; DEVICE_VTABLE_LEN],
    ansi: ManuallyDrop<IDirectInputDevice8A_Vtbl>,
    wide: ManuallyDrop<IDirectInputDevice8W_Vtbl>,
}

const _: () = assert!(size_of::<IDirectInputDeviceVtable>() == DEVICE_VTABLE_LEN * SLOT_SIZE);
const _: () =
    assert!(size_of::<IDirectInputDevice8A_Vtbl>() == size_of::<IDirectInputDevice8W_Vtbl>());

impl IDirectInputDeviceVtable {
    /// # Safety
    ///
    /// `original` must point to a vtable spanning at least
    /// [`DEVICE_VTABLE_LEN`] entries.
    unsafe fn read(original: *const usize) -> Self {
        let mut slots = Self {
            raw: [0; DEVICE_VTABLE_LEN],
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                original,
                (&raw mut slots.raw).cast::<usize>(),
                DEVICE_VTABLE_LEN,
            );
        }
        slots
    }

    fn table(&self) -> *const IDirectInputDevice8A_Vtbl {
        (&raw const self.ansi).cast()
    }

    fn table_mut(&mut self) -> *mut IDirectInputDevice8A_Vtbl {
        (&raw mut self.ansi).cast()
    }
}

pub(crate) struct IDirectInputHook {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<IDirectInputVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: IDirectInputVtable,
    /// The object's original vtable pointer, restored across releases.
    original: NonNull<usize>,
}

// SAFETY: `original` identifies an immutable vtable owned by the hooked
// module for the process lifetime; the hook only reads through it.
unsafe impl Send for IDirectInputHook {}
unsafe impl Sync for IDirectInputHook {}

impl IDirectInputHook {
    /// # Safety
    ///
    /// `original` must be null or point to a live `IDirectInput8` vtable.
    pub(crate) unsafe fn clone_from(original: *const usize) -> Option<Self> {
        let original = NonNull::new(original.cast_mut())?;
        let saved = unsafe { IDirectInputVtable::read(original.as_ptr()) };
        let mut patched = Box::new(unsafe { IDirectInputVtable::read(original.as_ptr()) });
        // SAFETY: the slots written below were copied above, so each place
        // projection targets an initialized entry within the clone.
        unsafe {
            let table = patched.table_mut();
            (*table).base__.Release = crate::sys::direct_input_release;
            (*table).CreateDevice =
                std::mem::transmute(crate::sys::create_device as CreateDeviceFn);
        }
        Some(Self {
            patched,
            saved,
            original,
        })
    }

    /// The vtable pointer that intercepts a hooked object.
    pub(crate) fn patched_vtable(&self) -> *const usize {
        std::ptr::from_ref::<IDirectInputVtable>(&self.patched).cast()
    }

    /// The object's original vtable pointer.
    pub(crate) fn original_vtable(&self) -> *const usize {
        self.original.as_ptr()
    }

    pub(crate) fn release(&self) -> ReleaseFn {
        unsafe { (*self.saved.table()).base__.Release }
    }

    pub(crate) fn create_device(&self) -> CreateDeviceFn {
        unsafe { std::mem::transmute((*self.saved.table()).CreateDevice) }
    }
}

pub(crate) struct IDirectInputDeviceHook {
    /// Hook-patched clone installed as the object's vtable pointer.
    patched: Box<IDirectInputDeviceVtable>,
    /// Verbatim copy of the original slots for downstream continuation.
    saved: IDirectInputDeviceVtable,
    /// The object's original vtable pointer, restored across releases.
    original: NonNull<usize>,
}

// SAFETY: see `IDirectInputHook`.
unsafe impl Send for IDirectInputDeviceHook {}
unsafe impl Sync for IDirectInputDeviceHook {}

impl IDirectInputDeviceHook {
    /// # Safety
    ///
    /// `original` must be null or point to a live `IDirectInputDevice8`
    /// vtable.
    pub(crate) unsafe fn clone_from(original: *const usize) -> Option<Self> {
        let original = NonNull::new(original.cast_mut())?;
        let saved = unsafe { IDirectInputDeviceVtable::read(original.as_ptr()) };
        let mut patched = Box::new(unsafe { IDirectInputDeviceVtable::read(original.as_ptr()) });
        // SAFETY: the slots written below were copied above, so each place
        // projection targets an initialized entry within the clone.
        unsafe {
            let table = patched.table_mut();
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
        Some(Self {
            patched,
            saved,
            original,
        })
    }

    /// The vtable pointer that intercepts a hooked object.
    pub(crate) fn patched_vtable(&self) -> *const usize {
        std::ptr::from_ref::<IDirectInputDeviceVtable>(&self.patched).cast()
    }

    /// The object's original vtable pointer.
    pub(crate) fn original_vtable(&self) -> *const usize {
        self.original.as_ptr()
    }

    pub(crate) fn release(&self) -> ReleaseFn {
        unsafe { (*self.saved.table()).base__.Release }
    }

    pub(crate) fn acquire(&self) -> AcquireFn {
        unsafe { std::mem::transmute((*self.saved.table()).Acquire) }
    }

    pub(crate) fn get_device_state(&self) -> GetDeviceStateFn {
        unsafe { std::mem::transmute((*self.saved.table()).GetDeviceState) }
    }

    pub(crate) fn get_device_data(&self) -> GetDeviceDataFn {
        unsafe { std::mem::transmute((*self.saved.table()).GetDeviceData) }
    }

    pub(crate) fn set_data_format(&self) -> SetDataFormatFn {
        unsafe { std::mem::transmute((*self.saved.table()).SetDataFormat) }
    }

    pub(crate) fn set_event_notification(&self) -> SetEventNotificationFn {
        unsafe { std::mem::transmute((*self.saved.table()).SetEventNotification) }
    }

    pub(crate) fn set_cooperative_level(&self) -> SetCooperativeLevelFn {
        unsafe { std::mem::transmute((*self.saved.table()).SetCooperativeLevel) }
    }
}
