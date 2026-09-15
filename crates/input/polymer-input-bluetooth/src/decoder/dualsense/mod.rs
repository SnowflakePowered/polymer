use super::{BluetoothControllerDevice, DecodedInput, GamepadState, PointerSource};
use polymer_bluetooth::devices::sony::dualsense::protocol::InputReport;
use polymer_input::{Calibration, Capabilities, DeviceRole};
use std::time::Instant;

#[derive(Default)]
pub struct DualSenseDecoder {
    calibration: Calibration,
    sensors: Option<polymer_bluetooth::devices::SensorCalibration>,
    last_timestamp: Option<u32>,
    elapsed_ticks: u64,
    touch: super::sony::SonyTouchState,
}

mod sensors;

impl BluetoothControllerDevice for DualSenseDecoder {
    fn set_sensor_calibration(
        &mut self,
        calibration: Option<polymer_bluetooth::devices::SensorCalibration>,
    ) {
        self.sensors = calibration;
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
        self.read_sensors(report, &mut state);
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
