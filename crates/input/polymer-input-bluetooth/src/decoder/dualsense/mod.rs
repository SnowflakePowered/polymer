use super::sony::{SonySensorFormat, SonySensors};
use super::{BluetoothControllerDevice, DecodedInput, PointerSource};
use polymer_bluetooth::devices::sony::dualsense::protocol::InputReport;
use polymer_input::{Calibration, Capabilities, DeviceRole};
use std::time::Instant;

#[derive(Default)]
pub struct DualSenseDecoder {
    calibration: Calibration,
    sensors: SonySensors,
}

impl DualSenseDecoder {
    const SENSORS: SonySensorFormat = SonySensorFormat {
        timestamp_mask: u32::MAX,
        tick_ns: [1000, 3],
        touch_extent: [1920.0, 1080.0],
    };
}

impl BluetoothControllerDevice for DualSenseDecoder {
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
            u32::from_le_bytes(report.buttons),
        );
        self.sensors.read(
            &Self::SENSORS,
            [report.accelerometer, report.gyroscope].map(|axes| axes.map(|v| v.get())),
            u32::from_le_bytes(report.timestamp),
            Some(&report.touch),
            &mut state,
        );
        let charge = report.status[0] >> 4;
        let battery_level = match charge {
            0 | 1 => Some(((report.status[0] & 15) * 10 + 5).min(100)),
            2 => Some(100),
            _ => None,
        };
        Some(DecodedInput {
            state,
            role: DeviceRole::Whole,
            battery_level,
            charging: charge == 1,
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
