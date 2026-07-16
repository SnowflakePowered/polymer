//! DirectInput proxy loading, device interception, and forwarded input state.
//!
//! The proxy deliberately resolves the real DLL from `System32`; using the
//! ordinary DLL search path here would recursively load this proxy.
#![allow(unsafe_code)]
#![allow(non_snake_case)]

use snowglass_input_core::{DirectInputData, DirectInputDevice};
use snowglass_ipc::{LocalSocket, Message};
use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    fs::OpenOptions,
    io::Write,
    sync::{
        LazyLock, Mutex, Once, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};
use windows_sys::{
    Win32::{
        Devices::HumanInterfaceDevice::{
            DIDEVICEOBJECTDATA, DIGDD_PEEK, GUID_SysKeyboard, GUID_SysMouse,
        },
        Foundation::{HINSTANCE, HMODULE},
        System::{
            LibraryLoader::{GetProcAddress, LoadLibraryExW},
            SystemInformation::GetSystemDirectoryW,
        },
    },
    core::{GUID, HRESULT, PCSTR},
};

pub type DirectInput8CreateFn = unsafe extern "system" fn(
    HINSTANCE,
    u32,
    *const GUID,
    *mut *mut c_void,
    *mut c_void,
) -> HRESULT;
type DllCanUnloadNowFn = unsafe extern "system" fn() -> HRESULT;
type DllGetClassObjectFn =
    unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> HRESULT;
type DllRegistrationFn = unsafe extern "system" fn() -> HRESULT;
type GetdfDIJoystickFn = unsafe extern "system" fn() -> *const c_void;

static SYSTEM_DINPUT8: OnceLock<Result<SystemDinput8, u32>> = OnceLock::new();
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

const DIRECT_INPUT_VTABLE_LEN: usize = 11;
const DEVICE_VTABLE_LEN: usize = 32;
const RELEASE_INDEX: usize = 2;
const CREATE_DEVICE_INDEX: usize = 3;
const ACQUIRE_INDEX: usize = 7;
const GET_DEVICE_STATE_INDEX: usize = 9;
const GET_DEVICE_DATA_INDEX: usize = 10;
const SET_DATA_FORMAT_INDEX: usize = 11;
const SET_EVENT_NOTIFICATION_INDEX: usize = 12;
const SET_COOPERATIVE_LEVEL_INDEX: usize = 13;

type CreateDeviceFn =
    unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void, *mut c_void) -> HRESULT;
type GetDeviceStateFn = unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> HRESULT;
type AcquireFn = unsafe extern "system" fn(*mut c_void) -> HRESULT;
type SetDataFormatFn = unsafe extern "system" fn(*mut c_void, *const c_void) -> HRESULT;
type SetCooperativeLevelFn = unsafe extern "system" fn(*mut c_void, *mut c_void, u32) -> HRESULT;
type GetDeviceDataFn =
    unsafe extern "system" fn(*mut c_void, u32, *mut DIDEVICEOBJECTDATA, *mut u32, u32) -> HRESULT;
type SetEventNotificationFn = unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT;
type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;

struct DirectInputHook {
    _vtable: Box<[usize; DIRECT_INPUT_VTABLE_LEN]>,
    original_vtable: usize,
    original_release: ReleaseFn,
    original_create_device: CreateDeviceFn,
}

// Function addresses and immutable vtable storage remain valid for the process lifetime.
unsafe impl Send for DirectInputHook {}

struct DeviceHook {
    _vtable: Box<[usize; DEVICE_VTABLE_LEN]>,
    original_vtable: usize,
    original_release: ReleaseFn,
    original_acquire: AcquireFn,
    original_get_device_state: GetDeviceStateFn,
    original_get_device_data: GetDeviceDataFn,
    original_set_data_format: SetDataFormatFn,
    original_set_event_notification: SetEventNotificationFn,
    original_set_cooperative_level: SetCooperativeLevelFn,
    device: DirectInputDevice,
    event_handle: usize,
}

unsafe impl Send for DeviceHook {}

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
    let Ok(path) = std::env::var("SNOWGLASS_DINPUT_LOG_FILE") else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{message}");
    }
}

struct SystemDinput8 {
    _module: HMODULE,
    direct_input_8_create: DirectInput8CreateFn,
    dll_can_unload_now: DllCanUnloadNowFn,
    dll_get_class_object: DllGetClassObjectFn,
    dll_register_server: DllRegistrationFn,
    dll_unregister_server: DllRegistrationFn,
    get_df_di_joystick: GetdfDIJoystickFn,
}

// The module remains loaded for the process lifetime and all function pointers
// refer to immutable executable memory in that module.
unsafe impl Send for SystemDinput8 {}
unsafe impl Sync for SystemDinput8 {}

impl SystemDinput8 {
    fn load() -> Result<Self, u32> {
        let mut system_path = vec![0u16; 32_768];
        let length = unsafe {
            GetSystemDirectoryW(
                system_path.as_mut_ptr(),
                system_path.len().try_into().unwrap(),
            )
        } as usize;
        if length == 0 || length >= system_path.len() {
            return Err(unsafe { windows_sys::Win32::Foundation::GetLastError() });
        }
        system_path.truncate(length);
        system_path.extend("\\dinput8.dll\0".encode_utf16());
        // SAFETY: the absolute, NUL-terminated path distinguishes the system
        // module from this already-loaded application-local proxy.
        let module = unsafe { LoadLibraryExW(system_path.as_ptr(), std::ptr::null_mut(), 0) };
        if module.is_null() {
            return Err(unsafe { windows_sys::Win32::Foundation::GetLastError() });
        }
        Ok(Self {
            _module: module,
            direct_input_8_create: unsafe { resolve(module, c"DirectInput8Create")? },
            dll_can_unload_now: unsafe { resolve(module, c"DllCanUnloadNow")? },
            dll_get_class_object: unsafe { resolve(module, c"DllGetClassObject")? },
            dll_register_server: unsafe { resolve(module, c"DllRegisterServer")? },
            dll_unregister_server: unsafe { resolve(module, c"DllUnregisterServer")? },
            get_df_di_joystick: unsafe { resolve(module, c"GetdfDIJoystick")? },
        })
    }
}

fn system_dinput8() -> Result<&'static SystemDinput8, HRESULT> {
    SYSTEM_DINPUT8
        .get_or_init(SystemDinput8::load)
        .as_ref()
        .map_err(|error| HRESULT_FROM_WIN32(*error))
}

pub fn system_direct_input8_create() -> Result<DirectInput8CreateFn, HRESULT> {
    system_dinput8().map(|system| system.direct_input_8_create)
}

fn start_input_receiver() {
    START_INPUT.call_once(|| {
        let Ok(session) = std::env::var("SNOWGLASS_SESSION") else {
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
    let original_vtable = unsafe { *object_vtable };
    let mut vtable = Box::new([0usize; DIRECT_INPUT_VTABLE_LEN]);
    unsafe {
        std::ptr::copy_nonoverlapping(
            original_vtable,
            vtable.as_mut_ptr(),
            DIRECT_INPUT_VTABLE_LEN,
        );
    }
    let original_create_device =
        unsafe { std::mem::transmute::<usize, CreateDeviceFn>(vtable[CREATE_DEVICE_INDEX]) };
    let original_release =
        unsafe { std::mem::transmute::<usize, ReleaseFn>(vtable[RELEASE_INDEX]) };
    vtable[RELEASE_INDEX] = crate::sys::direct_input_release as *const () as usize;
    vtable[CREATE_DEVICE_INDEX] = crate::sys::create_device as *const () as usize;
    unsafe { *object_vtable = vtable.as_ptr() };
    hooks.insert(
        key,
        DirectInputHook {
            _vtable: vtable,
            original_vtable: original_vtable as usize,
            original_release,
            original_create_device,
        },
    );
}

pub(crate) unsafe fn direct_input_release(this: *mut c_void) -> u32 {
    let (original_vtable, patched_vtable, original_release) = {
        let hooks = DIRECT_INPUT_HOOKS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get(&(this as usize)) else {
            return 0;
        };
        (
            hook.original_vtable as *const usize,
            hook._vtable.as_ptr(),
            hook.original_release,
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
    this: *mut c_void,
    class: *const GUID,
    output: *mut *mut c_void,
    outer: *mut c_void,
) -> HRESULT {
    if !class.is_null() {
        let class = unsafe { &*class };
        dinput_log(format!(
            "snowglass: DirectInput CreateDevice {:08x}-{:04x}-{:04x}",
            class.data1, class.data2, class.data3
        ));
    }
    let (original_vtable, patched_vtable, original) = {
        let hooks = DIRECT_INPUT_HOOKS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get(&(this as usize)) else {
            return 0x8000_4005u32 as i32;
        };
        (
            hook.original_vtable as *const usize,
            hook._vtable.as_ptr(),
            hook.original_create_device,
        )
    };
    let object_vtable = this.cast::<*const usize>();
    unsafe { *object_vtable = original_vtable };
    let result = unsafe { original(this, class, output, outer) };
    unsafe { *object_vtable = patched_vtable };
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
    result
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
    let original_vtable = unsafe { *object_vtable };
    let mut vtable = Box::new([0usize; DEVICE_VTABLE_LEN]);
    unsafe {
        std::ptr::copy_nonoverlapping(original_vtable, vtable.as_mut_ptr(), DEVICE_VTABLE_LEN);
    }
    let original_get_device_data =
        unsafe { std::mem::transmute::<usize, GetDeviceDataFn>(vtable[GET_DEVICE_DATA_INDEX]) };
    let original_acquire =
        unsafe { std::mem::transmute::<usize, AcquireFn>(vtable[ACQUIRE_INDEX]) };
    let original_get_device_state =
        unsafe { std::mem::transmute::<usize, GetDeviceStateFn>(vtable[GET_DEVICE_STATE_INDEX]) };
    let original_release =
        unsafe { std::mem::transmute::<usize, ReleaseFn>(vtable[RELEASE_INDEX]) };
    let original_set_event_notification = unsafe {
        std::mem::transmute::<usize, SetEventNotificationFn>(vtable[SET_EVENT_NOTIFICATION_INDEX])
    };
    let original_set_data_format =
        unsafe { std::mem::transmute::<usize, SetDataFormatFn>(vtable[SET_DATA_FORMAT_INDEX]) };
    let original_set_cooperative_level = unsafe {
        std::mem::transmute::<usize, SetCooperativeLevelFn>(vtable[SET_COOPERATIVE_LEVEL_INDEX])
    };
    vtable[RELEASE_INDEX] = crate::sys::device_release as *const () as usize;
    vtable[ACQUIRE_INDEX] = crate::sys::acquire as *const () as usize;
    vtable[GET_DEVICE_STATE_INDEX] = crate::sys::get_device_state as *const () as usize;
    vtable[GET_DEVICE_DATA_INDEX] = crate::sys::get_device_data as *const () as usize;
    vtable[SET_DATA_FORMAT_INDEX] = crate::sys::set_data_format as *const () as usize;
    vtable[SET_EVENT_NOTIFICATION_INDEX] = crate::sys::set_event_notification as *const () as usize;
    vtable[SET_COOPERATIVE_LEVEL_INDEX] = crate::sys::set_cooperative_level as *const () as usize;
    unsafe { *object_vtable = vtable.as_ptr() };
    hooks.insert(
        key,
        DeviceHook {
            _vtable: vtable,
            original_vtable: original_vtable as usize,
            original_release,
            original_acquire,
            original_get_device_state,
            original_get_device_data,
            original_set_data_format,
            original_set_event_notification,
            original_set_cooperative_level,
            device,
            event_handle: 0,
        },
    );
}

pub(crate) unsafe fn acquire(this: *mut c_void) -> HRESULT {
    let (original_vtable, patched_vtable, original) = {
        let hooks = DEVICE_HOOKS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get(&(this as usize)) else {
            return 0x8000_4005u32 as i32;
        };
        (
            hook.original_vtable as *const usize,
            hook._vtable.as_ptr(),
            hook.original_acquire,
        )
    };
    let slot = this.cast::<*const usize>();
    unsafe { *slot = original_vtable };
    let result = unsafe { original(this) };
    unsafe { *slot = patched_vtable };
    result
}

pub(crate) unsafe fn set_data_format(this: *mut c_void, format: *const c_void) -> HRESULT {
    let (original_vtable, patched_vtable, original) = {
        let hooks = DEVICE_HOOKS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get(&(this as usize)) else {
            return 0x8000_4005u32 as i32;
        };
        (
            hook.original_vtable as *const usize,
            hook._vtable.as_ptr(),
            hook.original_set_data_format,
        )
    };
    let slot = this.cast::<*const usize>();
    unsafe { *slot = original_vtable };
    let result = unsafe { original(this, format) };
    unsafe { *slot = patched_vtable };
    result
}

pub(crate) unsafe fn set_cooperative_level(
    this: *mut c_void,
    window: *mut c_void,
    flags: u32,
) -> HRESULT {
    let (original_vtable, patched_vtable, original) = {
        let hooks = DEVICE_HOOKS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get(&(this as usize)) else {
            return 0x8000_4005u32 as i32;
        };
        (
            hook.original_vtable as *const usize,
            hook._vtable.as_ptr(),
            hook.original_set_cooperative_level,
        )
    };
    let slot = this.cast::<*const usize>();
    unsafe { *slot = original_vtable };
    let result = unsafe { original(this, window, flags) };
    unsafe { *slot = patched_vtable };
    result
}

pub(crate) unsafe fn get_device_state(
    this: *mut c_void,
    size: u32,
    output: *mut c_void,
) -> HRESULT {
    let (original, device) = {
        let hooks = DEVICE_HOOKS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get(&(this as usize)) else {
            return 0x8000_4005u32 as i32;
        };
        (hook.original_get_device_state, hook.device)
    };
    if !INPUT_CONNECTED.load(Ordering::Acquire) {
        return unsafe { original(this, size, output) };
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

pub(crate) unsafe fn device_release(this: *mut c_void) -> u32 {
    let (original_vtable, patched_vtable, original_release) = {
        let hooks = DEVICE_HOOKS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get(&(this as usize)) else {
            return 0;
        };
        (
            hook.original_vtable as *const usize,
            hook._vtable.as_ptr(),
            hook.original_release,
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

pub(crate) unsafe fn set_event_notification(this: *mut c_void, event: *mut c_void) -> HRESULT {
    let original = {
        let mut hooks = DEVICE_HOOKS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get_mut(&(this as usize)) else {
            return 0x8000_4005u32 as i32;
        };
        hook.event_handle = event as usize;
        hook.original_set_event_notification
    };
    unsafe { original(this, event) }
}

pub(crate) unsafe fn get_device_data(
    this: *mut c_void,
    object_size: u32,
    output: *mut DIDEVICEOBJECTDATA,
    count: *mut u32,
    flags: u32,
) -> HRESULT {
    let (original, device) = {
        let hooks = DEVICE_HOOKS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(hook) = hooks.get(&(this as usize)) else {
            return 0x8000_4005u32 as i32;
        };
        (hook.original_get_device_data, hook.device)
    };
    if !INPUT_CONNECTED.load(Ordering::Acquire) {
        return unsafe { original(this, object_size, output, count, flags) };
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

unsafe fn resolve<T: Copy>(module: HMODULE, name: &'static std::ffi::CStr) -> Result<T, u32> {
    let address = unsafe { GetProcAddress(module, name.as_ptr().cast::<u8>() as PCSTR) };
    let Some(address) = address else {
        return Err(unsafe { windows_sys::Win32::Foundation::GetLastError() });
    };
    // SAFETY: every caller supplies the ABI signature of the named dinput8
    // export, verified against the Windows SDK declarations.
    Ok(unsafe { std::mem::transmute_copy(&address) })
}

#[allow(non_snake_case)]
const fn HRESULT_FROM_WIN32(error: u32) -> HRESULT {
    if error == 0 {
        0
    } else {
        ((error & 0x0000_ffff) | 0x8007_0000) as i32
    }
}

pub unsafe extern "system" fn DirectInput8Create(
    instance: HINSTANCE,
    version: u32,
    interface_id: *const GUID,
    output: *mut *mut c_void,
    outer: *mut c_void,
) -> HRESULT {
    let system = match system_dinput8() {
        Ok(system) => system,
        Err(error) => return error,
    };
    unsafe {
        intercept_direct_input8_create(
            system.direct_input_8_create,
            instance,
            version,
            interface_id,
            output,
            outer,
        )
    }
}

/// Shared interception entry used by both the legacy proxy and an installed
/// in-process detour. `original` must be the unhooked function or trampoline.
pub unsafe fn intercept_direct_input8_create(
    original: DirectInput8CreateFn,
    instance: HINSTANCE,
    version: u32,
    interface_id: *const GUID,
    output: *mut *mut c_void,
    outer: *mut c_void,
) -> HRESULT {
    start_input_receiver();
    dinput_log(format!(
        "snowglass: forwarding DirectInput8Create version={version:#x} iid={interface_id:p}"
    ));
    let result = unsafe { original(instance, version, interface_id, output, outer) };
    dinput_log(format!(
        "snowglass: DirectInput8Create returned {result:#x}"
    ));
    if result >= 0 && !output.is_null() {
        unsafe { hook_direct_input(*output) };
    }
    result
}

pub unsafe extern "system" fn DllCanUnloadNow() -> HRESULT {
    match system_dinput8() {
        Ok(system) => unsafe { (system.dll_can_unload_now)() },
        Err(error) => error,
    }
}

pub unsafe extern "system" fn DllGetClassObject(
    class_id: *const GUID,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    match system_dinput8() {
        Ok(system) => unsafe { (system.dll_get_class_object)(class_id, interface_id, output) },
        Err(error) => error,
    }
}

pub unsafe extern "system" fn DllRegisterServer() -> HRESULT {
    match system_dinput8() {
        Ok(system) => unsafe { (system.dll_register_server)() },
        Err(error) => error,
    }
}

pub unsafe extern "system" fn DllUnregisterServer() -> HRESULT {
    match system_dinput8() {
        Ok(system) => unsafe { (system.dll_unregister_server)() },
        Err(error) => error,
    }
}

pub unsafe extern "system" fn GetdfDIJoystick() -> *const c_void {
    match system_dinput8() {
        Ok(system) => unsafe { (system.get_df_di_joystick)() },
        Err(_) => std::ptr::null(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snowglass_ipc::LocalSocketListener;
    use std::time::{Duration, Instant};

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
    fn resolves_every_system_export() {
        let system = system_dinput8().expect("system dinput8.dll should load");
        assert!(!system._module.is_null());
    }

    #[test]
    fn forwarded_record_is_returned_by_device_get_data() {
        let session = format!("snowglass-dinput-test-{}", std::process::id());
        let listener = LocalSocketListener::bind(&format!("{session}-input")).unwrap();
        unsafe { std::env::set_var("SNOWGLASS_SESSION", &session) };
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
