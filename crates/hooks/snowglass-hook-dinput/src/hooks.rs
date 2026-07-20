//! Concrete DirectInput hook-chain declarations.

use crate::{
    DirectInput8CreateFn,
    hook_chain::{
        __resolve_around_terminal, __resolve_observer_terminal, around_chain, chain,
        observer_chain, post_chain,
    },
};
use std::ffi::c_void;
use windows_sys::{
    Win32::Devices::HumanInterfaceDevice::DIDEVICEOBJECTDATA,
    Win32::Foundation::HINSTANCE,
    core::{GUID, HRESULT},
};

chain! {
    pub dispatch_direct_input_8_create, register_direct_input_8_create;
    after DirectInput8Create;
    chain DIRECT_INPUT_8_CREATE_CHAIN, context DirectInput8CreateContext {
        runtime: std::sync::Arc<crate::runtime::Runtime> = crate::install::runtime(),
    };
    hook DirectInput8CreateHook(instance: HINSTANCE, version: u32, interface_id: *const GUID, output: *mut *mut c_void, outer: *mut c_void) -> result: HRESULT;
    abi (original: DirectInput8CreateFn, instance: HINSTANCE, version: u32, interface_id: *const GUID, output: *mut *mut c_void, outer: *mut c_void);
    resolve with original: DirectInput8CreateFn;
}

chain! {
    pub(crate) dispatch_direct_input_release, register_direct_input_release;
    around DirectInputRelease;
    chain DIRECT_INPUT_RELEASE_CHAIN, context DirectInputReleaseContext {
        runtime: std::sync::Arc<crate::runtime::Runtime> = crate::install::runtime(),
    };
    hook DirectInputReleaseHook(this: *mut c_void) -> u32;
    resolve with crate::runtime::Runtime::release;
}

chain! {
    pub(crate) dispatch_create_device, register_create_device;
    after CreateDevice;
    chain CREATE_DEVICE_CHAIN, context CreateDeviceContext {
        runtime: std::sync::Arc<crate::runtime::Runtime> = crate::install::runtime(),
    };
    hook CreateDeviceHook(this: *mut c_void, class: *const GUID, output: *mut *mut c_void, outer: *mut c_void) -> result: HRESULT;
    resolve with direct_input, create_device -> 0x8000_4005u32 as i32;
}

chain! {
    pub(crate) dispatch_device_release, register_device_release;
    around Release;
    chain DEVICE_RELEASE_CHAIN, context DeviceReleaseContext {
        runtime: std::sync::Arc<crate::runtime::Runtime> = crate::install::runtime(),
    };
    hook DeviceReleaseHook(this: *mut c_void) -> u32;
    resolve with crate::runtime::device_release;
}

chain! {
    pub(crate) dispatch_acquire, register_acquire;
    around Acquire;
    chain ACQUIRE_CHAIN, context AcquireContext {
        runtime: std::sync::Arc<crate::runtime::Runtime> = crate::install::runtime(),
    };
    hook AcquireHook(this: *mut c_void) -> HRESULT;
    resolve with device, acquire -> 0x8000_4005u32 as i32;
}

chain! {
    pub(crate) dispatch_get_device_state, register_get_device_state;
    around GetDeviceState;
    chain GET_DEVICE_STATE_CHAIN, context GetDeviceStateContext {
        runtime: std::sync::Arc<crate::runtime::Runtime> = crate::install::runtime(),
    };
    hook GetDeviceStateHook(this: *mut c_void, size: u32, output: *mut c_void) -> HRESULT;
    resolve with device, get_device_state -> 0x8000_4005u32 as i32;
}

chain! {
    pub(crate) dispatch_set_data_format, register_set_data_format;
    around SetDataFormat;
    chain SET_DATA_FORMAT_CHAIN, context SetDataFormatContext {
        runtime: std::sync::Arc<crate::runtime::Runtime> = crate::install::runtime(),
    };
    hook SetDataFormatHook(this: *mut c_void, format: *const c_void) -> HRESULT;
    resolve with device, set_data_format -> 0x8000_4005u32 as i32;
}

chain! {
    pub(crate) dispatch_get_device_data, register_get_device_data;
    around GetDeviceData;
    chain GET_DEVICE_DATA_CHAIN, context GetDeviceDataContext {
        runtime: std::sync::Arc<crate::runtime::Runtime> = crate::install::runtime(),
    };
    hook GetDeviceDataHook(this: *mut c_void, object_size: u32, output: *mut DIDEVICEOBJECTDATA, count: *mut u32, flags: u32) -> HRESULT;
    resolve with device, get_device_data -> 0x8000_4005u32 as i32;
}

chain! {
    pub(crate) dispatch_set_cooperative_level, register_set_cooperative_level;
    around SetCooperativeLevel;
    chain SET_COOPERATIVE_LEVEL_CHAIN, context SetCooperativeLevelContext {
        runtime: std::sync::Arc<crate::runtime::Runtime> = crate::install::runtime(),
    };
    hook SetCooperativeLevelHook(this: *mut c_void, window: *mut c_void, flags: u32) -> HRESULT;
    resolve with device, set_cooperative_level -> 0x8000_4005u32 as i32;
}

chain! {
    pub(crate) dispatch_set_event_notification, register_set_event_notification;
    observes SetEventNotification;
    chain SET_EVENT_NOTIFICATION_CHAIN, context SetEventNotificationContext {
        runtime: std::sync::Arc<crate::runtime::Runtime> = crate::install::runtime(),
    };
    hook SetEventNotificationHook(this: *mut c_void, event: *mut c_void) -> HRESULT;
    resolve with device, set_event_notification -> 0x8000_4005u32 as i32;
}
