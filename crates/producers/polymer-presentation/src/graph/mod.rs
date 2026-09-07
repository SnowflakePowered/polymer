//! Native-resource-free observation storage and borrowed resource views.

use crate::{Extent2D, Rect, RegionMapping};
use std::{collections::HashMap, hash::Hash};
mod store;
pub use store::ObservationStore;

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

/// Selection-scoped normalized metadata. Native resource ownership stays in the adapter.
pub struct GraphSnapshot<I> {
    pub surfaces: HashMap<I, Extent2D>,
    pub targets: Vec<Target<I>>,
    pub edges: HashMap<I, Vec<DependencyEdge<I>>>,
    pub resources: HashMap<I, (I, u32, u32)>,
}

impl<I> Default for GraphSnapshot<I> {
    fn default() -> Self {
        Self {
            surfaces: HashMap::new(),
            targets: Vec::new(),
            edges: HashMap::new(),
            resources: HashMap::new(),
        }
    }
}

impl<I: Copy + Eq + Hash> GraphSnapshot<I> {
    pub fn observation_list(self) -> (Vec<Target<I>>, Vec<DependencyEdge<I>>) {
        (self.targets, self.edges.into_values().flatten().collect())
    }

    pub fn resolve(
        &mut self,
        presented: I,
        mut surface: impl FnMut(I) -> Option<(Extent2D, (I, u32, u32))>,
    ) {
        let ids = self
            .targets
            .iter()
            .map(|target| target.id)
            .chain(
                self.edges
                    .values()
                    .flatten()
                    .flat_map(|edge| [edge.source, edge.destination]),
            )
            .chain([presented])
            .collect::<std::collections::HashSet<_>>();
        for id in ids {
            if let Some((source, resource)) = surface(id) {
                self.surfaces.insert(id, source);
                self.resources.insert(id, resource);
            }
        }
    }
}

impl<I: Copy + Eq + Hash> GraphSnapshot<I> {
    pub fn resource(&self, id: I) -> I {
        self.resources.get(&id).map_or(id, |value| value.0)
    }
    pub fn subresource(&self, id: I) -> (u32, u32) {
        self.resources
            .get(&id)
            .map_or((0, 0), |value| (value.1, value.2))
    }
}
