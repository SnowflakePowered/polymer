use std::time::{Duration, Instant};

/// Connection rejection waits for a source change; transient failures retry later.
#[derive(Default)]
pub enum ConnectionRetry {
    #[default]
    Ready,
    After(Instant),
    Parked,
}
impl ConnectionRetry {
    pub fn begin(&mut self) -> bool {
        let now = Instant::now();
        match self {
            Self::Parked => false,
            Self::After(deadline) if now < *deadline => false,
            _ => {
                *self = Self::After(now + Duration::from_secs(1));
                true
            }
        }
    }
    pub fn source_changed(&mut self) {
        if matches!(self, Self::Parked) {
            *self = Self::Ready;
        }
    }
}
