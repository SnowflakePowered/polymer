//! SDL3-only ABI and virtual joystick adapter.

use snowglass_input::gamepad::{
    GAMEPAD_AXIS_COUNT, GAMEPAD_BUTTON_COUNT, GamepadUpdate, MAX_GAMEPADS,
};
use snowglass_input_sdl::{BackendError, SdlBackend, effective_state};
use std::{
    ffi::{c_char, c_void},
    mem,
};
use windows_sys::Win32::{Foundation::HMODULE, System::LibraryLoader::GetProcAddress};

pub(crate) type JoystickId = u32;
pub(crate) type EnumerateFn = unsafe extern "C" fn(*mut i32) -> *mut JoystickId;
pub(crate) type GamepadEnumerateFn = unsafe extern "C" fn(*mut i32) -> *mut JoystickId;
pub(crate) type UpdateFn = unsafe extern "C" fn();
type AttachVirtualFn = unsafe extern "C" fn(*const VirtualJoystickDesc) -> JoystickId;
type OpenFn = unsafe extern "C" fn(JoystickId) -> *mut c_void;
type SetAxisFn = unsafe extern "C" fn(*mut c_void, i32, i16) -> bool;
type SetButtonFn = unsafe extern "C" fn(*mut c_void, i32, bool) -> bool;

const SDL_JOYSTICK_TYPE_GAMEPAD: u16 = 1;
const NAME: &[u8] = b"Snowglass Virtual Gamepad\0";

#[repr(C)]
struct VirtualJoystickDesc {
    version: u32,
    joystick_type: u16,
    padding: u16,
    vendor_id: u16,
    product_id: u16,
    naxes: u16,
    nbuttons: u16,
    nballs: u16,
    nhats: u16,
    ntouchpads: u16,
    nsensors: u16,
    padding2: [u16; 2],
    button_mask: u32,
    axis_mask: u32,
    name: *const c_char,
    touchpads: *const c_void,
    sensors: *const c_void,
    userdata: *mut c_void,
    update: Option<unsafe extern "C" fn(*mut c_void)>,
    set_player_index: Option<unsafe extern "C" fn(*mut c_void, i32)>,
    rumble: Option<unsafe extern "C" fn(*mut c_void, u16, u16) -> bool>,
    rumble_triggers: Option<unsafe extern "C" fn(*mut c_void, u16, u16) -> bool>,
    set_led: Option<unsafe extern "C" fn(*mut c_void, u8, u8, u8) -> bool>,
    send_effect: Option<unsafe extern "C" fn(*mut c_void, *const c_void, i32) -> bool>,
    set_sensors_enabled: Option<unsafe extern "C" fn(*mut c_void, bool) -> bool>,
    cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
}

impl Default for VirtualJoystickDesc {
    fn default() -> Self {
        Self {
            version: std::mem::size_of::<Self>() as u32,
            joystick_type: SDL_JOYSTICK_TYPE_GAMEPAD,
            padding: 0,
            vendor_id: 0,
            product_id: 0,
            naxes: GAMEPAD_AXIS_COUNT as u16,
            nbuttons: GAMEPAD_BUTTON_COUNT as u16,
            nballs: 0,
            nhats: 0,
            ntouchpads: 0,
            nsensors: 0,
            padding2: [0; 2],
            button_mask: (1 << GAMEPAD_BUTTON_COUNT) - 1,
            axis_mask: (1 << GAMEPAD_AXIS_COUNT) - 1,
            name: NAME.as_ptr().cast(),
            touchpads: std::ptr::null(),
            sensors: std::ptr::null(),
            userdata: std::ptr::null_mut(),
            update: None,
            set_player_index: None,
            rumble: None,
            rumble_triggers: None,
            set_led: None,
            send_effect: None,
            set_sensors_enabled: None,
            cleanup: None,
        }
    }
}

pub(crate) struct Api {
    attach_virtual: AttachVirtualFn,
    open: OpenFn,
    set_axis: SetAxisFn,
    set_button: SetButtonFn,
    ids: [JoystickId; MAX_GAMEPADS],
    handles: [usize; MAX_GAMEPADS],
}

impl Api {
    pub(crate) unsafe fn load(module: HMODULE) -> Option<Self> {
        Some(Self {
            attach_virtual: unsafe { symbol(module, c"SDL_AttachVirtualJoystick")? },
            open: unsafe { symbol(module, c"SDL_OpenJoystick")? },
            set_axis: unsafe { symbol(module, c"SDL_SetJoystickVirtualAxis")? },
            set_button: unsafe { symbol(module, c"SDL_SetJoystickVirtualButton")? },
            ids: [0; MAX_GAMEPADS],
            handles: [0; MAX_GAMEPADS],
        })
    }
}

impl SdlBackend for Api {
    fn ensure_attached(&mut self, count: usize) -> Result<(), BackendError> {
        for slot in 0..count {
            if self.handles[slot] != 0 {
                continue;
            }
            let description = VirtualJoystickDesc::default();
            let id = unsafe { (self.attach_virtual)(&raw const description) };
            if id == 0 {
                return Err(BackendError("SDL3 failed to attach a virtual joystick"));
            }
            let handle = unsafe { (self.open)(id) };
            if handle.is_null() {
                return Err(BackendError("SDL3 failed to open its virtual joystick"));
            }
            self.ids[slot] = id;
            self.handles[slot] = handle as usize;
        }
        Ok(())
    }

    fn publish(&mut self, update: GamepadUpdate) -> Result<(), BackendError> {
        let handle = self.handles[update.slot.index()] as *mut c_void;
        if handle.is_null() {
            return Err(BackendError("SDL3 virtual joystick is not attached"));
        }
        let state = effective_state(update);
        for (axis, value) in state.sdl_axes().into_iter().enumerate() {
            if !unsafe { (self.set_axis)(handle, axis as i32, value) } {
                return Err(BackendError("SDL3 rejected a virtual axis update"));
            }
        }
        for button in 0..GAMEPAD_BUTTON_COUNT {
            let pressed = (state.buttons >> button) & 1 != 0;
            if !unsafe { (self.set_button)(handle, button as i32, pressed) } {
                return Err(BackendError("SDL3 rejected a virtual button update"));
            }
        }
        Ok(())
    }
}

pub(crate) unsafe fn symbol<T: Copy>(module: HMODULE, name: &std::ffi::CStr) -> Option<T> {
    let address = unsafe { GetProcAddress(module, name.as_ptr().cast()) }?;
    debug_assert_eq!(mem::size_of::<T>(), mem::size_of_val(&address));
    Some(unsafe { mem::transmute_copy(&address) })
}
