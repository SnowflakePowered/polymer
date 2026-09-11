use super::*;
use crate::decoder::TouchContact;
use polymer_bluetooth::devices::sony::dualsense::motion::FactoryCalibration;

impl DualSenseDecoder {
    pub(super) fn read_sensors(&mut self, report: &InputReport, state: &mut GamepadState) {
        let calibration = self.sensors.unwrap_or_else(FactoryCalibration::nominal);
        state.accelerometer = calibration.acceleration(report.accelerometer.map(|v| v.get()));
        state.gyroscope = calibration.rotation(report.gyroscope.map(|v| v.get()));
        let timestamp = u32::from_le_bytes(report.timestamp);
        if let Some(previous) = self.last_timestamp {
            self.elapsed_ticks += u64::from(timestamp.wrapping_sub(previous));
        }
        self.last_timestamp = Some(timestamp);
        state.sensor_timestamp_ns = Some(self.elapsed_ticks * 1000 / 3);
        let contacts = std::array::from_fn(|i| {
            let contact = &report.touch[i];
            TouchContact {
                id: contact.id(),
                active: contact.active(),
                position: if contact.active() {
                    contact.position()
                } else {
                    [0; 2]
                },
            }
        });
        // Retain the same finger while both are down, regardless of slot order.
        let primary = contacts
            .iter()
            .find(|c| c.active && Some(c.id) == self.primary_touch)
            .or_else(|| contacts.iter().find(|c| c.active));
        self.primary_touch = primary.map(|c| c.id);
        if let Some(contact) = primary {
            self.last_pointer = std::array::from_fn(|axis| {
                let extent = [1920.0, 1080.0][axis];
                super::super::pointer_axis(f32::from(contact.position[axis]) / extent)
            });
        }
        state.pointer = self.last_pointer;
        state.touch_contacts = Some(contacts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic wire vectors with independently generated Python zlib CRCs.
    #[test]
    fn calibrated_motion_tracks_time_and_preserves_finger_identity() {
        let mut decoder = DualSenseDecoder::default();
        decoder.set_sensor_calibration(FactoryCalibration::parse(include_bytes!(
            "fixtures/calibration.bin"
        )));
        let first = decoder
            .decode(include_bytes!("fixtures/two_contacts.bin"), Instant::now())
            .unwrap();
        for (actual, expected) in first.state.gyroscope.into_iter().zip([
            std::f32::consts::FRAC_PI_2,
            0.0,
            -std::f32::consts::FRAC_PI_2,
        ]) {
            assert!((actual - expected).abs() < 0.00001);
        }
        assert_eq!(first.state.accelerometer, [0.0, 9.80665, 0.0]);
        assert_eq!(first.state.sensor_timestamp_ns, Some(0));
        let contacts = first.state.touch_contacts.unwrap();
        assert_eq!(contacts[0].position, [1000, 500]);
        assert_eq!(contacts[1].position, [1920, 1080]);
        let swapped = decoder
            .decode(include_bytes!("fixtures/swapped_slots.bin"), Instant::now())
            .unwrap();
        assert_eq!(swapped.state.sensor_timestamp_ns, Some(16_000));
        assert_eq!(swapped.state.pointer, first.state.pointer);
        assert_eq!(swapped.state.touch_contacts.unwrap()[1].id, 7);
        let released = decoder
            .decode(include_bytes!("fixtures/released.bin"), Instant::now())
            .unwrap();
        assert_eq!(released.state.pointer, first.state.pointer);
        assert!(
            released
                .state
                .touch_contacts
                .unwrap()
                .iter()
                .all(|c| !c.active && c.position == [0; 2])
        );
        assert_eq!(released.pointer_source, Some(PointerSource::Touchpad));
    }

    #[test]
    fn rejected_packet_does_not_advance_sensor_clock() {
        let mut decoder = DualSenseDecoder::default();
        decoder
            .decode(include_bytes!("fixtures/two_contacts.bin"), Instant::now())
            .unwrap();
        let mut bad = *include_bytes!("fixtures/swapped_slots.bin");
        bad[29] ^= 1;
        assert!(decoder.decode(&bad, Instant::now()).is_none());
        let valid = decoder
            .decode(include_bytes!("fixtures/swapped_slots.bin"), Instant::now())
            .unwrap();
        assert_eq!(valid.state.sensor_timestamp_ns, Some(16_000));
        // Nominal conversion remains available without a feature report.
        assert!((valid.state.accelerometer[1] - 7992.0 * 9.80665 / 8192.0).abs() < 0.00001);
    }
}
