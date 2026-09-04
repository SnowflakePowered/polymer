use crate::{BackendError, Calibration};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdRumbleChannel {
    pub low_frequency_hz: f32,
    pub low_amplitude: f32,
    pub high_frequency_hz: f32,
    pub high_amplitude: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdRumbleFrame {
    pub left: HdRumbleChannel,
    pub right: HdRumbleChannel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HdRumbleFormat {
    StereoF32 { sample_rate: u32 },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Feedback {
    Rumble {
        low_frequency: u16,
        high_frequency: u16,
        duration_ms: u32,
    },
    HdRumble(HdRumbleFrame),
    PlayerLed {
        index: u8,
    },
    HomeLed {
        brightness: u8,
    },
    Recenter,
    Calibrate(Calibration),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedbackClass {
    Actuation,
    Managed,
    Latched,
    Transient,
}

impl Feedback {
    pub const fn class(&self) -> FeedbackClass {
        match self {
            Self::Rumble { .. } | Self::HdRumble(_) => FeedbackClass::Actuation,
            Self::PlayerLed { .. } => FeedbackClass::Managed,
            Self::Calibrate(_) | Self::HomeLed { .. } => FeedbackClass::Latched,
            Self::Recenter => FeedbackClass::Transient,
        }
    }
}

pub trait HdRumbleSink: Send {
    fn submit(&mut self, samples: &[f32]) -> Result<(), BackendError>;
}
