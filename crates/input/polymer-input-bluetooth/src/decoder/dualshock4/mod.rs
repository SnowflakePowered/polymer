use super::sony::{SonySensorFormat, SonySensors};
use super::{BluetoothControllerDevice, DecodedInput, PointerSource};
use polymer_bluetooth::devices::sony::dualshock4::protocol::InputReport;
use polymer_input::{Calibration, Capabilities, DeviceRole};
use std::time::Instant;

#[derive(Default)]
pub struct DualShock4Decoder {
    calibration: Calibration,
    sensors: SonySensors,
}

impl DualShock4Decoder {
    const SENSORS: SonySensorFormat = SonySensorFormat {
        timestamp_mask: u16::MAX as u32,
        tick_ns: [16_000, 3],
        touch_extent: [1920.0, 942.0],
    };
}

impl BluetoothControllerDevice for DualShock4Decoder {
    fn set_sensor_calibration(
        &mut self,
        calibration: Option<polymer_bluetooth::devices::SensorCalibration>,
    ) {
        self.sensors.calibration = calibration;
    }
    fn additional_capabilities(&self) -> Capabilities {
        Capabilities::ACCELEROMETER
            | Capabilities::GYROSCOPE
            | Capabilities::TOUCHPAD
            | Capabilities::POINTER_SURFACE
            | Capabilities::POINTER_CONTACT
            | Capabilities::CALIBRATE_MOTION_BIAS
            | Capabilities::CALIBRATE_MOTION_SCALE
    }
    fn decode(&mut self, data: &[u8], _: Instant) -> Option<DecodedInput> {
        let report = InputReport::parse(data)?;
        let mut state = super::sony::SonyControls::state(
            report.sticks,
            report.triggers,
            u32::from_le_bytes([
                report.buttons[0],
                report.buttons[1],
                report.buttons[2] & 3,
                0,
            ]),
        );
        self.sensors.read(
            &Self::SENSORS,
            [report.accelerometer, report.gyroscope].map(|axes| axes.map(|v| v.get())),
            u32::from(u16::from_le_bytes(report.timestamp)),
            report.latest_touch(),
            &mut state,
        );
        let cable = report.status[0] & 0x10 != 0;
        let level = report.status[0] & 15;
        let battery_level = match level {
            0..=10 => Some((level * 10 + 5).min(100)),
            11 if cable => Some(100),
            _ => None,
        };
        Some(DecodedInput {
            state,
            role: DeviceRole::Whole,
            battery_level,
            charging: cable && level <= 10,
            pointer_source: Some(PointerSource::Touchpad),
            motion: None,
        })
    }
    fn calibration(&self) -> &Calibration {
        &self.calibration
    }
    fn calibration_mut(&mut self) -> &mut Calibration {
        &mut self.calibration
    }
}
