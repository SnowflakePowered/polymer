use super::{SelectedSurface, SurfaceSelection, View};
use crate::{DependencyEdge, Evidence, Rect};
use std::collections::HashSet;

impl<I: Copy + Eq + std::hash::Hash> View<'_, I> {
    pub(super) fn resolve(&self, presented: I, selected: I) -> Option<SelectedSurface<I>> {
        let extent = self.extent(selected)?;
        let mut paths = Vec::new();
        let mut budget = 4096;
        self.paths(
            presented,
            selected,
            &mut HashSet::new(),
            &mut Vec::new(),
            &mut paths,
            &mut budget,
        );
        // The budget may reach zero in a completed recursive call, causing
        // its parent's loop to stop before visiting a conflicting sibling.
        if budget == 0 {
            paths.push(Vec::new());
        }
        // An exact presentation of a composite does not prove that every
        // sampled dependency is itself a screen. Retain the weakest evidence
        // along every supporting path, including incomplete traversals.
        let proven = !paths.is_empty()
            && paths
                .iter()
                .all(|path| !path.is_empty() && path.iter().all(DependencyEdge::exact));
        let evidence = if proven {
            paths
                .iter()
                .flatten()
                .map(|edge| edge.evidence)
                .min()
                .unwrap_or(Evidence::SampledDraw)
        } else {
            Evidence::SampledDraw
        };
        let mapped = paths
            .iter()
            .map(|path| self.map_path(path, extent.rect()))
            .collect::<Option<Vec<_>>>();
        let mut result = SelectedSurface {
            surface: selected,
            selection: SurfaceSelection {
                target_extent: self.extent(presented).unwrap_or_default(),
                exact: false,
                source_rect: extent.rect(),
                presentation_rect: None,
                flip_x: false,
                flip_y: false,
                evidence,
            },
        };
        if let Some(mapped) = mapped
            && let Some(first) = mapped
                .first()
                .copied()
                .filter(|first| mapped.iter().all(|item| item == first))
        {
            result.selection.source_rect = first.0;
            result.selection.presentation_rect = Some(first.1);
            result.selection.flip_x = first.2;
            result.selection.flip_y = first.3;
        }
        Some(result)
    }

    fn paths(
        &self,
        node: I,
        selected: I,
        visited: &mut HashSet<I>,
        path: &mut Vec<DependencyEdge<I>>,
        paths: &mut Vec<Vec<DependencyEdge<I>>>,
        budget: &mut usize,
    ) {
        // An exponential graph must not stall a hooked present. An incomplete
        // traversal is represented by an unknown mapping, never trusted.
        if *budget == 0 {
            paths.push(Vec::new());
            return;
        }
        *budget -= 1;
        if paths.len() > 64 {
            return;
        }
        if paths.len() == 64 || path.len() >= 64 {
            paths.push(Vec::new());
            return;
        }
        if node == selected {
            paths.push(path.clone());
            return;
        }
        if !visited.insert(node) {
            return;
        }
        let edges = self.edges.get(&node).into_iter().flatten();
        for edge in edges {
            if paths.len() > 64 || *budget == 0 {
                break;
            }
            path.push(*edge);
            self.paths(edge.source, selected, visited, path, paths, budget);
            path.pop();
        }
        visited.remove(&node);
    }

    fn map_path(&self, path: &[DependencyEdge<I>], full: Rect) -> Option<(Rect, Rect, bool, bool)> {
        let mut source = path.last()?.mapping?.source.intersect(full)?;
        let mut destination = source;
        let (mut flip_x, mut flip_y) = (false, false);
        let mut prefix = Vec::<crate::RegionMapping>::new();
        for edge in path.iter().rev() {
            let mapping = edge.mapping?;
            let clipped = destination.intersect(mapping.source)?;
            if clipped != destination {
                let mut original = clipped;
                for previous in prefix.iter().rev() {
                    original = previous.inverse().map(original)?;
                }
                source = source.intersect(original)?;
                destination = clipped;
            }
            destination = mapping.map(destination)?;
            if let Some(coverage) = edge.coverage {
                let clipped = destination.intersect(coverage)?;
                let mut original = mapping.inverse().map(clipped)?;
                for previous in prefix.iter().rev() {
                    original = previous.inverse().map(original)?;
                }
                source = source.intersect(original)?;
                destination = clipped;
            }
            flip_x ^= mapping.flip_x;
            flip_y ^= mapping.flip_y;
            prefix.push(mapping);
            if let Some(extent) = self.extent(edge.destination) {
                let clipped = destination.intersect(extent.rect())?;
                if clipped != destination {
                    let mut original = clipped;
                    for previous in prefix.iter().rev() {
                        original = previous.inverse().map(original)?;
                    }
                    source = source.intersect(original)?;
                    destination = clipped;
                }
            }
        }
        Some((source, destination, flip_x, flip_y))
    }
}
