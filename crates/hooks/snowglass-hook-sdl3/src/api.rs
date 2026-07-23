//! SDL3-only ABI and virtual joystick adapter.

use snowglass_input::gamepad::{GAMEPAD_AXIS_COUNT, GAMEPAD_BUTTON_COUNT, GamepadUpdate};
use snowglass_input::stone::SdlOutputLayout;
use snowglass_input_sdl::{BackendError, SdlBackend, effective_state};
use std::{
    ffi::{CString, c_char, c_void},
    mem,
};
use windows_sys::Win32::{Foundation::HMODULE, System::LibraryLoader::GetProcAddress};

pub(crate) type JoystickId = u32;
pub(crate) type DynapiEntryFn = unsafe extern "C" fn(u32, *mut c_void, u32) -> i32;
type AttachVirtualFn = unsafe extern "C" fn(*const VirtualJoystickDesc) -> JoystickId;
type OpenFn = unsafe extern "C" fn(JoystickId) -> *mut c_void;
type SetAxisFn = unsafe extern "C" fn(*mut c_void, i32, i16) -> bool;
type SetButtonFn = unsafe extern "C" fn(*mut c_void, i32, bool) -> bool;
type SendSensorFn = unsafe extern "C" fn(*mut c_void, i32, u64, *const f32, i32) -> bool;
type MallocFn = unsafe extern "C" fn(usize) -> *mut c_void;

const SDL_JOYSTICK_TYPE_GAMEPAD: u16 = 1;
const SDL_SENSOR_ACCEL: i32 = 1;
const SDL_SENSOR_GYRO: i32 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtualJoystickSensorDesc {
    sensor_type: i32,
    rate: f32,
}
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

impl VirtualJoystickDesc {
    fn new(
        layout: SdlOutputLayout,
        name: *const c_char,
        sensors: &[VirtualJoystickSensorDesc],
    ) -> Self {
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
            nsensors: sensors.len() as u16,
            padding2: [0; 2],
            button_mask: layout.button_mask,
            axis_mask: layout.axis_mask,
            name,
            touchpads: std::ptr::null(),
            sensors: sensors.as_ptr().cast(),
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
    send_sensor: SendSensorFn,
    ids: Vec<JoystickId>,
    handles: Vec<usize>,
    layouts: Vec<SdlOutputLayout>,
    names: Vec<CString>,
    sensors: Vec<Box<[VirtualJoystickSensorDesc]>>,
    malloc: MallocFn,
}

impl Api {
    pub(crate) unsafe fn load(
        module: HMODULE,
        layouts: Vec<(SdlOutputLayout, String)>,
    ) -> Option<Self> {
        let (layouts, names): (Vec<_>, Vec<_>) = layouts
            .into_iter()
            .map(|(layout, name)| Some((layout, CString::new(name).ok()?)))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .unzip();
        let sensors = layouts
            .iter()
            .map(|layout| {
                let mut sensors = Vec::with_capacity(2);
                if layout.accelerometer {
                    sensors.push(VirtualJoystickSensorDesc {
                        sensor_type: SDL_SENSOR_ACCEL,
                        rate: 0.0,
                    });
                }
                if layout.gyroscope {
                    sensors.push(VirtualJoystickSensorDesc {
                        sensor_type: SDL_SENSOR_GYRO,
                        rate: 0.0,
                    });
                }
                sensors.into_boxed_slice()
            })
            .collect();
        Some(Self {
            attach_virtual: unsafe { symbol(module, c"SDL_AttachVirtualJoystick")? },
            open: unsafe { symbol(module, c"SDL_OpenJoystick")? },
            set_axis: unsafe { symbol(module, c"SDL_SetJoystickVirtualAxis")? },
            set_button: unsafe { symbol(module, c"SDL_SetJoystickVirtualButton")? },
            send_sensor: unsafe { symbol(module, c"SDL_SendJoystickVirtualSensorData")? },
            ids: vec![0; layouts.len()],
            handles: vec![0; layouts.len()],
            layouts,
            names,
            sensors,
            malloc: unsafe { symbol(module, c"SDL_malloc")? },
        })
    }

    /// SDL owns and frees enumerator results, so return an SDL-allocated,
    /// zero-terminated ID list containing only Snowglass virtual devices.
    pub(crate) unsafe fn enumerate(&self, count: *mut i32) -> *mut JoystickId {
        let ids = self
            .ids
            .iter()
            .copied()
            .filter(|id| *id != 0)
            .collect::<Vec<_>>();
        if !count.is_null() {
            unsafe { *count = ids.len() as i32 };
        }
        let output = unsafe { (self.malloc)((ids.len() + 1) * std::mem::size_of::<JoystickId>()) }
            .cast::<JoystickId>();
        if output.is_null() {
            if !count.is_null() {
                unsafe { *count = 0 }
            }
            return std::ptr::null_mut();
        }
        unsafe {
            std::ptr::copy_nonoverlapping(ids.as_ptr(), output, ids.len());
            *output.add(ids.len()) = 0;
        }
        output
    }
}

impl SdlBackend for Api {
    fn ensure_attached(&mut self, count: usize) -> Result<(), BackendError> {
        for slot in 0..count {
            if self.handles[slot] != 0 {
                continue;
            }
            let description = VirtualJoystickDesc::new(
                self.layouts[slot],
                self.names[slot].as_ptr(),
                &self.sensors[slot],
            );
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
        let layout = self.layouts[update.slot.index()];
        for (axis, value) in state.sdl_axes().into_iter().enumerate() {
            if layout.axis_mask & (1 << axis) == 0 {
                continue;
            }
            if !unsafe { (self.set_axis)(handle, axis as i32, value) } {
                return Err(BackendError("SDL3 rejected a virtual axis update"));
            }
        }
        for button in 0..GAMEPAD_BUTTON_COUNT {
            if layout.button_mask & (1 << button) == 0 {
                continue;
            }
            let pressed = (state.buttons >> button) & 1 != 0;
            if !unsafe { (self.set_button)(handle, button as i32, pressed) } {
                return Err(BackendError("SDL3 rejected a virtual button update"));
            }
        }
        for (enabled, sensor_type, values) in [
            (layout.accelerometer, SDL_SENSOR_ACCEL, state.accelerometer),
            (layout.gyroscope, SDL_SENSOR_GYRO, state.gyroscope),
        ] {
            if enabled
                && !unsafe {
                    (self.send_sensor)(handle, sensor_type, 0, values.as_ptr(), values.len() as i32)
                }
            {
                return Err(BackendError("SDL3 rejected a virtual sensor update"));
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
