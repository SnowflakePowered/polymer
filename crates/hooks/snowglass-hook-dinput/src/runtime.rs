//! DirectInput device interception and forwarded input state.
#![allow(unsafe_code)]
#![allow(non_snake_case)]

use crate::vtable::{DeviceHookState, IDirectInputDeviceHook, IDirectInputHook};
use snowglass_hook::config;
use snowglass_input::DirectInputDevice;
use snowglass_input_dinput::Runtime as InputRuntime;
use std::{
    ffi::c_void,
    fs::OpenOptions,
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use windows::core::Interface;
use windows_sys::{
    Win32::{
        Devices::HumanInterfaceDevice::{
            DIDEVICEOBJECTDATA, DIGDD_PEEK, GUID_SysKeyboard, GUID_SysMouse,
        },
        Foundation::HINSTANCE,
    },
    core::{GUID, HRESULT},
};

pub(crate) struct Runtime {
    logged_delivery: AtomicBool,
    logged_keyboard_poll: AtomicBool,
    logged_keyboard_pressed: AtomicBool,
    input: Arc<InputRuntime>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            logged_delivery: AtomicBool::new(false),
            logged_keyboard_poll: AtomicBool::new(false),
            logged_keyboard_pressed: AtomicBool::new(false),
            input: Arc::new(InputRuntime::new(input_log)),
        }
    }
}

fn dinput_log(message: impl std::fmt::Display) {
    let Some(path) = config::dinput_log_file() else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{message}");
    }
}

fn input_log(message: &str) {
    dinput_log(message);
}

impl Runtime {
    pub(crate) fn start_input_receiver(self: &Arc<Self>) {
        let Ok(session) = config::session() else {
            return;
        };
        self.input.start_receiver(format!("{session}-input"));
    }

    pub(crate) fn set_event_notification(
        &self,
        key: usize,
        device: DirectInputDevice,
        event: usize,
    ) {
        self.input.set_event_notification(key, device, event);
    }
}

fn guid_matches_interface<T: Interface>(guid: &GUID) -> bool {
    windows::core::GUID::from_values(guid.data1, guid.data2, guid.data3, guid.data4) == T::IID
}

unsafe fn wrap_direct_input(
    runtime: Arc<Runtime>,
    interface: *mut c_void,
    iid: &GUID,
) -> Option<*mut c_void> {
    let (wide, iid) = if guid_matches_interface::<
        windows::Win32::Devices::HumanInterfaceDevice::IDirectInput8W,
    >(iid)
    {
        (
            true,
            windows::Win32::Devices::HumanInterfaceDevice::IDirectInput8W::IID,
        )
    } else if guid_matches_interface::<windows::Win32::Devices::HumanInterfaceDevice::IDirectInput8A>(
        iid,
    ) {
        (
            false,
            windows::Win32::Devices::HumanInterfaceDevice::IDirectInput8A::IID,
        )
    } else {
        return None;
    };
    unsafe {
        IDirectInputHook::<windows::Win32::Devices::HumanInterfaceDevice::IDirectInput8A_Vtbl>::wrap(
            interface,
            iid,
            crate::vtable::DirectInputContext { runtime, wide },
        )
    }
}

pub(crate) unsafe fn create_device(
    class: *const GUID,
    output: *mut *mut c_void,
    result: HRESULT,
    context: &crate::vtable::CreateDeviceContext<'_>,
) {
    let runtime = context.common.runtime.clone();
    if !class.is_null() {
        let class = unsafe { &*class };
        dinput_log(format!(
            "snowglass: DirectInput CreateDevice {:08x}-{:04x}-{:04x}",
            class.data1, class.data2, class.data3
        ));
    }
    dinput_log(format!(
        "snowglass: DirectInput CreateDevice returned {result:#x} interface={:#x}",
        if output.is_null() {
            0
        } else {
            (unsafe { *output }) as usize
        }
    ));
    if result >= 0 && !class.is_null() && !output.is_null() {
        let device_class = if unsafe { guid_eq(&*class, &GUID_SysKeyboard) } {
            Some(DirectInputDevice::Keyboard)
        } else if unsafe { guid_eq(&*class, &GUID_SysMouse) } {
            Some(DirectInputDevice::Mouse)
        } else {
            None
        };
        if let Some(device_class) = device_class {
            if let Some(proxy) =
                unsafe { wrap_device(runtime, *output, device_class, context.common.wide) }
            {
                unsafe { *output = proxy };
            }
            dinput_log(format!(
                "snowglass: hooked {device_class:?} DirectInput device"
            ));
        }
    }
}

fn guid_eq(left: &GUID, right: &GUID) -> bool {
    left.data1 == right.data1
        && left.data2 == right.data2
        && left.data3 == right.data3
        && left.data4 == right.data4
}

unsafe fn wrap_device(
    runtime: Arc<Runtime>,
    interface: *mut c_void,
    device: DirectInputDevice,
    wide: bool,
) -> Option<*mut c_void> {
    let iid = if wide {
        windows::Win32::Devices::HumanInterfaceDevice::IDirectInputDevice8W::IID
    } else {
        windows::Win32::Devices::HumanInterfaceDevice::IDirectInputDevice8A::IID
    };
    unsafe {
        IDirectInputDeviceHook::<
            windows::Win32::Devices::HumanInterfaceDevice::IDirectInputDevice8A_Vtbl,
        >::wrap(
            interface,
            iid,
            crate::vtable::DeviceContext { runtime, device },
        )
    }
}

pub(crate) unsafe fn get_device_state(
    this: *mut c_void,
    size: u32,
    output: *mut c_void,
    mut next: crate::vtable::GetDeviceStateContext<'_>,
) -> HRESULT {
    let device = &next.common.device;
    if !next.common.runtime.input.connected() {
        return snowglass_hook::next!(next, this, size, output);
    }
    if output.is_null() {
        return 0x8007_0057u32 as i32;
    }
    match device {
        DirectInputDevice::Keyboard => {
            let state = next.common.runtime.input.keyboard_state();
            if size as usize != state.len() {
                return 0x8007_0057u32 as i32;
            }
            if !next
                .common
                .runtime
                .logged_keyboard_poll
                .swap(true, Ordering::AcqRel)
            {
                dinput_log("snowglass: target first polled forwarded keyboard state");
            }
            if state.iter().any(|state| *state != 0)
                && !next
                    .common
                    .runtime
                    .logged_keyboard_pressed
                    .swap(true, Ordering::AcqRel)
            {
                dinput_log("snowglass: target polled a pressed forwarded keyboard key");
            }
            unsafe {
                std::ptr::copy_nonoverlapping(state.as_ptr(), output.cast::<u8>(), state.len());
            }
        }
        DirectInputDevice::Mouse => {
            // DIMOUSESTATE is 16 bytes (4 buttons); DIMOUSESTATE2 is 20 bytes
            // (8 buttons). Both begin with three signed relative axes.
            if size != 16 && size != 20 {
                return 0x8007_0057u32 as i32;
            }
            let state = next.common.runtime.input.take_mouse_state();
            let output = output.cast::<u8>();
            for (index, axis) in state.axes.iter().enumerate() {
                unsafe { std::ptr::write_unaligned(output.add(index * 4).cast::<i32>(), *axis) };
            }
            let button_count = size as usize - 12;
            unsafe {
                std::ptr::copy_nonoverlapping(state.buttons.as_ptr(), output.add(12), button_count);
            }
        }
    }
    0
}

pub(crate) unsafe fn device_destroyed(this: *mut c_void, state: &dyn DeviceHookState) {
    state.common().runtime.input.remove_device(this as usize);
}

pub(crate) unsafe fn get_device_data(
    this: *mut c_void,
    object_size: u32,
    output: *mut DIDEVICEOBJECTDATA,
    count: *mut u32,
    flags: u32,
    mut next: crate::vtable::GetDeviceDataContext<'_>,
) -> HRESULT {
    let device = &next.common.device;
    if !next.common.runtime.input.connected() {
        return snowglass_hook::next!(next, this, object_size, output, count, flags);
    }
    if count.is_null() {
        return 0x8007_0057u32 as i32;
    }

    let requested = if output.is_null() {
        next.common.runtime.input.buffered_len(*device)
    } else {
        (unsafe { *count }) as usize
    };
    if output.is_null() {
        unsafe { *count = requested as u32 };
        return 0;
    }
    if object_size < 16 {
        return 0x8007_0057u32 as i32;
    }
    let peek = flags & DIGDD_PEEK != 0;
    let events = next
        .common
        .runtime
        .input
        .buffered_data(*device, requested, peek);
    unsafe { *count = events.len() as u32 };
    for (index, event) in events.iter().copied().enumerate() {
        let destination = unsafe { (output.cast::<u8>()).add(index * object_size as usize) };
        unsafe {
            std::ptr::write_unaligned(destination.cast::<u32>(), event.offset);
            std::ptr::write_unaligned(destination.add(4).cast::<u32>(), event.data);
            std::ptr::write_unaligned(destination.add(8).cast::<u32>(), event.timestamp);
            std::ptr::write_unaligned(destination.add(12).cast::<u32>(), event.sequence);
            if object_size as usize >= std::mem::size_of::<DIDEVICEOBJECTDATA>() {
                std::ptr::write_unaligned(destination.add(16).cast::<usize>(), 0);
            }
        }
    }
    if !events.is_empty()
        && !next
            .common
            .runtime
            .logged_delivery
            .swap(true, Ordering::AcqRel)
    {
        dinput_log(format!(
            "snowglass: returned first forwarded {device:?} record from GetDeviceData"
        ));
    }
    0
}

pub(crate) unsafe fn direct_input_8_create(
    instance: HINSTANCE,
    version: u32,
    interface_id: *const GUID,
    output: *mut *mut c_void,
    outer: *mut c_void,
    result: HRESULT,
    context: &crate::hooks::DirectInput8CreateContext,
) {
    context.common.runtime.start_input_receiver();
    dinput_log(format!(
        "snowglass: DirectInput8Create version={version:#x} iid={interface_id:p} returned {result:#x}"
    ));
    if result >= 0 && !interface_id.is_null() && !output.is_null() && outer.is_null() {
        if let Some(proxy) =
            unsafe { wrap_direct_input(context.common.runtime.clone(), *output, &*interface_id) }
        {
            unsafe { *output = proxy };
        }
    }
    let _ = (instance, outer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vtable::{
        DEVICE_VTABLE_LEN, GetDeviceDataFn, GetDeviceStateFn, SetEventNotificationFn,
    };
    use snowglass_input::DirectInputData;
    use snowglass_ipc::{LocalSocketListener, Message};
    use std::{
        thread,
        time::{Duration, Instant},
    };
    use windows::Win32::Devices::HumanInterfaceDevice::IDirectInputDevice8A_Vtbl;

    const SLOT_SIZE: usize = size_of::<usize>();
    const GET_DEVICE_STATE_INDEX: usize =
        std::mem::offset_of!(IDirectInputDevice8A_Vtbl, GetDeviceState) / SLOT_SIZE;
    const GET_DEVICE_DATA_INDEX: usize =
        std::mem::offset_of!(IDirectInputDevice8A_Vtbl, GetDeviceData) / SLOT_SIZE;
    const SET_EVENT_NOTIFICATION_INDEX: usize =
        std::mem::offset_of!(IDirectInputDevice8A_Vtbl, SetEventNotification) / SLOT_SIZE;

    unsafe extern "system" fn original_get_data_stub(
        _this: *mut c_void,
        _object_size: u32,
        _output: *mut DIDEVICEOBJECTDATA,
        _count: *mut u32,
        _flags: u32,
    ) -> HRESULT {
        0x8000_4005u32 as i32
    }

    unsafe extern "system" fn original_get_state_stub(
        _this: *mut c_void,
        _size: u32,
        _output: *mut c_void,
    ) -> HRESULT {
        0x8000_4005u32 as i32
    }

    unsafe extern "system" fn original_set_event_stub(
        _this: *mut c_void,
        _event: *mut c_void,
    ) -> HRESULT {
        0
    }

    #[test]
    fn forwarded_record_is_returned_by_device_get_data() {
        let session = format!("snowglass-dinput-test-{}", std::process::id());
        let listener = LocalSocketListener::bind(&format!("{session}-input")).unwrap();
        unsafe { std::env::set_var(config::SESSION, &session) };
        let _ = unsafe { crate::install::install() };
        let runtime = crate::hooks::HookInstallation::active_state();
        runtime.start_input_receiver();
        let mut socket = listener.accept().unwrap();

        let mut original_vtable = [0usize; DEVICE_VTABLE_LEN];
        original_vtable[GET_DEVICE_STATE_INDEX] = original_get_state_stub as *const () as usize;
        original_vtable[GET_DEVICE_DATA_INDEX] = original_get_data_stub as *const () as usize;
        original_vtable[SET_EVENT_NOTIFICATION_INDEX] =
            original_set_event_stub as *const () as usize;
        let mut object_vtable = original_vtable.as_ptr();
        let inner_keyboard = (&raw mut object_vtable).cast::<c_void>();
        let keyboard = unsafe {
            wrap_device(
                runtime.clone(),
                inner_keyboard,
                DirectInputDevice::Keyboard,
                false,
            )
        }
        .unwrap();
        let event = unsafe {
            windows_sys::Win32::System::Threading::CreateEventW(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
            )
        };
        assert!(!event.is_null());
        let keyboard_vtable = unsafe { *keyboard.cast::<*const usize>() };
        let set_event: SetEventNotificationFn = unsafe {
            std::mem::transmute::<usize, SetEventNotificationFn>(
                *keyboard_vtable.add(SET_EVENT_NOTIFICATION_INDEX),
            )
        };
        assert_eq!(unsafe { set_event(keyboard, event) }, 0);
        socket
            .send(Message::DirectInputData(DirectInputData {
                device: DirectInputDevice::Keyboard,
                offset: 0x1e,
                data: 0x80,
                timestamp: 12,
                sequence: 34,
            }))
            .unwrap();
        assert_eq!(
            unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(event, 1000) },
            windows_sys::Win32::Foundation::WAIT_OBJECT_0
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.input.buffered_len(DirectInputDevice::Keyboard) == 0
            && Instant::now() < deadline
        {
            thread::yield_now();
        }
        let get_data: GetDeviceDataFn = unsafe {
            std::mem::transmute::<usize, GetDeviceDataFn>(
                *keyboard_vtable.add(GET_DEVICE_DATA_INDEX),
            )
        };
        let mut record = DIDEVICEOBJECTDATA::default();
        let mut count = 1;
        let result = unsafe {
            get_data(
                keyboard,
                std::mem::size_of::<DIDEVICEOBJECTDATA>() as u32,
                &mut record,
                &mut count,
                0,
            )
        };
        assert_eq!(result, 0);
        assert_eq!(count, 1);
        assert_eq!(record.dwOfs, 0x1e);
        assert_eq!(record.dwData, 0x80);
        assert_eq!(record.dwTimeStamp, 12);
        assert_eq!(record.dwSequence, 34);

        let get_state: GetDeviceStateFn = unsafe {
            std::mem::transmute::<usize, GetDeviceStateFn>(
                *keyboard_vtable.add(GET_DEVICE_STATE_INDEX),
            )
        };
        let mut state = [0u8; 256];
        assert_eq!(
            unsafe { get_state(keyboard, state.len() as u32, state.as_mut_ptr().cast()) },
            0
        );
        assert_eq!(state[0x1e], 0x80);
        unsafe { windows_sys::Win32::Foundation::CloseHandle(event) };
    }
}
