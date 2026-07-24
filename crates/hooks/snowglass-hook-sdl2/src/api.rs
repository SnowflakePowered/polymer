//! SDL2-only ABI and virtual joystick adapter.

use snowglass_input::gamepad::{GAMEPAD_AXIS_COUNT, GAMEPAD_BUTTON_COUNT, GamepadUpdate};
use snowglass_input_sdl::{BackendError, SdlBackend, effective_state};
use std::{ffi::c_void, mem};
use windows_sys::Win32::{Foundation::HMODULE, System::LibraryLoader::GetProcAddress};

pub(crate) type DynapiEntryFn = unsafe extern "C" fn(u32, *mut c_void, u32) -> i32;
type AttachVirtualFn = unsafe extern "C" fn(i32, i32, i32, i32) -> i32;
type OpenFn = unsafe extern "C" fn(i32) -> *mut c_void;
type SetAxisFn = unsafe extern "C" fn(*mut c_void, i32, i16) -> i32;
type SetButtonFn = unsafe extern "C" fn(*mut c_void, i32, u8) -> i32;

const SDL_JOYSTICK_TYPE_GAMECONTROLLER: i32 = 1;

pub(crate) struct Api {
    attach_virtual: AttachVirtualFn,
    open: OpenFn,
    set_axis: SetAxisFn,
    set_button: SetButtonFn,
    handles: Vec<usize>,
}

impl Api {
    pub(crate) unsafe fn load(module: HMODULE) -> Option<Self> {
        Some(Self {
            attach_virtual: unsafe { symbol(module, c"SDL_JoystickAttachVirtual")? },
            open: unsafe { symbol(module, c"SDL_JoystickOpen")? },
            set_axis: unsafe { symbol(module, c"SDL_JoystickSetVirtualAxis")? },
            set_button: unsafe { symbol(module, c"SDL_JoystickSetVirtualButton")? },
            handles: Vec::new(),
        })
    }
}

impl SdlBackend for Api {
    fn ensure_attached(&mut self, count: usize) -> Result<(), BackendError> {
        self.handles.resize(count, 0);
        for slot in 0..count {
            if self.handles[slot] != 0 {
                continue;
            }
            let index = unsafe {
                (self.attach_virtual)(
                    SDL_JOYSTICK_TYPE_GAMECONTROLLER,
                    GAMEPAD_AXIS_COUNT as i32,
                    GAMEPAD_BUTTON_COUNT as i32,
                    0,
                )
            };
            if index < 0 {
                return Err(BackendError("SDL2 failed to attach a virtual joystick"));
            }
            let handle = unsafe { (self.open)(index) };
            if handle.is_null() {
                return Err(BackendError("SDL2 failed to open its virtual joystick"));
            }
            self.handles[slot] = handle as usize;
        }
        Ok(())
    }

    fn publish(&mut self, update: GamepadUpdate) -> Result<(), BackendError> {
        let handle = self.handles[update.slot.index()] as *mut c_void;
        if handle.is_null() {
            return Err(BackendError("SDL2 virtual joystick is not attached"));
        }
        let state = effective_state(update);
        for (axis, value) in state.sdl_axes().into_iter().enumerate() {
            if unsafe { (self.set_axis)(handle, axis as i32, value) } < 0 {
                return Err(BackendError("SDL2 rejected a virtual axis update"));
            }
        }
        for button in 0..GAMEPAD_BUTTON_COUNT {
            let pressed = ((state.buttons >> button) & 1) as u8;
            if unsafe { (self.set_button)(handle, button as i32, pressed) } < 0 {
                return Err(BackendError("SDL2 rejected a virtual button update"));
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
