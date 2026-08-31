use snowglass_bluetooth::devices::ProfileId;
use snowglass_input::{DeviceRole, GamepadButton};
use std::time::Instant;

pub mod switch;
pub mod switch2;
pub mod wiimote;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GamepadState {
    pub buttons: u32,
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

pub trait ReportDecoder: Send + Sync {
    fn decode(&mut self, report: &[u8], received_at: Instant) -> Option<DecodedInput>;

    fn recenter(&mut self) -> bool {
        false
    }
}

pub fn for_profile(profile: &ProfileId) -> Option<Box<dyn ReportDecoder>> {
    match profile.as_str() {
        "nintendo.wiimote.original" | "nintendo.wiimote.plus" | "nintendo.wiimote.unknown" => {
            Some(Box::new(wiimote::WiimoteDecoder::default()))
        }
        "nintendo.switch.pro" => Some(Box::new(switch::SwitchProDecoder)),
        "nintendo.switch.joycon-left" => Some(Box::new(switch::JoyConLeftDecoder)),
        "nintendo.switch.joycon-right" => Some(Box::new(switch::JoyConRightDecoder)),
        "nintendo.switch.nso-nes" => Some(Box::new(switch::NsoNesDecoder)),
        "nintendo.switch.nso-snes" => Some(Box::new(switch::NsoSnesDecoder)),
        "nintendo.switch.nso-n64" => Some(Box::new(switch::NsoN64Decoder)),
        "nintendo.switch.nso-genesis" => Some(Box::new(switch::NsoGenesisDecoder)),
        "nintendo.switch2.joycon-left" => Some(Box::new(switch2::JoyCon2LeftDecoder::default())),
        "nintendo.switch2.joycon-right" => Some(Box::new(switch2::JoyCon2RightDecoder::default())),
        "nintendo.switch2.pro" => Some(Box::new(switch2::Switch2ProDecoder::default())),
        "nintendo.switch2.nso-gamecube" => Some(Box::new(switch2::NsoGameCubeDecoder::default())),
        _ => None,
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
