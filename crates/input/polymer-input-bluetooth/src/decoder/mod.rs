use polymer_bluetooth::manager::{COMMAND_RESPONSE_TIMEOUT, SetPlayerIndex, SetRumble};
use polymer_bluetooth::{BluetoothManager, ConnectedController, SessionId, devices::ProfileId};
use polymer_input::{
    BackendError, Calibration, Capabilities, DeviceDescriptor, DeviceId, DeviceRole, DeviceState,
    Feedback, GamepadButton, RawHidFilter,
};
use std::{sync::Arc, time::Instant};

pub mod dualshock3;
pub mod switch;
pub mod switch2;
pub mod wiimote;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GamepadState {
    pub buttons: u32,
    pub pressure: polymer_input::ButtonPressure,
    pub left_stick: [i16; 2],
    pub right_stick: [i16; 2],
    pub left_trigger: u16,
    pub right_trigger: u16,
    pub pointer: [i16; 2],
    pub accelerometer: [f32; 3],
    pub gyroscope: [f32; 3],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerSource {
    IrCamera,
    Optical,
    AirMouse,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimedMotionVector {
    pub value: [f32; 3],
    pub sampled_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionFrame {
    /// Unit quaternion in `w, x, y, z` order.
    pub orientation: [f32; 4],
    pub orientation_sampled_at: Instant,
    /// Raw accelerometer history. Precision keyframes retain the most recent
    /// history because they carry a filtered vector instead of new ADC samples.
    pub accelerometer: [TimedMotionVector; 3],
    /// Raw gyroscope history, retained across precision keyframes.
    pub gyroscope: [TimedMotionVector; 2],
    /// Filtered Q16.16 acceleration carried by a precision keyframe.
    pub filtered_accelerometer: Option<TimedMotionVector>,
    /// Whether `accelerometer` and `gyroscope` were updated by this packet.
    pub raw_samples_updated: bool,
    pub temperature_celsius: f32,
    pub sensor_timestamp: u16,
    pub elapsed_ticks: u16,
}

#[derive(Clone, Copy, Debug)]
pub struct DecodedInput {
    pub state: GamepadState,
    pub role: DeviceRole,
    pub battery_level: Option<u8>,
    pub charging: bool,
    pub pointer_source: Option<PointerSource>,
    pub motion: Option<MotionFrame>,
}

pub trait BluetoothControllerDevice: Send + Sync {
    fn set_ds3_motion(
        &mut self,
        _motion: Option<polymer_bluetooth::devices::sony::dualshock3::motion::MotionState>,
    ) {
    }
    fn decode(&mut self, report: &[u8], received_at: Instant) -> Option<DecodedInput>;
    fn calibration(&self) -> &Calibration;
    fn calibration_mut(&mut self) -> &mut Calibration;

    fn descriptor(&self, id: DeviceId, connected: &ConnectedController) -> DeviceDescriptor {
        DeviceDescriptor {
            id,
            name: Arc::from(
                connected
                    .identity
                    .name
                    .as_deref()
                    .unwrap_or(connected.profile_id.as_str()),
            ),
            profile: Some(Arc::from(connected.profile_id.as_str())),
            vendor_id: connected.vendor_product.map(|ids| ids.vendor_id),
            product_id: connected.vendor_product.map(|ids| ids.product_id),
            serial: connected
                .serial
                .as_deref()
                .and_then(DeviceDescriptor::serial_uuid),
            role: self.role(),
            capabilities: self.capabilities(),
        }
    }

    fn role(&self) -> DeviceRole {
        DeviceRole::Whole
    }

    fn additional_capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::RUMBLE
            | Capabilities::PLAYER_LED
            | Capabilities::BATTERY
            | Capabilities::RAW_HID
            | Capabilities::CALIBRATE_STICK_CENTER
            | Capabilities::CALIBRATE_STICK_RANGE
            | Capabilities::CALIBRATE_DEAD_ZONE
            | self.additional_capabilities()
    }

    fn apply_feedback(
        &mut self,
        manager: &BluetoothManager,
        session: SessionId,
        feedback: &Feedback,
    ) -> Result<(), BackendError> {
        match feedback {
            Feedback::Rumble {
                low_frequency,
                high_frequency,
                ..
            } => {
                let manager = manager.clone();
                let low = *low_frequency;
                let high = *high_frequency;
                tokio::spawn(async move {
                    let _ = manager
                        .actor
                        .send(SetRumble {
                            session_id: session,
                            low_frequency: low,
                            high_frequency: high,
                        })
                        .timeout(COMMAND_RESPONSE_TIMEOUT)
                        .await;
                });
            }
            Feedback::PlayerLed { index } => {
                let manager = manager.clone();
                let index = *index;
                tokio::spawn(async move {
                    let _ = manager
                        .actor
                        .send(SetPlayerIndex {
                            session_id: session,
                            index,
                        })
                        .timeout(COMMAND_RESPONSE_TIMEOUT)
                        .await;
                });
            }
            Feedback::Calibrate(calibration) => {
                *self.calibration_mut() = calibration.clone();
            }
            Feedback::Recenter | Feedback::HdRumble(_) | Feedback::HomeLed { .. } => {
                return Err(BackendError::Unsupported);
            }
        }
        Ok(())
    }

    fn apply_calibration(&self, state: &mut DeviceState) {
        self.calibration().correct(state);
        self.calibration().shape(state);
    }

    fn raw_hid_filter(&self) -> Option<Arc<dyn RawHidFilter>> {
        None
    }
}

pub fn for_profile(profile: &ProfileId) -> Option<Box<dyn BluetoothControllerDevice>> {
    match profile.as_str() {
        "sony.dualshock3" => Some(Box::new(dualshock3::DualShock3Decoder::default())),
        "nintendo.wiimote.original" | "nintendo.wiimote.plus" | "nintendo.wiimote.unknown" => {
            Some(Box::new(wiimote::WiimoteDecoder::default()))
        }
        "nintendo.switch.pro" => Some(Box::new(switch::SwitchProDecoder::default())),
        "nintendo.switch.joycon-left" => Some(Box::new(switch::JoyConLeftDecoder::default())),
        "nintendo.switch.joycon-right" => Some(Box::new(switch::JoyConRightDecoder::default())),
        "nintendo.switch.nso-nes" => Some(Box::new(switch::NsoNesDecoder::default())),
        "nintendo.switch.nso-snes" => Some(Box::new(switch::NsoSnesDecoder::default())),
        "nintendo.switch.nso-n64" => Some(Box::new(switch::NsoN64Decoder::default())),
        "nintendo.switch.nso-genesis" => Some(Box::new(switch::NsoGenesisDecoder::default())),
        "nintendo.switch2.joycon-left" => Some(Box::new(switch2::JoyCon2LeftDecoder::default())),
        "nintendo.switch2.joycon-right" => Some(Box::new(switch2::JoyCon2RightDecoder::default())),
        "nintendo.switch2.pro" => Some(Box::new(switch2::Switch2ProDecoder::default())),
        "nintendo.switch2.nso-gamecube" => Some(Box::new(switch2::NsoGameCubeDecoder::default())),
        _ => None,
    }
}

pub(crate) fn unsupported() -> Box<dyn BluetoothControllerDevice> {
    Box::new(UnsupportedControllerDevice {
        calibration: Calibration::default(),
    })
}

struct UnsupportedControllerDevice {
    calibration: Calibration,
}

impl BluetoothControllerDevice for UnsupportedControllerDevice {
    fn decode(&mut self, _report: &[u8], _received_at: Instant) -> Option<DecodedInput> {
        None
    }

    fn calibration(&self) -> &Calibration {
        &self.calibration
    }

    fn calibration_mut(&mut self) -> &mut Calibration {
        &mut self.calibration
    }
}

pub(crate) fn set_button(state: &mut GamepadState, pressed: bool, button: GamepadButton) {
    if pressed {
        state.buttons |= button.mask();
    }
}

pub(crate) fn normalize_stick(raw: u16) -> i16 {
    let centered = i32::from(raw) - 2048;
    (centered * 32767 / 2047).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

pub(crate) fn pointer_axis(value: f32) -> i16 {
    ((value.clamp(0.0, 1.0) * 65535.0).round() as i32 - 32768) as i16
}
