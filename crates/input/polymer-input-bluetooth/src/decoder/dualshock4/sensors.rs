use super::*;
use polymer_bluetooth::devices::sony::dualshock4::motion::FactoryCalibration;

impl DualShock4Decoder {
    pub(super) fn read_sensors(&mut self, report: &InputReport, state: &mut GamepadState) {
        let calibration = self.sensors.unwrap_or_else(FactoryCalibration::nominal);
        state.accelerometer = calibration.acceleration(report.accelerometer.map(|v| v.get()));
        state.gyroscope = calibration.rotation(report.gyroscope.map(|v| v.get()));
        let timestamp = u16::from_le_bytes(report.timestamp);
        if let Some(previous) = self.last_timestamp {
            self.elapsed_ticks += u64::from(timestamp.wrapping_sub(previous));
        }
        self.last_timestamp = Some(timestamp);
        state.sensor_timestamp_ns = Some(self.elapsed_ticks * 16_000 / 3);
        self.touch
            .apply(report.latest_touch(), [1920.0, 942.0], state);
    }
}
