//! Shared, bounded presentation-source selection.

use crate::{DependencyEdge, Evidence, Extent2D, GraphSnapshot, Rect};
use std::collections::{HashMap, HashSet};
mod components;
mod geometry;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectedSurface<I> {
    pub surface: I,
    pub selection: SurfaceSelection,
}

/// Selection geometry carried unchanged from the graph through history to capture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceSelection {
    pub target_extent: Extent2D,
    pub exact: bool,
    pub source_rect: Rect,
    pub presentation_rect: Option<Rect>,
    pub flip_x: bool,
    pub flip_y: bool,
    pub evidence: Evidence,
}

impl Default for SurfaceSelection {
    fn default() -> Self {
        Self {
            target_extent: Extent2D::default(),
            source_rect: Rect::default(),
            presentation_rect: None,
            exact: false,
            flip_x: false,
            flip_y: false,
            evidence: Evidence::SampledDraw,
        }
    }
}

pub struct Selection<I> {
    pub surfaces: Vec<SelectedSurface<I>>,
    /// Complete explicit presentation evidence; history must not pad this
    /// selection with stale sources from an earlier layout.
    pub exact: bool,
}

pub struct PresentationSelector;

impl PresentationSelector {
    /// Backbuffer content comes from the newest observed presentation generation.
    /// Within that generation, retain the strongest clipped raster coverage.
    pub fn observed_content(
        extent: Extent2D,
        observations: impl IntoIterator<Item = (u64, Rect)>,
    ) -> Option<Rect> {
        observations
            .into_iter()
            .filter_map(|(generation, rect)| {
                rect.intersect(extent.rect()).map(|rect| (generation, rect))
            })
            .max_by_key(|(generation, rect)| (*generation, rect.extent.area()))
            .map(|(_, rect)| rect)
    }

    pub fn select<I: Copy + Eq + std::hash::Hash>(
        graph: &GraphSnapshot<I>,
        presented: I,
        limit: usize,
        traverse: bool,
    ) -> Selection<I> {
        let mut view = View::new(graph, presented);
        let mut roots = view.edges.get(&presented).cloned().unwrap_or_default();
        roots.sort_by_key(|edge| view.order(edge.source));
        if !traverse {
            let mut coverage_scores = HashMap::<I, u64>::new();
            for edge in &roots {
                if let Some(coverage) = edge.coverage {
                    coverage_scores
                        .entry(edge.source)
                        .and_modify(|score| *score = (*score).max(coverage.extent.area()))
                        .or_insert(coverage.extent.area());
                }
            }
            for (source, score) in coverage_scores {
                let order = view.order(source).2;
                view.scores.insert(source, (score, order));
            }
        }
        let exact = roots
            .iter()
            .filter(|edge| edge.source != presented && edge.exact())
            .map(|edge| edge.source)
            .collect();
        let whole = roots
            .iter()
            .filter(|edge| {
                edge.exact()
                    && view.extent(edge.source).is_some_and(|extent| {
                        edge.mapping
                            .map_or(edge.whole_source, |mapping| mapping.source == extent.rect())
                    })
            })
            .map(|edge| edge.source)
            .collect();
        let mut selected = view.expand(exact, limit, traverse, whole);
        let trusted = !selected.is_empty();
        if !trusted {
            let roots = roots
                .iter()
                .map(|edge| edge.source)
                .filter(|id| *id != presented && view.rendered.contains(id) && view.plausible(*id))
                .collect();
            selected = view.expand(roots, limit, traverse, HashSet::new());
        }
        if !traverse {
            selected.sort_by_key(|id| {
                std::cmp::Reverse(
                    roots
                        .iter()
                        .filter(|edge| edge.source == *id)
                        .map(|edge| {
                            (
                                edge.mapping.is_some(),
                                edge.coverage.map_or(0, |coverage| coverage.extent.area()),
                                view.extent(*id).map_or(0, Extent2D::area),
                            )
                        })
                        .max(),
                )
            });
        }
        let mut surfaces = selected
            .into_iter()
            .filter_map(|id| view.resolve(presented, id))
            .filter(|source| {
                source.selection.evidence != Evidence::SampledDraw || view.plausible(source.surface)
            })
            .collect::<Vec<_>>();
        let uncertain = surfaces
            .iter()
            .filter(|source| source.selection.evidence == Evidence::SampledDraw)
            .map(|source| source.surface)
            .collect();
        let cohort = view
            .cohort(uncertain, limit)
            .into_iter()
            .collect::<HashSet<_>>();
        surfaces.retain(|source| {
            source.selection.evidence != Evidence::SampledDraw || cohort.contains(&source.surface)
        });
        if !traverse {
            surfaces.truncate(limit);
        }
        surfaces.sort_by_key(|source| {
            (
                u32::MAX - source.selection.source_rect.extent.height,
                u32::MAX - source.selection.source_rect.extent.width,
                view.order(source.surface).2,
            )
        });
        surfaces.truncate(limit);
        if surfaces.is_empty()
            && limit != 0
            && let Some(extent) = view.extent(presented)
        {
            let rect = Self::observed_content(
                extent,
                roots
                    .iter()
                    .filter_map(|edge| edge.coverage.map(|coverage| (edge.generation, coverage))),
            )
            .unwrap_or(extent.rect());
            return Selection {
                surfaces: vec![SelectedSurface {
                    surface: presented,
                    selection: SurfaceSelection {
                        target_extent: extent,
                        exact: false,
                        source_rect: rect,
                        presentation_rect: Some(rect),
                        flip_x: false,
                        flip_y: false,
                        evidence: Evidence::SampledDraw,
                    },
                }],
                exact: false,
            };
        }
        let exact = trusted
            && !surfaces.is_empty()
            && surfaces.iter().all(|source| {
                source.selection.evidence != Evidence::SampledDraw
                    && source.selection.presentation_rect.is_some()
            });
        for source in &mut surfaces {
            source.selection.exact = exact;
        }
        Selection { surfaces, exact }
    }
}

struct View<'a, I: Copy + Eq + std::hash::Hash> {
    graph: &'a GraphSnapshot<I>,
    scores: HashMap<I, (u64, u64)>,
    rendered: HashSet<I>,
    edges: HashMap<I, Vec<DependencyEdge<I>>>,
    components: components::Components<I>,
}

impl<'a, I: Copy + Eq + std::hash::Hash> View<'a, I> {
    fn new(graph: &'a GraphSnapshot<I>, presented: I) -> Self {
        let scores = graph
            .targets
            .iter()
            .copied()
            .map(|target| (target.id, (target.score, target.order)))
            .collect();
        let mut result = Self {
            graph,
            scores,
            rendered: graph
                .targets
                .iter()
                .copied()
                .map(|target| target.id)
                .collect(),
            edges: HashMap::new(),
            components: components::Components::default(),
        };
        let mut pending = graph
            .targets
            .iter()
            .copied()
            .map(|target| target.id)
            .collect::<Vec<_>>();
        pending.push(presented);
        while let Some(id) = pending.pop() {
            if result.edges.contains_key(&id) {
                continue;
            }
            let edges = Self::current_inputs(
                graph
                    .edges
                    .get(&id)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|edge| edge.evidence != Evidence::Clear)
                    .collect(),
            );
            pending.extend(edges.iter().map(|edge| edge.source));
            result.edges.insert(id, edges);
        }
        // Explicit transfers may present resources without a raster write.
        // Give these sources a stable observation order without inventing
        // rendered-content provenance or draw coverage.
        for edge in result.edges.values().flatten() {
            if !result.rendered.contains(&edge.source) {
                result
                    .scores
                    .entry(edge.source)
                    .and_modify(|score| score.1 = score.1.min(edge.order))
                    .or_insert((0, edge.order));
            }
        }
        result.components = components::Components::new(&result.edges);
        result
    }

    /// Keep all conflicting mappings from the latest source observation, but
    /// do not let older exact evidence override a newer sampled binding.
    fn current_inputs(mut edges: Vec<DependencyEdge<I>>) -> Vec<DependencyEdge<I>> {
        let mut newest = HashMap::<I, (u64, bool)>::new();
        for edge in &edges {
            let rank = (edge.generation, edge.exact());
            newest
                .entry(edge.source)
                .and_modify(|current| *current = (*current).max(rank))
                .or_insert(rank);
        }
        edges.retain(|edge| newest[&edge.source] == (edge.generation, edge.exact()));
        edges
    }

    fn extent(&self, id: I) -> Option<Extent2D> {
        self.graph
            .surfaces
            .get(&id)
            .copied()
            .filter(|extent| extent.area() != 0)
    }

    fn plausible(&self, id: I) -> bool {
        self.extent(id)
            .is_some_and(|extent| extent.width >= 32 && extent.height >= 32)
    }

    fn order(&self, id: I) -> (u32, u32, u64) {
        let extent = self.extent(id).unwrap_or_default();
        (
            u32::MAX - extent.height,
            u32::MAX - extent.width,
            self.scores.get(&id).map_or(u64::MAX, |value| value.1),
        )
    }

    fn reachable(&self, start: I, needle: I) -> bool {
        let mut pending = vec![start];
        let mut visited = HashSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            for edge in self.edges.get(&id).into_iter().flatten() {
                if edge.source == needle {
                    return true;
                }
                pending.push(edge.source);
            }
        }
        false
    }

    fn expand(&self, mut roots: Vec<I>, limit: usize, traverse: bool, whole: HashSet<I>) -> Vec<I> {
        if limit == 0 {
            return Vec::new();
        }
        let mut seen = HashSet::new();
        roots.retain(|id| self.extent(*id).is_some() && seen.insert(*id));
        self.collapse_flips(&mut roots);
        if !traverse {
            return roots;
        }
        // Mutually reachable roots belong to one strongly connected component.
        // Retain the first representative, rather than eliminating all roots.
        let snapshot = roots.clone();
        roots.retain(|id| {
            !snapshot.iter().any(|other| {
                other != id
                    && self.reachable(*other, *id)
                    && (!self.reachable(*id, *other)
                        || snapshot.iter().position(|item| item == other)
                            < snapshot.iter().position(|item| item == id))
            })
        });
        let whole = whole
            .into_iter()
            .filter_map(|id| self.components.index.get(&id).copied())
            .collect::<HashSet<_>>();
        let initial = roots
            .iter()
            .map(|id| self.components.index[id])
            .collect::<HashSet<_>>();
        let mut expanded = HashSet::new();
        loop {
            let next = roots.iter().enumerate().find_map(|(index, id)| {
                let component = self.components.index[id];
                if expanded.contains(&component)
                    || (roots.len() >= limit
                        && !(whole.contains(&component) && initial.contains(&component)))
                {
                    return None;
                }
                self.decomposition(*id).map(|inputs| (index, inputs))
            });
            let Some((index, inputs)) = next else {
                break;
            };
            expanded.insert(self.components.index[&roots.remove(index)]);
            roots.splice(index..index, inputs);
            let mut seen = HashSet::new();
            roots.retain(|id| seen.insert(self.components.index[id]));
        }
        roots
    }

    fn decomposition(&self, id: I) -> Option<Vec<I>> {
        let component = self.components.index[&id];
        let mut inputs = self.components.members[component]
            .iter()
            .flat_map(|member| self.edges.get(member).into_iter().flatten())
            .filter(|edge| edge.exact() || self.plausible(edge.source))
            .filter(|edge| edge.exact() || self.rendered.contains(&edge.source))
            // Never descend within a strongly connected component.
            .map(|edge| edge.source)
            .filter(|input| self.components.index[input] != component)
            .filter(|input| self.extent(*input).is_some())
            .collect::<Vec<_>>();
        let mut seen = HashSet::new();
        inputs.retain(|input| seen.insert(self.components.index[input]));
        self.collapse_flips(&mut inputs);
        if inputs.is_empty() {
            return None;
        }
        let area = |id| self.extent(id).map_or(0, Extent2D::area);
        if inputs.len() == 1 && area(inputs[0]) >= area(id) {
            return None;
        }
        Some(inputs)
    }

    fn collapse_flips(&self, roots: &mut Vec<I>) {
        let mut removed = HashSet::new();
        for (index, &first) in roots.iter().enumerate() {
            for &second in &roots[index + 1..] {
                if removed.contains(&first) || removed.contains(&second) {
                    continue;
                }
                if let Some(remove) = self.older_face(first, second) {
                    removed.insert(remove);
                }
            }
        }
        roots.retain(|id| !removed.contains(id));
    }

    fn older_face(&self, first: I, second: I) -> Option<I> {
        if self.graph.resource(first) == self.graph.resource(second)
            || self.graph.subresource(first) != self.graph.subresource(second)
            || self.extent(first)? != self.extent(second)?
        {
            return None;
        }
        let first_edges = self.edges.get(&first)?;
        let second_edges = self.edges.get(&second)?;
        let shared = first_edges
            .iter()
            .flat_map(|a| {
                second_edges
                    .iter()
                    .filter(move |b| a.source == b.source)
                    .map(move |b| ((a.generation, a.order), (b.generation, b.order)))
            })
            .collect::<Vec<_>>();
        let newest_first = shared.iter().map(|pair| pair.0).max()?;
        let newest_second = shared.iter().map(|pair| pair.1).max()?;
        Some(if newest_first >= newest_second {
            second
        } else {
            first
        })
    }

    fn cohort(&self, candidates: Vec<I>, limit: usize) -> Vec<I> {
        let mut distinct = HashSet::new();
        let candidates = candidates
            .into_iter()
            .filter(|id| distinct.insert(*id))
            .collect::<Vec<_>>();
        let mut cohorts = HashMap::<Extent2D, (usize, u64)>::new();
        for id in &candidates {
            if let Some(extent) = self.extent(*id) {
                let cohort = cohorts.entry(extent).or_default();
                cohort.0 += 1;
                cohort.1 = cohort.1.max(self.scores.get(id).map_or(0, |value| value.0));
            }
        }
        let Some(best) = cohorts
            .into_iter()
            .max_by_key(|(extent, (count, score))| {
                (
                    (*count).min(limit),
                    *score,
                    extent.area(),
                    extent.height,
                    extent.width,
                )
            })
            .map(|value| value.0)
        else {
            return Vec::new();
        };
        candidates
            .into_iter()
            .filter(|id| self.extent(*id) == Some(best))
            .collect()
    }
}
