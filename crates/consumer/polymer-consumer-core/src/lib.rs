//! Consumer scheduling, latest-frame selection, and stream lifecycle.
#![forbid(unsafe_code)]

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameSelection {
    Ordered,
    LatestAvailable,
}
