//! Fixed-resource virtual presentation policy. Native objects remain backend-owned.
mod matching;
pub use matching::{
    AssignmentRegistry, Candidate, FixedSurfaceBinding, ProvisionalBinding, SourceIdentity,
    SourceRegistration, WindowIdentity,
};

#[cfg(feature = "producer")]
mod backend;
#[cfg(feature = "producer")]
mod controller;
#[cfg(all(feature = "producer", windows))]
mod maintenance;
mod pacing;
#[cfg(feature = "producer")]
mod retirement;
#[cfg(all(feature = "producer", windows))]
pub use maintenance::{DxgiMaintenance, PacingSource, RetiringResource};
#[cfg(all(feature = "producer", windows))]
mod latency;
#[cfg(all(feature = "producer", windows))]
pub use latency::FrameLatency;
#[cfg(feature = "producer")]
mod state;
pub use pacing::AcquireDeadline;
#[cfg(feature = "producer")]
pub use {backend::*, controller::*, retirement::*, state::*};

mod owner;
pub use owner::{OwnerCall, OwnerCell, OwnerUnavailable};
#[cfg(feature = "producer")]
mod budget;
