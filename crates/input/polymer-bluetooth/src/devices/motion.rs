/// Factory sensor correction, producing acceleration in m/s² and rotation in rad/s.
#[derive(Clone, Copy, Debug)]
pub struct SensorCalibration {
    pub accelerometer_bias: [f32; 3],
    pub accelerometer_scale: [f32; 3],
    pub gyroscope_bias: [f32; 3],
    pub gyroscope_scale: [f32; 3],
}

impl SensorCalibration {
    pub fn acceleration(&self, raw: [i16; 3]) -> [f32; 3] {
        std::array::from_fn(|i| {
            (f32::from(raw[i]) - self.accelerometer_bias[i]) * self.accelerometer_scale[i]
        })
    }

    pub fn rotation(&self, raw: [i16; 3]) -> [f32; 3] {
        std::array::from_fn(|i| {
            (f32::from(raw[i]) - self.gyroscope_bias[i]) * self.gyroscope_scale[i]
        })
    }
}
