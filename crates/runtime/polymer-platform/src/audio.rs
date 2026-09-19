#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NormalizedVolume(f32);

impl NormalizedVolume {
    pub fn new(value: f32) -> Option<Self> {
        value.is_finite().then_some(Self(value.clamp(0.0, 1.0)))
    }

    pub const fn get(self) -> f32 {
        self.0
    }
}

impl Default for NormalizedVolume {
    fn default() -> Self {
        Self(1.0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioState {
    pub available: bool,
    pub volume: NormalizedVolume,
    pub muted: bool,
    pub active_sessions: usize,
}
