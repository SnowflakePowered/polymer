use super::super::protocol::Report;
use crate::devices::SensorCalibration;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, byteorder::little_endian::I16};

/// Sony feature report 0x05, including report ID and Bluetooth CRC.
#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct FactoryCalibration {
    id: u8,
    gyro_bias: [I16; 3],
    gyro_plus: [I16; 3],
    gyro_minus: [I16; 3],
    gyro_speed: [I16; 2],
    accel_endpoints: [[I16; 2]; 3],
    reserved: [u8; 2],
    crc: [u8; 4],
}

impl FactoryCalibration {
    pub fn nominal() -> SensorCalibration {
        SensorCalibration {
            accelerometer_bias: [0.0; 3],
            accelerometer_scale: [9.80665 / 8192.0; 3],
            gyroscope_bias: [0.0; 3],
            gyroscope_scale: [std::f32::consts::PI / (180.0 * 16.0); 3],
        }
    }

    pub fn parse(bytes: &[u8]) -> Option<SensorCalibration> {
        if !Report::valid(bytes, 5, 41, 0xa3) {
            return None;
        }
        let data = Self::ref_from_bytes(bytes).ok()?;
        let speed = f32::from(data.gyro_speed[0].get()) + f32::from(data.gyro_speed[1].get());
        if speed <= 0.0 {
            return None;
        }
        let mut calibration = Self::nominal();
        for i in 0..3 {
            let gyro_span = (f32::from(data.gyro_plus[i].get())
                - f32::from(data.gyro_bias[i].get()))
            .abs()
                + (f32::from(data.gyro_minus[i].get()) - f32::from(data.gyro_bias[i].get())).abs();
            let accel_plus = f32::from(data.accel_endpoints[i][0].get());
            let accel_span = accel_plus - f32::from(data.accel_endpoints[i][1].get());
            if gyro_span <= 0.0 || accel_span <= 0.0 {
                return None;
            }
            // DS4 firmware already corrects gyro bias (Linux hid-playstation).
            calibration.gyroscope_scale[i] = speed.to_radians() / gyro_span;
            calibration.accelerometer_bias[i] = accel_plus - accel_span / 2.0;
            calibration.accelerometer_scale[i] = 2.0 * 9.80665 / accel_span;
        }
        Some(calibration)
    }
}

const _: () = assert!(std::mem::size_of::<FactoryCalibration>() == 41);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bluetooth_axis_order_and_invalid_calibration() {
        let valid = include_bytes!("fixtures/calibration.bin");
        let calibration = FactoryCalibration::parse(valid).unwrap();
        // Bluetooth groups positive endpoints before negative endpoints.
        let rotation = calibration.rotation([1440, 2880, 960]);
        assert!(
            rotation
                .iter()
                .all(|v| (*v - std::f32::consts::FRAC_PI_2).abs() < 0.00001)
        );
        assert_eq!(calibration.acceleration([100, -200, 50]), [0.0; 3]);
        assert!(FactoryCalibration::parse(&valid[..40]).is_none());
        let mut corrupt = *valid;
        corrupt[9] ^= 1;
        assert!(FactoryCalibration::parse(&corrupt).is_none());
        // A correctly checksummed but degenerate accelerometer span is invalid.
        corrupt = *valid;
        corrupt[25..27].copy_from_slice(&valid[23..25]);
        let crc = Report::checksum(0xa3, &corrupt[..37]);
        corrupt[37..].copy_from_slice(&crc.to_le_bytes());
        assert!(FactoryCalibration::parse(&corrupt).is_none());
    }
}
