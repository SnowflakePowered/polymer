use super::protocol::Report;
use crate::devices::SensorCalibration;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, byteorder::little_endian::I16};

#[derive(Clone, Copy, Debug)]
pub enum GyroLayout {
    Split,
    Interleaved,
}

impl GyroLayout {
    fn span(self, endpoints: &[I16; 6], bias: f32, axis: usize) -> f32 {
        let endpoint = |index: usize| f32::from(endpoints[index].get());
        match self {
            Self::Split => (endpoint(axis) - bias).abs() + (endpoint(axis + 3) - bias).abs(),
            Self::Interleaved => endpoint(axis * 2) - endpoint(axis * 2 + 1),
        }
    }

    fn bias(self, bias: f32) -> f32 {
        match self {
            Self::Split => 0.0,
            Self::Interleaved => bias,
        }
    }
}

/// Sony feature report 0x05, including report ID and Bluetooth CRC.
#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct FactoryCalibration {
    id: u8,
    gyro_bias: [I16; 3],
    gyro_endpoints: [I16; 6],
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

    pub fn parse(bytes: &[u8], layout: GyroLayout) -> Option<SensorCalibration> {
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
            let bias = f32::from(data.gyro_bias[i].get());
            let gyro_span = layout.span(&data.gyro_endpoints, bias, i);
            let accel_plus = f32::from(data.accel_endpoints[i][0].get());
            let accel_span = accel_plus - f32::from(data.accel_endpoints[i][1].get());
            if gyro_span <= 0.0 || accel_span <= 0.0 {
                return None;
            }
            calibration.gyroscope_bias[i] = layout.bias(bias);
            calibration.gyroscope_scale[i] = speed.to_radians() / gyro_span;
            calibration.accelerometer_bias[i] = accel_plus - accel_span / 2.0;
            calibration.accelerometer_scale[i] = 2.0 * 9.80665 / accel_span;
        }
        Some(calibration)
    }
}

const _: () = assert!(std::mem::size_of::<FactoryCalibration>() == 41);
