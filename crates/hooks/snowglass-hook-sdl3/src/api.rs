//! SDL3-only ABI and virtual joystick adapter.

use consolespec::input::{Alignment, AnalogClass};
use snowglass_hook_input_sdl::Runtime;
use snowglass_hook_input_sdl::{BackendError, SdlBackend};
use snowglass_hook_input_sdl::{
    ConnectionState, GAMEPAD_AXIS_COUNT, GAMEPAD_BUTTON_COUNT, GamepadFeedback, GamepadFrame,
    GamepadSlot,
};
use snowglass_input_layout::{layout::OutputLayout, model::Sensor};
use std::{
    ffi::{CString, c_char, c_void},
    mem,
    sync::Arc,
};
use windows_sys::Win32::{Foundation::HMODULE, System::LibraryLoader::GetProcAddress};

pub(crate) type JoystickId = u32;
pub(crate) type DynapiEntryFn = unsafe extern "C" fn(u32, *mut c_void, u32) -> i32;
type AttachVirtualFn = unsafe extern "C" fn(*const VirtualJoystickDesc) -> JoystickId;
type DetachVirtualFn = unsafe extern "C" fn(JoystickId) -> bool;
type OpenFn = unsafe extern "C" fn(JoystickId) -> *mut c_void;
type SetAxisFn = unsafe extern "C" fn(*mut c_void, i32, i16) -> bool;
type SetButtonFn = unsafe extern "C" fn(*mut c_void, i32, bool) -> bool;
type SendSensorFn = unsafe extern "C" fn(*mut c_void, i32, u64, *const f32, i32) -> bool;
type UpdateJoysticksFn = unsafe extern "C" fn();
type MallocFn = unsafe extern "C" fn(usize) -> *mut c_void;

const SDL_JOYSTICK_TYPE_GAMEPAD: u16 = 1;
// SDL_SensorType. The `_L`/`_R` variants exist for devices carrying two of one
// sensor: a pair of Joy-Cons, or a Wii Remote with a Nunchuk attached.
const SDL_SENSOR_ACCEL: i32 = 1;
const SDL_SENSOR_GYRO: i32 = 2;
const SDL_SENSOR_ACCEL_L: i32 = 3;
const SDL_SENSOR_GYRO_L: i32 = 4;
const SDL_SENSOR_ACCEL_R: i32 = 5;
const SDL_SENSOR_GYRO_R: i32 = 6;

/// Snowglass names a sensor by where it sits; SDL names it by number.
fn sdl_sensor_type(sensor: Sensor) -> i32 {
    match (sensor.class, sensor.alignment) {
        (AnalogClass::Accelerometer, None) => SDL_SENSOR_ACCEL,
        (AnalogClass::Gyroscope, None) => SDL_SENSOR_GYRO,
        (AnalogClass::Accelerometer, Some(Alignment::Left)) => SDL_SENSOR_ACCEL_L,
        (AnalogClass::Gyroscope, Some(Alignment::Left)) => SDL_SENSOR_GYRO_L,
        (AnalogClass::Accelerometer, Some(Alignment::Right)) => SDL_SENSOR_ACCEL_R,
        (AnalogClass::Gyroscope, Some(Alignment::Right)) => SDL_SENSOR_GYRO_R,
        _ => unreachable!("output layouts contain only SDL-supported sensors"),
    }
}
const SDL_GAMEPAD_AXIS_COUNT: usize = 6;

fn dense_mask_index(mask: u32, semantic_index: usize) -> Option<i32> {
    let bit = 1u32.checked_shl(semantic_index as u32)?;
    (mask & bit != 0).then(|| (mask & (bit - 1)).count_ones() as i32)
}

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
        layout: OutputLayout,
        name: *const c_char,
        sensors: &[VirtualJoystickSensorDesc],
        userdata: *mut c_void,
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
            userdata,
            update: None,
            set_player_index: None,
            rumble: Some(FeedbackContext::rumble),
            rumble_triggers: None,
            set_led: None,
            send_effect: None,
            set_sensors_enabled: None,
            cleanup: None,
        }
    }
}

pub(crate) struct Api {
    runtime: Arc<Runtime>,
    attach_virtual: AttachVirtualFn,
    detach_virtual: DetachVirtualFn,
    open: OpenFn,
    set_axis: SetAxisFn,
    set_button: SetButtonFn,
    send_sensor: SendSensorFn,
    update_joysticks: UpdateJoysticksFn,
    ids: Vec<JoystickId>,
    handles: Vec<usize>,
    layouts: Vec<OutputLayout>,
    names: Vec<CString>,
    sensors: Vec<Box<[VirtualJoystickSensorDesc]>>,
    malloc: MallocFn,
    feedback: Vec<FeedbackContext>,
    connected: Vec<bool>,
}

struct FeedbackContext {
    runtime: Arc<Runtime>,
    slot: GamepadSlot,
}

impl FeedbackContext {
    unsafe extern "C" fn rumble(userdata: *mut c_void, low: u16, high: u16) -> bool {
        let Some(context) = (unsafe { userdata.cast::<Self>().as_ref() }) else {
            return false;
        };
        context.runtime.send_feedback(GamepadFeedback {
            slot: context.slot,
            low_frequency: low,
            high_frequency: high,
            duration_ms: 0,
        })
    }
}

impl Api {
    pub(crate) unsafe fn load(
        module: HMODULE,
        layouts: Vec<(OutputLayout, String)>,
        runtime: Arc<Runtime>,
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
                layout
                    .sensors()
                    .map(|sensor| VirtualJoystickSensorDesc {
                        sensor_type: sdl_sensor_type(sensor),
                        rate: 0.0,
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            })
            .collect();
        let feedback = (0..layouts.len())
            .map(|slot| FeedbackContext {
                runtime: runtime.clone(),
                slot: GamepadSlot::new(slot as u8).expect("virtual gamepad count is bounded to u8"),
            })
            .collect();
        let connected = vec![true; layouts.len()];
        Some(Self {
            runtime,
            attach_virtual: unsafe { symbol(module, c"SDL_AttachVirtualJoystick")? },
            detach_virtual: unsafe { symbol(module, c"SDL_DetachVirtualJoystick")? },
            open: unsafe { symbol(module, c"SDL_OpenJoystick")? },
            set_axis: unsafe { symbol(module, c"SDL_SetJoystickVirtualAxis")? },
            set_button: unsafe { symbol(module, c"SDL_SetJoystickVirtualButton")? },
            send_sensor: unsafe { symbol(module, c"SDL_SendJoystickVirtualSensorData")? },
            update_joysticks: unsafe { symbol(module, c"SDL_UpdateJoysticks")? },
            ids: vec![0; layouts.len()],
            handles: vec![0; layouts.len()],
            layouts,
            names,
            sensors,
            malloc: unsafe { symbol(module, c"SDL_malloc")? },
            feedback,
            connected,
        })
    }

    /// SDL owns and frees enumerator results, so return an SDL-allocated,
    /// zero-terminated ID list containing only Snowglass virtual devices.
    pub(crate) unsafe fn enumerate(&self, count: *mut i32) -> *mut JoystickId {
        let ids = self
            .ids
            .iter()
            .copied()
            .zip(&self.connected)
            .filter_map(|(id, connected)| (*connected && id != 0).then_some(id))
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
    fn rebind(&mut self, layouts: &[OutputLayout]) -> Result<(), BackendError> {
        for id in self.ids.drain(..).filter(|id| *id != 0) {
            unsafe { (self.detach_virtual)(id) };
        }
        self.layouts = layouts.to_vec();
        self.names = layouts
            .iter()
            .map(|_| CString::new("Snowglass Virtual Gamepad").expect("static name is valid"))
            .collect();
        self.sensors = layouts
            .iter()
            .map(|layout| {
                layout
                    .sensors()
                    .map(|sensor| VirtualJoystickSensorDesc {
                        sensor_type: sdl_sensor_type(sensor),
                        rate: 0.0,
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            })
            .collect();
        self.feedback = (0..layouts.len())
            .map(|slot| FeedbackContext {
                runtime: self.runtime.clone(),
                slot: GamepadSlot::new(slot as u8).expect("topology is bounded to eight players"),
            })
            .collect();
        self.ids = vec![0; layouts.len()];
        self.handles = vec![0; layouts.len()];
        self.connected = vec![false; layouts.len()];
        Ok(())
    }

    fn ensure_attached(&mut self, count: usize) -> Result<(), BackendError> {
        for slot in 0..count {
            if self.handles[slot] != 0 {
                continue;
            }
            let description = VirtualJoystickDesc::new(
                self.layouts[slot],
                self.names[slot].as_ptr(),
                &self.sensors[slot],
                (&mut self.feedback[slot] as *mut FeedbackContext).cast(),
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

    fn set_connected(&mut self, connection: ConnectionState) -> Result<(), BackendError> {
        let Some(connected) = self.connected.get_mut(connection.slot.index()) else {
            return Err(BackendError("SDL3 virtual gamepad slot is out of range"));
        };
        if *connected != connection.connected {
            *connected = connection.connected;
            unsafe { (self.update_joysticks)() };
        }
        Ok(())
    }

    fn publish(&mut self, frame: GamepadFrame) -> Result<(), BackendError> {
        let handle = self.handles[frame.slot.index()] as *mut c_void;
        if handle.is_null() {
            return Err(BackendError("SDL3 virtual joystick is not attached"));
        }
        let state = if frame.connected {
            frame.state
        } else {
            Default::default()
        };
        let layout = self.layouts[frame.slot.index()];
        let axes = state.sdl_axes();
        for (semantic_axis, value) in axes[..SDL_GAMEPAD_AXIS_COUNT].iter().copied().enumerate() {
            let Some(raw_axis) = dense_mask_index(layout.axis_mask, semantic_axis) else {
                continue;
            };
            if !unsafe { (self.set_axis)(handle, raw_axis, value) } {
                return Err(BackendError("SDL3 rejected a virtual axis update"));
            }
        }
        // The two Snowglass pointer axes are deliberately raw joystick axes,
        // outside SDL's six standard gamepad axes, so they retain indices 6/7.
        for (raw_axis, value) in axes[SDL_GAMEPAD_AXIS_COUNT..].iter().copied().enumerate() {
            if !unsafe {
                (self.set_axis)(handle, (SDL_GAMEPAD_AXIS_COUNT + raw_axis) as i32, value)
            } {
                return Err(BackendError("SDL3 rejected a virtual axis update"));
            }
        }
        for semantic_button in 0..GAMEPAD_BUTTON_COUNT {
            let Some(raw_button) = dense_mask_index(layout.button_mask, semantic_button) else {
                continue;
            };
            let pressed = (state.buttons >> semantic_button) & 1 != 0;
            if !unsafe { (self.set_button)(handle, raw_button, pressed) } {
                return Err(BackendError("SDL3 rejected a virtual button update"));
            }
        }
        for sensor in layout.sensors() {
            // The wire carries one accelerometer and one gyroscope; a device
            // with a left and a right instance drives both from the same
            // sample until the transport grows a second set.
            let values = match sensor {
                Sensor {
                    class: AnalogClass::Accelerometer,
                    ..
                } => state.accelerometer,
                Sensor {
                    class: AnalogClass::Gyroscope,
                    ..
                } => state.gyroscope,
                _ => unreachable!("output layouts contain only motion sensors"),
            };
            if !unsafe {
                (self.send_sensor)(
                    handle,
                    sdl_sensor_type(sensor),
                    0,
                    values.as_ptr(),
                    values.len() as i32,
                )
            } {
                return Err(BackendError("SDL3 rejected a virtual sensor update"));
            }
        }
        // SDL virtual setters stage their values. Commit them before the
        // consumer's current getter call continues.
        unsafe { (self.update_joysticks)() };
        Ok(())
    }
}

pub(crate) unsafe fn symbol<T: Copy>(module: HMODULE, name: &std::ffi::CStr) -> Option<T> {
    let address = unsafe { GetProcAddress(module, name.as_ptr().cast()) }?;
    debug_assert_eq!(mem::size_of::<T>(), mem::size_of_val(&address));
    Some(unsafe { mem::transmute_copy(&address) })
}

#[cfg(test)]
mod tests {
    use super::dense_mask_index;

    #[test]
    fn sparse_gamepad_masks_use_dense_virtual_button_indices() {
        let wii_u_drc_mask =
            (0..=6).fold(0u32, |mask, button| mask | (1 << button)) | (0b11_1111 << 9);

        assert_eq!(dense_mask_index(wii_u_drc_mask, 0), Some(0));
        assert_eq!(dense_mask_index(wii_u_drc_mask, 6), Some(6));
        assert_eq!(dense_mask_index(wii_u_drc_mask, 7), None);
        assert_eq!(dense_mask_index(wii_u_drc_mask, 12), Some(10));
        assert_eq!(dense_mask_index(wii_u_drc_mask, 14), Some(12));
    }

    #[test]
    fn sparse_standard_axis_masks_are_dense_but_pointer_axes_are_not() {
        let axis_mask = (1 << 0) | (1 << 2) | (1 << 5);

        assert_eq!(dense_mask_index(axis_mask, 0), Some(0));
        assert_eq!(dense_mask_index(axis_mask, 2), Some(1));
        assert_eq!(dense_mask_index(axis_mask, 5), Some(2));
    }
}
