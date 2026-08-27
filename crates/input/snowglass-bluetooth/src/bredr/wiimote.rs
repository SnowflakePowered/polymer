//! Wii Remote initialization and input report decoding shared by the test viewer and slot layer.

pub mod airmouse;

use std::time::Duration;

use crate::{Result, Transport};

use super::BrEdrHid;

const LED_REPORT_ID: u8 = 0x11;
const REPORT_MODE_ID: u8 = 0x12;
const IR_ENABLE_ID: u8 = 0x13;
const IR_ENABLE_2_ID: u8 = 0x1a;
const WRITE_DATA_ID: u8 = 0x16;
const CONTINUOUS: u8 = 0x04;
const REPORT_CORE_ACCEL_IR10_EXT6: u8 = 0x37;

const I2C_CAMERA: u8 = 0x58;
const I2C_MOTION_PLUS_INACTIVE: u8 = 0x53;
const MOTION_PLUS_INIT_ADDRESS: u16 = 0x00f0;
const MOTION_PLUS_MODE_ADDRESS: u16 = 0x00fe;
const MOTION_PLUS_DISABLED_PASSTHROUGH: u8 = 0x04;

const BUTTON_MASK: u16 = !0x60e0;

/// A decoded Wii Remote accelerometer sample in its native 10-bit range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Accelerometer {
    pub x: u16,
    pub y: u16,
    pub z: u16,
}

/// One point reported by the Wii Remote IR camera.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrPoint {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
}

/// A raw 14-bit MotionPlus sample and its per-axis range flags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gyroscope {
    pub pitch: u16,
    pub roll: u16,
    pub yaw: u16,
    pub pitch_slow: bool,
    pub roll_slow: bool,
    pub yaw_slow: bool,
}

/// Live values decoded from one Wii Remote input data report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Telemetry {
    pub report_id: u8,
    pub buttons: u16,
    pub accelerometer: Option<Accelerometer>,
    pub gyroscope: Option<Gyroscope>,
    pub ir_points: Option<[IrPoint; 4]>,
}

impl Telemetry {
    /// Returns pressed core buttons in the order used by the test viewer.
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

/// Builds the Wii Remote LED output report for a zero-based player slot.
/// The rumble bit shares this byte and must be preserved by callers changing
/// LEDs while rumble is active.
pub fn player_led_report(slot: usize, rumble: bool) -> [u8; 2] {
    let led = 0x10u8 << (slot % 4);
    [LED_REPORT_ID, led | u8::from(rumble)]
}

/// Stops the pairing animation and displays the assigned player number.
pub async fn set_player_led(transport: &mut BrEdrHid, slot: usize, rumble: bool) -> Result<()> {
    transport.send(&player_led_report(slot, rumble)).await
}

/// Enables the IR camera and built-in/external MotionPlus, then selects the
/// continuous report that carries buttons, acceleration, IR, and gyro data.
/// A non-Plus Wii Remote remains usable; its extension bytes simply will not
/// decode as MotionPlus samples.
pub async fn initialize(transport: &mut BrEdrHid, slot: usize) -> Result<()> {
    set_player_led(transport, slot, false).await?;

    // Activate MotionPlus with passthrough disabled. The inactive device is at
    // I2C address 0x53 and moves to 0x52 after the mode write.
    send_i2c_write(
        transport,
        I2C_MOTION_PLUS_INACTIVE,
        MOTION_PLUS_INIT_ADDRESS,
        &[0x55],
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    send_i2c_write(
        transport,
        I2C_MOTION_PLUS_INACTIVE,
        MOTION_PLUS_MODE_ADDRESS,
        &[MOTION_PLUS_DISABLED_PASSTHROUGH],
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Dolphin's real-Wiimote backend enables both IR logic reports before
    // programming the camera. Level 3 is the Wii's middle sensitivity.
    transport.send(&[IR_ENABLE_2_ID, 0x04]).await?;
    transport.send(&[IR_ENABLE_ID, 0x04]).await?;
    send_i2c_write(
        transport,
        I2C_CAMERA,
        0x0000,
        &[0x02, 0x00, 0x00, 0x71, 0x01, 0x00, 0xaa, 0x00, 0x64],
    )
    .await?;
    send_i2c_write(transport, I2C_CAMERA, 0x001a, &[0x63, 0x03]).await?;
    send_i2c_write(transport, I2C_CAMERA, 0x0033, &[0x01]).await?;
    send_i2c_write(transport, I2C_CAMERA, 0x0030, &[0x08]).await?;

    transport
        .send(&[REPORT_MODE_ID, CONTINUOUS, REPORT_CORE_ACCEL_IR10_EXT6])
        .await
}

async fn send_i2c_write(
    transport: &mut BrEdrHid,
    slave: u8,
    address: u16,
    data: &[u8],
) -> Result<()> {
    debug_assert!(data.len() <= 16);
    let mut report = [0u8; 22];
    report[0] = WRITE_DATA_ID;
    report[1] = 0x04; // I2C address space in bits 2..3.
    report[2] = slave << 1;
    report[3..5].copy_from_slice(&address.to_be_bytes());
    report[5] = data.len() as u8;
    report[6..6 + data.len()].copy_from_slice(data);
    transport.send(&report).await
}

/// Decodes Wii Remote data reports. Handshake, status, acknowledgement, and
/// memory-read replies return `None` so they do not replace live telemetry.
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
        // Replicate the top bit while expanding 9-bit Y/Z into the 10-bit
        // range used by Dolphin and by the rest of Snowglass.
        y: y9 << 1 | y9 >> 8,
        z: z9 << 1 | z9 >> 8,
    })
}

fn decode_basic_ir(data: &[u8; 10]) -> [IrPoint; 4] {
    let decode_block = |block: &[u8]| {
        let packed = block[2];
        [
            ir_point(
                u16::from(block[0]) | (u16::from((packed >> 4) & 0b11) << 8),
                u16::from(block[1]) | (u16::from((packed >> 6) & 0b11) << 8),
            ),
            ir_point(
                u16::from(block[3]) | (u16::from(packed & 0b11) << 8),
                u16::from(block[4]) | (u16::from((packed >> 2) & 0b11) << 8),
            ),
        ]
    };
    let first = decode_block(&data[..5]);
    let second = decode_block(&data[5..]);
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
    // Bit 1 of byte 5 distinguishes MotionPlus data from passthrough data.
    if data[5] & 0x02 == 0 {
        return None;
    }
    Some(Gyroscope {
        yaw: u16::from(data[0]) | (u16::from(data[3] >> 2) << 8),
        roll: u16::from(data[1]) | (u16::from(data[4] >> 2) << 8),
        pitch: u16::from(data[2]) | (u16::from(data[5] >> 2) << 8),
        pitch_slow: data[3] & 0x01 != 0,
        yaw_slow: data[3] & 0x02 != 0,
        roll_slow: data[4] & 0x02 != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_one_hot_player_led_reports() {
        assert_eq!(player_led_report(0, false), [0x11, 0x10]);
        assert_eq!(player_led_report(1, false), [0x11, 0x20]);
        assert_eq!(player_led_report(3, true), [0x11, 0x81]);
        assert_eq!(player_led_report(4, false), [0x11, 0x10]);
    }

    #[test]
    fn decodes_core_accel_ir_and_motion_plus() {
        let mut report = [0xff; 22];
        report[0] = 0x37;
        report[1] = 0x79; // Left, Up, Plus, and accel X low bits.
        report[2] = 0xe8; // A, Home, and accel Y/Z low bits.
        report[3..6].copy_from_slice(&[0x80, 0x81, 0x82]);
        report[6..11].copy_from_slice(&[0x2c, 0x90, 0x5a, 0xbc, 0x58]);
        report[16..22].copy_from_slice(&[0x34, 0xbc, 0x56, 0x4b, 0xaa, 0xd2]);

        let telemetry = parse_report(&report).unwrap();
        assert_eq!(telemetry.buttons, 0x8819);
        assert_eq!(
            telemetry.pressed_buttons().collect::<Vec<_>>(),
            ["A", "+", "Home", "Up", "Left"]
        );
        assert_eq!(
            telemetry.accelerometer,
            Some(Accelerometer {
                x: 515,
                y: 519,
                z: 523,
            })
        );
        assert_eq!(
            telemetry.ir_points.unwrap()[..2],
            [
                IrPoint {
                    x: 300,
                    y: 400,
                    visible: true,
                },
                IrPoint {
                    x: 700,
                    y: 600,
                    visible: true,
                },
            ]
        );
        assert_eq!(
            telemetry.gyroscope,
            Some(Gyroscope {
                pitch: 0x3456,
                roll: 0x2abc,
                yaw: 0x1234,
                pitch_slow: true,
                roll_slow: true,
                yaw_slow: true,
            })
        );
    }

    #[test]
    fn ignores_non_data_reports_and_motion_plus_passthrough() {
        assert_eq!(parse_report(&[0x22, 0, 0, 0x16, 0]), None);
        let mut report = [0xff; 22];
        report[0] = 0x37;
        report[21] &= !0x02;
        assert!(parse_report(&report).unwrap().gyroscope.is_none());
    }
}
