//! DirectInput device interception and forwarded input state.
#![allow(unsafe_code)]
#![allow(non_snake_case)]

use crate::vtable::{IDirectInputDeviceHook, IDirectInputHook};
use snowglass_hook::{HookChain, config};
use snowglass_input::DirectInputDevice;
use snowglass_input_dinput::Runtime as InputRuntime;
use std::{
    collections::HashMap,
    ffi::c_void,
    fs::OpenOptions,
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
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
    direct_input_hooks: Mutex<HashMap<usize, DirectInputHook>>,
    device_hooks: Mutex<HashMap<usize, DeviceHook>>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            logged_delivery: AtomicBool::new(false),
            logged_keyboard_poll: AtomicBool::new(false),
            logged_keyboard_pressed: AtomicBool::new(false),
            input: Arc::new(InputRuntime::new(input_log)),
            direct_input_hooks: Mutex::new(HashMap::new()),
            device_hooks: Mutex::new(HashMap::new()),
        }
    }
}

pub(crate) struct DirectInputHook {
    vtable: IDirectInputHook,
}

pub(crate) struct DeviceHook {
    vtable: IDirectInputDeviceHook,
    device: DirectInputDevice,
}

impl Runtime {
    pub(crate) fn direct_input_method<T: Copy>(
        &self,
        this: *mut c_void,
        select: impl FnOnce(&IDirectInputHook) -> T,
    ) -> Option<crate::vtable::OriginalMethod<T>> {
        let hooks = self
            .direct_input_hooks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let hook = hooks.get(&(this as usize))?;
        Some(crate::vtable::OriginalMethod {
            original_vtable: hook.vtable.original_vtable(),
            intercepted_vtable: hook.vtable.patched_vtable(),
            function: select(&hook.vtable),
        })
    }

    pub(crate) fn device_method<T: Copy>(
        &self,
        this: *mut c_void,
        select: impl FnOnce(&IDirectInputDeviceHook) -> T,
    ) -> Option<crate::vtable::OriginalMethod<T>> {
        let hooks = self
            .device_hooks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let hook = hooks.get(&(this as usize))?;
        Some(crate::vtable::OriginalMethod {
            original_vtable: hook.vtable.original_vtable(),
            intercepted_vtable: hook.vtable.patched_vtable(),
            function: select(&hook.vtable),
        })
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

    pub(crate) unsafe fn release(
        this: *mut c_void,
        next: crate::hooks::DirectInputReleaseContext<'_>,
    ) -> u32 {
        let (original_vtable, patched_vtable, original_release) = {
            let hooks = next
                .runtime
                .direct_input_hooks
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(hook) = hooks.get(&(this as usize)) else {
                return 0;
            };
            (
                hook.vtable.original_vtable(),
                hook.vtable.patched_vtable(),
                hook.vtable.release(),
            )
        };
        let object_vtable = this.cast::<*const usize>();
        unsafe { *object_vtable = original_vtable };
        let remaining = unsafe { original_release(this) };
        if remaining == 0 {
            next.runtime
                .direct_input_hooks
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&(this as usize));
        } else {
            unsafe { *object_vtable = patched_vtable };
        }
        remaining
    }

    unsafe fn install(runtime: &Runtime, interface: *mut c_void) {
        if interface.is_null() {
            return;
        }
        let key = interface as usize;
        let mut hooks = runtime
            .direct_input_hooks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if hooks.contains_key(&key) {
            return;
        }
        // SAFETY: a successful DirectInput8Create result is a COM object whose first
        // field is an IDirectInput8A/W vtable pointer. Both variants share this layout.
        let object_vtable = interface.cast::<*const usize>();
        let Some(vtable) = (unsafe { IDirectInputHook::clone_from(*object_vtable) }) else {
            return;
        };
        unsafe { *object_vtable = vtable.patched_vtable() };
        hooks.insert(key, DirectInputHook { vtable });
    }
}

pub(crate) unsafe fn create_device(
    _this: *mut c_void,
    class: *const GUID,
    output: *mut *mut c_void,
    _outer: *mut c_void,
    result: HRESULT,
    context: &crate::hooks::CreateDeviceContext,
) {
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
            unsafe { hook_device(&context.runtime, *output, device_class) };
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

unsafe fn hook_device(runtime: &Runtime, interface: *mut c_void, device: DirectInputDevice) {
    if interface.is_null() {
        return;
    }
    let key = interface as usize;
    let mut hooks = runtime
        .device_hooks
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if hooks.contains_key(&key) {
        return;
    }
    let object_vtable = interface.cast::<*const usize>();
    let Some(vtable) = (unsafe { IDirectInputDeviceHook::clone_from(*object_vtable) }) else {
        return;
    };
    unsafe { *object_vtable = vtable.patched_vtable() };
    hooks.insert(key, DeviceHook { vtable, device });
}

pub(crate) unsafe fn get_device_state(
    this: *mut c_void,
    size: u32,
    output: *mut c_void,
    mut next: crate::hooks::GetDeviceStateContext<'_>,
) -> HRESULT {
    let device = {
        let hooks = next
            .runtime
            .device_hooks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get(&(this as usize)) else {
            return 0x8000_4005u32 as i32;
        };
        hook.device
    };
    if !next.runtime.input.connected() {
        let continuation = *next
            .fp_next()
            .expect("DirectInput hook chains always have a terminal");
        return unsafe { continuation(this, size, output, next) };
    }
    if output.is_null() {
        return 0x8007_0057u32 as i32;
    }
    match device {
        DirectInputDevice::Keyboard => {
            let state = next.runtime.input.keyboard_state();
            if size as usize != state.len() {
                return 0x8007_0057u32 as i32;
            }
            if !next
                .runtime
                .logged_keyboard_poll
                .swap(true, Ordering::AcqRel)
            {
                dinput_log("snowglass: target first polled forwarded keyboard state");
            }
            if state.iter().any(|state| *state != 0)
                && !next
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
            let state = next.runtime.input.take_mouse_state();
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

pub(crate) unsafe fn device_release(
    this: *mut c_void,
    next: crate::hooks::DeviceReleaseContext<'_>,
) -> u32 {
    let (original_vtable, patched_vtable, original_release) = {
        let hooks = next
            .runtime
            .device_hooks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get(&(this as usize)) else {
            return 0;
        };
        (
            hook.vtable.original_vtable(),
            hook.vtable.patched_vtable(),
            hook.vtable.release(),
        )
    };
    let object_vtable = this.cast::<*const usize>();
    unsafe { *object_vtable = original_vtable };
    let remaining = unsafe { original_release(this) };
    if remaining == 0 {
        next.runtime
            .device_hooks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(this as usize));
        next.runtime.input.remove_device(this as usize);
    } else {
        unsafe { *object_vtable = patched_vtable };
    }
    remaining
}

pub(crate) unsafe fn set_event_notification(
    this: *mut c_void,
    event: *mut c_void,
    context: &crate::hooks::SetEventNotificationContext,
) {
    let device = {
        let hooks = context
            .runtime
            .device_hooks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get(&(this as usize)) else {
            return;
        };
        hook.device
    };
    context
        .runtime
        .input
        .set_event_notification(this as usize, device, event as usize);
}

pub(crate) unsafe fn get_device_data(
    this: *mut c_void,
    object_size: u32,
    output: *mut DIDEVICEOBJECTDATA,
    count: *mut u32,
    flags: u32,
    mut next: crate::hooks::GetDeviceDataContext<'_>,
) -> HRESULT {
    let device = {
        let hooks = next
            .runtime
            .device_hooks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get(&(this as usize)) else {
            return 0x8000_4005u32 as i32;
        };
        hook.device
    };
    if !next.runtime.input.connected() {
        let continuation = *next
            .fp_next()
            .expect("DirectInput hook chains always have a terminal");
        return unsafe { continuation(this, object_size, output, count, flags, next) };
    }
    if count.is_null() {
        return 0x8007_0057u32 as i32;
    }

    let requested = if output.is_null() {
        next.runtime.input.buffered_len(device)
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
    let events = next.runtime.input.buffered_data(device, requested, peek);
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
    if !events.is_empty() && !next.runtime.logged_delivery.swap(true, Ordering::AcqRel) {
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
    context.runtime.start_input_receiver();
    dinput_log(format!(
        "snowglass: DirectInput8Create version={version:#x} iid={interface_id:p} returned {result:#x}"
    ));
    if result >= 0 && !output.is_null() {
        unsafe { Runtime::install(&context.runtime, *output) };
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
        let runtime = crate::install::runtime();
        runtime.start_input_receiver();
        let mut socket = listener.accept().unwrap();

        let mut original_vtable = [0usize; DEVICE_VTABLE_LEN];
        original_vtable[GET_DEVICE_STATE_INDEX] = original_get_state_stub as *const () as usize;
        original_vtable[GET_DEVICE_DATA_INDEX] = original_get_data_stub as *const () as usize;
        original_vtable[SET_EVENT_NOTIFICATION_INDEX] =
            original_set_event_stub as *const () as usize;
        let mut object_vtable = original_vtable.as_ptr();
        let keyboard = (&raw mut object_vtable).cast::<c_void>();
        unsafe { hook_device(&runtime, keyboard, DirectInputDevice::Keyboard) };
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
