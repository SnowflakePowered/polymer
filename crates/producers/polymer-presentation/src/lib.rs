//! API-independent presentation observations and capture policy.
//!
//! Graph adapters own native resources and may query their APIs through
//! [`Surface`]. Selection never performs capture or synchronization.

pub mod geometry;
pub mod graph;
pub mod history;
pub mod observation;
#[cfg(all(feature = "producer", windows))]
pub mod producer;
pub mod registry;
pub mod scheduling;
pub mod selection;

pub use geometry::{Extent2D, Rect, RegionMapping};
pub use graph::{
    DependencyEdge, Evidence, GraphSnapshot, ObservationStore, PresentationGraph, Surface, Target,
};
pub use selection::{PresentationSelector, SelectedSurface, Selection};
