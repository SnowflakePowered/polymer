//! DirectInput device interception and forwarded input state.
#![allow(unsafe_code)]
#![allow(non_snake_case)]

use crate::vtable::{IDirectInputDeviceHook, IDirectInputHook};
use snowglass_hook::{HookChain, config};
use snowglass_input::{DirectInputData, DirectInputDevice};
use snowglass_ipc::{LocalSocket, Message};
use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    fs::OpenOptions,
    io::Write,
    sync::{
        LazyLock, Mutex, Once,
        atomic::{AtomicBool, Ordering},
    },
    thread,
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

static START_INPUT: Once = Once::new();
static INPUT_CONNECTED: AtomicBool = AtomicBool::new(false);
static LOGGED_DELIVERY: AtomicBool = AtomicBool::new(false);
static LOGGED_KEYBOARD_RECEIVE: AtomicBool = AtomicBool::new(false);
static LOGGED_KEYBOARD_POLL: AtomicBool = AtomicBool::new(false);
static LOGGED_KEYBOARD_PRESSED: AtomicBool = AtomicBool::new(false);
static INPUT_QUEUE: Mutex<InputQueues> = Mutex::new(InputQueues::new());
static DIRECT_INPUT_HOOKS: LazyLock<Mutex<HashMap<usize, DirectInputHook>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static DEVICE_HOOKS: LazyLock<Mutex<HashMap<usize, DeviceHook>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) struct DirectInputHook {
    vtable: IDirectInputHook,
}

pub(crate) struct DeviceHook {
    vtable: IDirectInputDeviceHook,
    device: DirectInputDevice,
    event_handle: usize,
}

pub(crate) fn direct_input_method<T: Copy>(
    this: *mut c_void,
    select: impl FnOnce(&IDirectInputHook) -> T,
) -> Option<crate::vtable::OriginalMethod<T>> {
    let hooks = DIRECT_INPUT_HOOKS
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
    this: *mut c_void,
    select: impl FnOnce(&IDirectInputDeviceHook) -> T,
) -> Option<crate::vtable::OriginalMethod<T>> {
    let hooks = DEVICE_HOOKS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let hook = hooks.get(&(this as usize))?;
    Some(crate::vtable::OriginalMethod {
        original_vtable: hook.vtable.original_vtable(),
        intercepted_vtable: hook.vtable.patched_vtable(),
        function: select(&hook.vtable),
    })
}

struct InputQueues {
    keyboard: VecDeque<DirectInputData>,
    mouse: VecDeque<DirectInputData>,
    keyboard_state: [u8; 256],
    mouse_axes: [i32; 3],
    mouse_buttons: [u8; 8],
}

impl InputQueues {
    const fn new() -> Self {
        Self {
            keyboard: VecDeque::new(),
            mouse: VecDeque::new(),
            keyboard_state: [0; 256],
            mouse_axes: [0; 3],
            mouse_buttons: [0; 8],
        }
    }

    fn get_mut(&mut self, device: DirectInputDevice) -> &mut VecDeque<DirectInputData> {
        match device {
            DirectInputDevice::Keyboard => &mut self.keyboard,
            DirectInputDevice::Mouse => &mut self.mouse,
        }
    }

    fn apply(&mut self, event: DirectInputData) {
        match event.device {
            DirectInputDevice::Keyboard if (event.offset as usize) < self.keyboard_state.len() => {
                self.keyboard_state[event.offset as usize] = event.data as u8;
            }
            DirectInputDevice::Mouse => match event.offset {
                0 | 4 | 8 => {
                    let axis = (event.offset / 4) as usize;
                    self.mouse_axes[axis] = self.mouse_axes[axis].wrapping_add(event.data as i32);
                }
                12..=19 => {
                    self.mouse_buttons[(event.offset - 12) as usize] = event.data as u8;
                }
                _ => {}
            },
            _ => {}
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

fn start_input_receiver() {
    START_INPUT.call_once(|| {
        let Ok(session) = config::session() else {
            return;
        };
        thread::spawn(move || receive_input(format!("{session}-input")));
    });
}

fn receive_input(session: String) {
    let Ok(mut socket) = LocalSocket::connect(&session) else {
        dinput_log("snowglass: DirectInput input socket connection failed");
        return;
    };
    INPUT_CONNECTED.store(true, Ordering::Release);
    dinput_log("snowglass: DirectInput input socket connected");
    while let Ok(message) = socket.receive() {
        match message {
            Message::DirectInputData(event) => {
                if event.device == DirectInputDevice::Keyboard
                    && !LOGGED_KEYBOARD_RECEIVE.swap(true, Ordering::AcqRel)
                {
                    dinput_log(format!(
                        "snowglass: received first keyboard transition offset={:#x} data={:#x}",
                        event.offset, event.data
                    ));
                }
                let mut queues = INPUT_QUEUE
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                queues.apply(event);
                let queue = queues.get_mut(event.device);
                // Match the bounded behavior of a DirectInput buffered device.
                if queue.len() == 1024 {
                    queue.pop_front();
                }
                queue.push_back(event);
                drop(queues);
                let handles = DEVICE_HOOKS
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .values()
                    .filter(|hook| hook.device == event.device && hook.event_handle != 0)
                    .map(|hook| hook.event_handle)
                    .collect::<Vec<_>>();
                for handle in handles {
                    unsafe { windows_sys::Win32::System::Threading::SetEvent(handle as _) };
                }
            }
            Message::Shutdown => break,
            _ => {}
        }
    }
    INPUT_CONNECTED.store(false, Ordering::Release);
}

unsafe fn hook_direct_input(interface: *mut c_void) {
    if interface.is_null() {
        return;
    }
    let key = interface as usize;
    let mut hooks = DIRECT_INPUT_HOOKS
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

pub(crate) unsafe fn direct_input_release(
    this: *mut c_void,
    _next: crate::hooks::DirectInputReleaseContext<'_>,
) -> u32 {
    let (original_vtable, patched_vtable, original_release) = {
        let hooks = DIRECT_INPUT_HOOKS
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
        DIRECT_INPUT_HOOKS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(this as usize));
    } else {
        unsafe { *object_vtable = patched_vtable };
    }
    remaining
}

pub(crate) unsafe fn create_device(
    _this: *mut c_void,
    class: *const GUID,
    output: *mut *mut c_void,
    _outer: *mut c_void,
    result: HRESULT,
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
            unsafe { hook_device(*output, device_class) };
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

unsafe fn hook_device(interface: *mut c_void, device: DirectInputDevice) {
    if interface.is_null() {
        return;
    }
    let key = interface as usize;
    let mut hooks = DEVICE_HOOKS
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
    hooks.insert(
        key,
        DeviceHook {
            vtable,
            device,
            event_handle: 0,
        },
    );
}

pub(crate) unsafe fn get_device_state(
    this: *mut c_void,
    size: u32,
    output: *mut c_void,
    mut next: crate::hooks::GetDeviceStateContext<'_>,
) -> HRESULT {
    let device = {
        let hooks = DEVICE_HOOKS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get(&(this as usize)) else {
            return 0x8000_4005u32 as i32;
        };
        hook.device
    };
    if !INPUT_CONNECTED.load(Ordering::Acquire) {
        let continuation = *next
            .fp_next()
            .expect("DirectInput hook chains always have a terminal");
        return unsafe { continuation(this, size, output, next) };
    }
    if output.is_null() {
        return 0x8007_0057u32 as i32;
    }
    let mut queues = INPUT_QUEUE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    match device {
        DirectInputDevice::Keyboard => {
            if size as usize != queues.keyboard_state.len() {
                return 0x8007_0057u32 as i32;
            }
            if !LOGGED_KEYBOARD_POLL.swap(true, Ordering::AcqRel) {
                dinput_log("snowglass: target first polled forwarded keyboard state");
            }
            if queues.keyboard_state.iter().any(|state| *state != 0)
                && !LOGGED_KEYBOARD_PRESSED.swap(true, Ordering::AcqRel)
            {
                dinput_log("snowglass: target polled a pressed forwarded keyboard key");
            }
            unsafe {
                std::ptr::copy_nonoverlapping(
                    queues.keyboard_state.as_ptr(),
                    output.cast::<u8>(),
                    queues.keyboard_state.len(),
                );
            }
        }
        DirectInputDevice::Mouse => {
            // DIMOUSESTATE is 16 bytes (4 buttons); DIMOUSESTATE2 is 20 bytes
            // (8 buttons). Both begin with three signed relative axes.
            if size != 16 && size != 20 {
                return 0x8007_0057u32 as i32;
            }
            let output = output.cast::<u8>();
            for (index, axis) in queues.mouse_axes.iter().enumerate() {
                unsafe { std::ptr::write_unaligned(output.add(index * 4).cast::<i32>(), *axis) };
            }
            let button_count = size as usize - 12;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    queues.mouse_buttons.as_ptr(),
                    output.add(12),
                    button_count,
                );
            }
            queues.mouse_axes = [0; 3];
        }
    }
    0
}

pub(crate) unsafe fn device_release(
    this: *mut c_void,
    _next: crate::hooks::DeviceReleaseContext<'_>,
) -> u32 {
    let (original_vtable, patched_vtable, original_release) = {
        let hooks = DEVICE_HOOKS
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
        DEVICE_HOOKS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(this as usize));
    } else {
        unsafe { *object_vtable = patched_vtable };
    }
    remaining
}

pub(crate) unsafe fn set_event_notification(this: *mut c_void, event: *mut c_void) {
    {
        let mut hooks = DEVICE_HOOKS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get_mut(&(this as usize)) else {
            return;
        };
        hook.event_handle = event as usize;
    }
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
        let hooks = DEVICE_HOOKS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get(&(this as usize)) else {
            return 0x8000_4005u32 as i32;
        };
        hook.device
    };
    if !INPUT_CONNECTED.load(Ordering::Acquire) {
        let continuation = *next
            .fp_next()
            .expect("DirectInput hook chains always have a terminal");
        return unsafe { continuation(this, object_size, output, count, flags, next) };
    }
    if count.is_null() {
        return 0x8007_0057u32 as i32;
    }

    let mut queues = INPUT_QUEUE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let queue = queues.get_mut(device);
    let requested = if output.is_null() {
        queue.len()
    } else {
        (unsafe { *count }) as usize
    };
    let available = queue.len().min(requested);
    unsafe { *count = available as u32 };
    if output.is_null() {
        return 0;
    }
    if object_size < 16 {
        return 0x8007_0057u32 as i32;
    }
    let peek = flags & DIGDD_PEEK != 0;
    for index in 0..available {
        let event = if peek {
            queue[index]
        } else {
            queue.pop_front().expect("available count was bounded")
        };
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
    if available != 0 && !LOGGED_DELIVERY.swap(true, Ordering::AcqRel) {
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
) {
    start_input_receiver();
    dinput_log(format!(
        "snowglass: DirectInput8Create version={version:#x} iid={interface_id:p} returned {result:#x}"
    ));
    if result >= 0 && !output.is_null() {
        unsafe { hook_direct_input(*output) };
    }
    let _ = (instance, outer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vtable::{
        DEVICE_VTABLE_LEN, GetDeviceDataFn, GetDeviceStateFn, SetEventNotificationFn,
    };
    use snowglass_ipc::LocalSocketListener;
    use std::time::{Duration, Instant};
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
        start_input_receiver();
        let mut socket = listener.accept().unwrap();

        let mut original_vtable = [0usize; DEVICE_VTABLE_LEN];
        original_vtable[GET_DEVICE_STATE_INDEX] = original_get_state_stub as *const () as usize;
        original_vtable[GET_DEVICE_DATA_INDEX] = original_get_data_stub as *const () as usize;
        original_vtable[SET_EVENT_NOTIFICATION_INDEX] =
            original_set_event_stub as *const () as usize;
        let mut object_vtable = original_vtable.as_ptr();
        let keyboard = (&raw mut object_vtable).cast::<c_void>();
        unsafe { hook_device(keyboard, DirectInputDevice::Keyboard) };
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
        while INPUT_QUEUE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .keyboard
            .is_empty()
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
        INPUT_CONNECTED.store(false, Ordering::Release);
    }
}
