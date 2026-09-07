use std::time::{Duration, Instant};

/// Connection rejection parks the current capture generation; transient failures retry later.
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
}
