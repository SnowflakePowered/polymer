use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub enum AcquireDeadline {
    Nonblocking,
    Until(Instant),
}
impl AcquireDeadline {
    pub fn after(timeout: Duration) -> Self {
        if timeout.is_zero() {
            return Self::Nonblocking;
        }
        Self::Until(
            Instant::now()
                .checked_add(timeout)
                .unwrap_or_else(Instant::now),
        )
    }
    pub fn expired(self) -> bool {
        match self {
            Self::Nonblocking => true,
            Self::Until(deadline) => Instant::now() >= deadline,
        }
    }
    pub(crate) fn pause(self) {
        if let Self::Until(deadline) = self {
            std::thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(1)),
            );
        }
    }
}
