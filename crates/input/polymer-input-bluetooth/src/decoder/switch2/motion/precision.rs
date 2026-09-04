use super::{GRAVITY_METERS_PER_SECOND_SQUARED, NativeMotionHeader};
use zerocopy::{
    FromBytes, Immutable, KnownLayout, Unaligned,
    byteorder::little_endian::{I16, I32},
};

pub(super) const TIMING_CLASS: u8 = 0x0c;

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct OrientationLane {
    control: u8,
    payload: [u8; 3],
}

impl OrientationLane {
    fn selector(&self) -> u8 {
        self.control & 0b11
    }

    fn coordinate(&self) -> f32 {
        let raw = u32::from(self.payload[0])
            | (u32::from(self.payload[1]) << 8)
            | (u32::from(self.payload[2]) << 16);
        (raw as i32 - 0x80_0000) as f32 / 0x80_0000 as f32
    }
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct PrecisionMotionBody {
    orientation: [OrientationLane; 3],
    acceleration: [I32; 3],
    temperature: I16,
}

impl PrecisionMotionBody {
    fn has_valid_selectors(&self) -> bool {
        self.orientation[0].control & !0b11 == 0
            && self.orientation[1].control & !0b11 == 0
            && self.orientation[2].control & !0b01 == 0
    }
}

pub(super) struct PrecisionMotion<'a> {
    pub header: &'a NativeMotionHeader,
    body: &'a PrecisionMotionBody,
}

impl<'a> PrecisionMotion<'a> {
    pub fn parse(header: &'a NativeMotionHeader, body: &'a [u8]) -> Option<Self> {
        (header.timing_class() == TIMING_CLASS)
            .then(|| PrecisionMotionBody::ref_from_bytes(body).ok())
            .flatten()
            .filter(|body| body.has_valid_selectors())
            .map(|body| Self { header, body })
    }

    pub fn orientation(&self) -> [f32; 4] {
        let face = usize::from(self.body.orientation[0].selector());
        let folded: [f32; 3] =
            std::array::from_fn(|index| self.body.orientation[index].coordinate());
        let first_selector = f32::from(self.body.orientation[1].selector());
        let second_selector = f32::from(self.body.orientation[2].selector());
        let coordinates = [
            (folded[0] + first_selector.mul_add(2.0, -3.0)) / 4.0,
            (folded[1] + second_selector.mul_add(2.0, -1.0)) / 2.0,
            folded[2],
        ];
        let mut orientation = [0.0_f32; 4];
        orientation[face] = 1.0;
        for (coordinate, value) in coordinates.into_iter().enumerate() {
            orientation[(face + coordinate + 1) & 3] = value;
        }
        let norm = orientation
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        orientation.map(|value| value / norm)
    }

    pub fn filtered_accelerometer(&self) -> [f32; 3] {
        std::array::from_fn(|index| {
            self.body.acceleration[index].get() as f32 / 65536.0 / 4096.0
                * GRAVITY_METERS_PER_SECOND_SQUARED
        })
    }

    pub fn temperature_celsius(&self) -> f32 {
        f32::from(self.body.temperature.get()) / 128.0 + 25.0
    }
}
