//! Userspace Wii Remote report decoder and input providers.

pub mod airmouse;

const REPORT_CORE_ACCEL_IR10_EXT6: u8 = 0x37;
const BUTTON_MASK: u16 = !0x60e0;

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

/// Test-only userspace input provider. It owns decoding and is the boundary
/// where airmouse/pointer transforms are applied.
#[derive(Default)]
pub struct WiimoteInputProvider;
impl WiimoteInputProvider {
    pub fn process(&mut self, report: &[u8]) -> Option<Telemetry> {
        parse_report(report)
    }
}

pub fn parse_report(report: &[u8]) -> Option<Telemetry> {
    let report_id = *report.first()?;
    if !(0x30..=0x37).contains(&report_id) || report.len() < 3 {
        return None;
    }
    let raw_buttons = u16::from_le_bytes([report[1], report[2]]);
    let buttons = raw_buttons & BUTTON_MASK;
    let accelerometer = matches!(report_id, 0x31 | 0x33 | 0x35 | 0x37)
        .then(|| decode_accelerometer(report, raw_buttons))
        .flatten();
    let (ir_points, gyroscope) = if report_id == REPORT_CORE_ACCEL_IR10_EXT6 && report.len() >= 22 {
        (
            Some(decode_basic_ir(report[6..16].try_into().ok()?)),
            decode_motion_plus(report[16..22].try_into().ok()?),
        )
    } else {
        (None, None)
    };
    Some(Telemetry {
        report_id,
        buttons,
        accelerometer,
        gyroscope,
        ir_points,
    })
}

fn decode_accelerometer(report: &[u8], raw_buttons: u16) -> Option<Accelerometer> {
    let [x_msb, y_msb, z_msb] = *report.get(3..6)?.first_chunk::<3>()?;
    let x = u16::from(x_msb) << 2 | ((raw_buttons >> 5) & 0b11);
    let y9 = u16::from(y_msb) << 1 | ((raw_buttons >> 13) & 1);
    let z9 = u16::from(z_msb) << 1 | ((raw_buttons >> 14) & 1);
    Some(Accelerometer {
        x,
        y: y9 << 1 | y9 >> 8,
        z: z9 << 1 | z9 >> 8,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ignores_non_data_reports() {
        assert_eq!(parse_report(&[0x22, 0, 0]), None);
    }
}
