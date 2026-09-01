use super::{DecodedInput, GamepadState, PointerSource, ReportDecoder, pointer_axis, set_button};
use crate::airmouse::{AirMouse, PointerPosition};
use snowglass_input::{DeviceRole, GamepadButton};
use std::time::{Duration, Instant};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

const BUTTON_MASK: u16 = !0x60e0;
const DOUBLE_HOME_WINDOW: Duration = Duration::from_millis(500);

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct CoreReport {
    report_id: u8,
    buttons: [u8; 2],
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct AccelerometerReport {
    _report_id: u8,
    _buttons: [u8; 2],
    accelerometer: [u8; 3],
}

/// Protocol-shape metadata shared by normalized observation and the raw
/// Dolphin bridge. It deliberately describes byte locations rather than
/// decoding a second semantic controller state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReportMetadata {
    pub report_id: u8,
    pub core_buttons: bool,
}

impl ReportMetadata {
    pub fn inspect(report: &[u8]) -> Option<Self> {
        let report_id = *report.first()?;
        let core_buttons =
            matches!(report_id, 0x20..=0x22 | 0x30..=0x3c | 0x3e..=0x3f) && report.len() >= 3;
        (core_buttons || report_id == 0x3d).then_some(Self {
            report_id,
            core_buttons,
        })
    }

    pub fn filter_for_dolphin(self, report: &mut [u8], capture: bool) {
        if !self.core_buttons {
            return;
        }
        if capture {
            report[1] &= 0xe0;
            report[2] &= 0x60;
        } else {
            report[2] &= !0x80;
        }
    }
}

pub mod airmouse {
    pub use crate::airmouse::*;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Accelerometer {
    pub x: u16,
    pub y: u16,
    pub z: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrPoint {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gyroscope {
    pub pitch: u16,
    pub roll: u16,
    pub yaw: u16,
    pub pitch_slow: bool,
    pub roll_slow: bool,
    pub yaw_slow: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Telemetry {
    pub report_id: u8,
    pub buttons: u16,
    pub accelerometer: Option<Accelerometer>,
    pub gyroscope: Option<Gyroscope>,
    pub ir_points: Option<[IrPoint; 4]>,
}

impl Telemetry {
    pub fn pressed_buttons(self) -> impl Iterator<Item = &'static str> {
        const BUTTONS: [(u16, &str); 11] = [
            (0x0800, "A"),
            (0x0400, "B"),
            (0x0200, "1"),
            (0x0100, "2"),
            (0x0010, "+"),
            (0x1000, "−"),
            (0x8000, "Home"),
            (0x0008, "Up"),
            (0x0004, "Down"),
            (0x0001, "Left"),
            (0x0002, "Right"),
        ];
        BUTTONS
            .into_iter()
            .filter_map(move |(mask, name)| (self.buttons & mask != 0).then_some(name))
    }

    pub fn home_pressed(self) -> bool {
        self.buttons & 0x8000 != 0
    }
}

#[derive(Default)]
pub struct WiimoteTelemetryDecoder;

impl WiimoteTelemetryDecoder {
    pub fn process(&mut self, report: &[u8]) -> Option<Telemetry> {
        parse_report(report)
    }
}

pub fn parse_report(report: &[u8]) -> Option<Telemetry> {
    let (core, _) = CoreReport::ref_from_prefix(report).ok()?;
    let report_id = core.report_id;
    if !(0x30..=0x3f).contains(&report_id) || report_id == 0x3d {
        return None;
    }
    let raw_buttons = u16::from_le_bytes(core.buttons);
    let buttons = raw_buttons & BUTTON_MASK;
    let accelerometer = matches!(report_id, 0x31 | 0x33 | 0x35 | 0x37)
        .then(|| {
            AccelerometerReport::ref_from_prefix(report)
                .ok()
                .map(|(report, _)| decode_accelerometer(report.accelerometer, raw_buttons))
        })
        .flatten();
    let ir_points = match report_id {
        0x33 => report
            .get(6..18)
            .and_then(|data| <&[u8; 12]>::try_from(data).ok())
            .map(decode_extended_ir),
        0x36 => report
            .get(3..13)
            .and_then(|data| <&[u8; 10]>::try_from(data).ok())
            .map(decode_basic_ir),
        0x37 => report
            .get(6..16)
            .and_then(|data| <&[u8; 10]>::try_from(data).ok())
            .map(decode_basic_ir),
        _ => None,
    };
    let gyroscope = match report_id {
        0x35 => report
            .get(6..12)
            .and_then(|data| <&[u8; 6]>::try_from(data).ok())
            .and_then(decode_motion_plus),
        0x37 => report
            .get(16..22)
            .and_then(|data| <&[u8; 6]>::try_from(data).ok())
            .and_then(decode_motion_plus),
        _ => None,
    };
    Some(Telemetry {
        report_id,
        buttons,
        accelerometer,
        gyroscope,
        ir_points,
    })
}

#[derive(Default)]
pub struct WiimoteDecoder {
    air_mouse: AirMouse,
    last_home_press: Option<Instant>,
    home_was_pressed: bool,
    interleaved_ir: [Option<[IrPoint; 2]>; 2],
}

impl WiimoteDecoder {
    pub fn decode(&mut self, report: &[u8], received_at: Instant) -> Option<DecodedInput> {
        let mut telemetry = parse_report(report)?;
        if matches!(telemetry.report_id, 0x3e | 0x3f)
            && let Some(data) = report
                .get(4..22)
                .and_then(|data| <&[u8; 18]>::try_from(data).ok())
        {
            self.interleaved_ir[usize::from(telemetry.report_id - 0x3e)] =
                Some(decode_full_ir_half(data));
            let hidden = [ir_point(1023, 1023); 2];
            let first = self.interleaved_ir[0].unwrap_or(hidden);
            let second = self.interleaved_ir[1].unwrap_or(hidden);
            telemetry.ir_points = Some([first[0], first[1], second[0], second[1]]);
        }
        let buttons = telemetry.buttons;
        let accelerometer = telemetry.accelerometer;
        let gyroscope = telemetry.gyroscope;
        let ir_points = telemetry.ir_points;

        let mut state = GamepadState::default();
        for (mask, button) in [
            (0x0800, GamepadButton::A),
            (0x0400, GamepadButton::B),
            (0x0200, GamepadButton::X),
            (0x0100, GamepadButton::Y),
            (0x0010, GamepadButton::Start),
            (0x1000, GamepadButton::Select),
            (0x8000, GamepadButton::Guide),
            (0x0008, GamepadButton::DpadUp),
            (0x0004, GamepadButton::DpadDown),
            (0x0001, GamepadButton::DpadLeft),
            (0x0002, GamepadButton::DpadRight),
        ] {
            set_button(&mut state, buttons & mask != 0, button);
        }

        if let Some(accel) = accelerometer {
            const RAW_PER_G: f32 = 104.0;
            const GRAVITY: f32 = 9.80665;
            state.accelerometer = [
                (f32::from(accel.x) - 512.0) / RAW_PER_G * GRAVITY,
                (f32::from(accel.y) - 512.0) / RAW_PER_G * GRAVITY,
                (f32::from(accel.z) - 512.0) / RAW_PER_G * GRAVITY,
            ];
        }
        if let Some(gyro) = gyroscope {
            let rate =
                |raw: u16, slow: bool| (f32::from(raw) - 8192.0) / if slow { 923.5 } else { 207.8 };
            state.gyroscope = [
                rate(gyro.pitch, gyro.pitch_slow),
                rate(gyro.roll, gyro.roll_slow),
                rate(gyro.yaw, gyro.yaw_slow),
            ];
        }

        let home_pressed = buttons & 0x8000 != 0;
        if home_pressed && !self.home_was_pressed {
            if self
                .last_home_press
                .is_some_and(|at| received_at.saturating_duration_since(at) <= DOUBLE_HOME_WINDOW)
            {
                self.last_home_press = None;
                self.air_mouse.center();
            } else {
                self.last_home_press = Some(received_at);
            }
        }
        self.home_was_pressed = home_pressed;

        let mut pointer_source = None;
        let position = ir_points
            .and_then(ir_pointer_position)
            .inspect(|position| {
                self.air_mouse
                    .track_absolute_position(*position, received_at);
                pointer_source = Some(PointerSource::IrCamera);
            })
            .or_else(|| {
                let position =
                    self.air_mouse
                        .next_pointer_position(accelerometer?, gyroscope?, received_at);
                pointer_source = Some(PointerSource::AirMouse);
                Some(position)
            });
        if let Some(position) = position {
            state.pointer = [pointer_axis(position.x), pointer_axis(position.y)];
        }
        Some(DecodedInput {
            state,
            role: DeviceRole::Whole,
            battery_level: None,
            charging: false,
            pointer_source,
            motion: None,
        })
    }

    pub fn center(&mut self) {
        self.air_mouse.center();
    }

    pub fn recalibrate(&mut self, accelerometer: Accelerometer, gyroscope: Gyroscope) {
        self.air_mouse.recalibrate(accelerometer, gyroscope);
    }
}

impl ReportDecoder for WiimoteDecoder {
    fn decode(&mut self, report: &[u8], received_at: Instant) -> Option<DecodedInput> {
        WiimoteDecoder::decode(self, report, received_at)
    }

    fn recenter(&mut self) -> bool {
        self.center();
        true
    }
}

fn decode_accelerometer([x_msb, y_msb, z_msb]: [u8; 3], raw_buttons: u16) -> Accelerometer {
    let x = u16::from(x_msb) << 2 | ((raw_buttons >> 5) & 0b11);
    let y9 = u16::from(y_msb) << 1 | ((raw_buttons >> 13) & 1);
    let z9 = u16::from(z_msb) << 1 | ((raw_buttons >> 14) & 1);
    Accelerometer {
        x,
        y: y9 << 1 | y9 >> 8,
        z: z9 << 1 | z9 >> 8,
    }
}

fn decode_basic_ir(data: &[u8; 10]) -> [IrPoint; 4] {
    let decode = |block: &[u8]| {
        let packed = block[2];
        [
            ir_point(
                u16::from(block[0]) | (u16::from((packed >> 4) & 3) << 8),
                u16::from(block[1]) | (u16::from((packed >> 6) & 3) << 8),
            ),
            ir_point(
                u16::from(block[3]) | (u16::from(packed & 3) << 8),
                u16::from(block[4]) | (u16::from((packed >> 2) & 3) << 8),
            ),
        ]
    };
    let first = decode(&data[..5]);
    let second = decode(&data[5..]);
    [first[0], first[1], second[0], second[1]]
}

fn decode_extended_ir(data: &[u8; 12]) -> [IrPoint; 4] {
    std::array::from_fn(|index| {
        let point = &data[index * 3..index * 3 + 3];
        ir_point(
            u16::from(point[0]) | (u16::from((point[2] >> 4) & 3) << 8),
            u16::from(point[1]) | (u16::from((point[2] >> 6) & 3) << 8),
        )
    })
}

fn decode_full_ir_half(data: &[u8; 18]) -> [IrPoint; 2] {
    std::array::from_fn(|index| {
        let point = &data[index * 9..index * 9 + 3];
        ir_point(
            u16::from(point[0]) | (u16::from((point[2] >> 4) & 3) << 8),
            u16::from(point[1]) | (u16::from((point[2] >> 6) & 3) << 8),
        )
    })
}

fn ir_point(x: u16, y: u16) -> IrPoint {
    IrPoint {
        x,
        y,
        visible: x < 1024 && y < 768,
    }
}

fn decode_motion_plus(data: &[u8; 6]) -> Option<Gyroscope> {
    if data[5] & 0x02 == 0 {
        return None;
    }
    Some(Gyroscope {
        yaw: u16::from(data[0]) | (u16::from(data[3] >> 2) << 8),
        roll: u16::from(data[1]) | (u16::from(data[4] >> 2) << 8),
        pitch: u16::from(data[2]) | (u16::from(data[5] >> 2) << 8),
        pitch_slow: data[3] & 1 != 0,
        yaw_slow: data[3] & 2 != 0,
        roll_slow: data[4] & 2 != 0,
    })
}

fn ir_pointer_position(points: [IrPoint; 4]) -> Option<PointerPosition> {
    let (x_sum, y_sum, count) = points
        .into_iter()
        .filter(|point| point.visible)
        .fold((0_u32, 0_u32, 0_u32), |(x, y, count), point| {
            (x + u32::from(point.x), y + u32::from(point.y), count + 1)
        });
    (count != 0).then(|| {
        PointerPosition {
            x: 1.0 - x_sum as f32 / count as f32 / 1023.0,
            y: y_sum as f32 / count as f32 / 767.0,
        }
        .clamped()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_a_and_home_reports_keep_distinct_semantic_buttons() {
        let mut decoder = WiimoteDecoder::default();
        let a = decoder
            .decode(&[0x30, 0x00, 0x08], Instant::now())
            .unwrap()
            .state;
        assert_ne!(a.buttons & GamepadButton::A.mask(), 0);
        assert_eq!(a.buttons & GamepadButton::Guide.mask(), 0);

        let home = decoder
            .decode(&[0x30, 0x00, 0x80], Instant::now())
            .unwrap()
            .state;
        assert_ne!(home.buttons & GamepadButton::Guide.mask(), 0);
        assert_eq!(home.buttons & GamepadButton::A.mask(), 0);
    }

    #[test]
    fn mirrors_ir_x() {
        let position = ir_pointer_position([
            ir_point(0, 384),
            ir_point(1023, 1023),
            ir_point(1023, 1023),
            ir_point(1023, 1023),
        ])
        .unwrap();
        assert_eq!(position.x, 1.0);
    }

    #[test]
    fn observes_extended_ir_without_requiring_report_mode_37() {
        let mut report = [0xff; 18];
        report[0] = 0x33;
        report[1..6].fill(0);
        report[6..9].copy_from_slice(&[0x00, 0x80, 0x10]);
        let telemetry = parse_report(&report).unwrap();
        let points = telemetry.ir_points.unwrap();
        assert_eq!(
            (points[0].x, points[0].y, points[0].visible),
            (256, 128, true)
        );
    }

    #[test]
    fn combines_interleaved_full_ir_reports() {
        let mut decoder = WiimoteDecoder::default();
        let mut first = [0xff; 22];
        first[0] = 0x3e;
        first[1..4].fill(0);
        first[4..7].copy_from_slice(&[0x20, 0x80, 0x10]);
        let observed = decoder.decode(&first, Instant::now()).unwrap();
        assert_eq!(observed.pointer_source, Some(PointerSource::IrCamera));

        let mut second = [0xff; 22];
        second[0] = 0x3f;
        second[1..4].fill(0);
        second[4..7].copy_from_slice(&[0x40, 0x90, 0x10]);
        let observed = decoder.decode(&second, Instant::now()).unwrap();
        assert_eq!(observed.pointer_source, Some(PointerSource::IrCamera));
    }
}
