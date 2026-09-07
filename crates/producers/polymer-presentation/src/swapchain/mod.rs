//! Fixed-resource virtual presentation policy. Native objects remain backend-owned.
mod matching;
pub use matching::{
    AssignmentRegistry, Candidate, FixedSurfaceBinding, ProvisionalBinding, SourceIdentity,
    SourceRegistration, WindowIdentity,
};

#[cfg(all(feature = "producer", windows))]
mod backend;
#[cfg(all(feature = "producer", windows))]
mod controller;
#[cfg(all(feature = "producer", windows))]
mod maintenance;
mod pacing;
#[cfg(all(feature = "producer", windows))]
mod retirement;
#[cfg(all(feature = "producer", windows))]
pub use maintenance::{DxgiMaintenance, PacingSource, RetiringResource};
#[cfg(all(feature = "producer", windows))]
mod latency;
#[cfg(all(feature = "producer", windows))]
pub use latency::FrameLatency;
#[cfg(all(feature = "producer", windows))]
mod state;
pub use pacing::AcquireDeadline;
#[cfg(all(feature = "producer", windows))]
pub use {backend::*, controller::*, retirement::*, state::*};

mod owner;
pub use owner::{OwnerCall, OwnerCell, OwnerUnavailable};
#[cfg(all(feature = "producer", windows))]
mod budget;
