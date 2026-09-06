//! Native-resource-free observation storage and borrowed resource views.

use crate::{Extent2D, Rect, RegionMapping};
use std::{collections::HashMap, hash::Hash};
mod store;
pub use store::ObservationStore;

pub trait Surface {
    /// Called at most once per surface during each selection. Unknown metadata
    /// excludes a source; implementations must uphold their API's thread rules.
    fn extent(&self) -> Option<Extent2D>;
}

impl Surface for Extent2D {
    fn extent(&self) -> Option<Extent2D> {
        Some(*self)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Evidence {
    /// Establishes written content, not sampled dependency or visible draw bounds.
    Clear,
    SampledDraw,
    Resolve,
    Copy,
    Blit,
    RecoveredQuad,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DependencyEdge<I> {
    pub source: I,
    pub destination: I,
    pub generation: u64,
    pub order: u64,
    pub evidence: Evidence,
    /// Observation established that the entire source was presented, even
    /// when destination geometry was not recorded.
    pub whole_source: bool,
    pub mapping: Option<RegionMapping>,
    pub coverage: Option<Rect>,
}

impl<I: Copy> DependencyEdge<I> {
    pub fn sampled(source: I, destination: I, generation: u64) -> Self {
        Self {
            source,
            destination,
            generation,
            order: generation,
            evidence: Evidence::SampledDraw,
            whole_source: false,
            mapping: None,
            coverage: None,
        }
    }

    pub fn exact(&self) -> bool {
        !matches!(self.evidence, Evidence::SampledDraw | Evidence::Clear)
            && (self.mapping.is_some() || self.whole_source)
    }
}

#[derive(Clone, Copy, Debug)]
/// A surface with observed rendered content. Adapters report raster writes;
/// the store can propagate that evidence through exact transfers. Uploaded
/// assets remain discoverable through edges but are not uncertain screen roots.
pub struct Target<I> {
    pub id: I,
    pub score: u64,
    pub order: u64,
}

pub trait PresentationGraph {
    type Id: Copy + Eq + Hash;
    fn surface(&self, id: Self::Id) -> Option<&dyn Surface>;
    /// Validated rendered-content evidence, including temporally valid exact
    /// transfers. ObservationStore supplies this propagation; the selector
    /// cannot infer write chronology from an unstamped target list alone.
    fn targets(&self) -> impl Iterator<Item = Target<Self::Id>>;
    fn inputs(&self, destination: Self::Id) -> impl Iterator<Item = DependencyEdge<Self::Id>>;
    /// Resource identity without mip/layer. IDs remain scoped to this view.
    fn resource(&self, id: Self::Id) -> Self::Id {
        id
    }
    fn subresource(&self, _id: Self::Id) -> (u32, u32) {
        (0, 0)
    }
}

/// A selection-scoped view owning stable resource records. `S` can be a native
/// resource wrapper whose Surface implementation queries the graphics API.
pub struct GraphSnapshot<I, S> {
    pub surfaces: HashMap<I, S>,
    pub targets: Vec<Target<I>>,
    pub edges: HashMap<I, Vec<DependencyEdge<I>>>,
    pub resources: HashMap<I, (I, u32, u32)>,
}

impl<I, S> Default for GraphSnapshot<I, S> {
    fn default() -> Self {
        Self {
            surfaces: HashMap::new(),
            targets: Vec::new(),
            edges: HashMap::new(),
            resources: HashMap::new(),
        }
    }
}

impl<I: Copy + Eq + Hash, S: Surface> PresentationGraph for GraphSnapshot<I, S> {
    type Id = I;
    fn surface(&self, id: I) -> Option<&dyn Surface> {
        self.surfaces
            .get(&id)
            .map(|surface| surface as &dyn Surface)
    }
    fn targets(&self) -> impl Iterator<Item = Target<I>> {
        self.targets.iter().copied()
    }
    fn inputs(&self, destination: I) -> impl Iterator<Item = DependencyEdge<I>> {
        self.edges.get(&destination).into_iter().flatten().copied()
    }
    fn resource(&self, id: I) -> I {
        self.resources.get(&id).map_or(id, |value| value.0)
    }
    fn subresource(&self, id: I) -> (u32, u32) {
        self.resources
            .get(&id)
            .map_or((0, 0), |value| (value.1, value.2))
    }
}
