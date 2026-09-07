use super::{DependencyEdge, Target};
use dashmap::DashMap;
use std::hash::Hash;

/// Generation-stamped evidence. Native resource lifetime is owned by adapters.
pub struct ObservationStore<I: Eq + Hash> {
    transaction: std::sync::Mutex<()>,
    generation: std::sync::atomic::AtomicU64,
    edges: DashMap<I, Vec<DependencyEdge<I>>>,
    // First observation, write generation, last use, and latest write order.
    // Stable selection ordering is distinct from temporal content provenance.
    targets: DashMap<I, (Target<I>, u64, u64, u64)>,
}

impl<I: Copy + Eq + Hash> Default for ObservationStore<I> {
    fn default() -> Self {
        Self {
            transaction: std::sync::Mutex::new(()),
            generation: std::sync::atomic::AtomicU64::new(0),
            edges: DashMap::new(),
            targets: DashMap::new(),
        }
    }
}

impl<I: Copy + Eq + Hash> ObservationStore<I> {
    /// Serializes an adapter's complete submitted batch with presentation's
    /// snapshot/clear and native metadata updates. Individual store methods do
    /// not acquire this guard: adapters hold it across their whole transaction.
    /// Acquire it before taking native descriptor or command-store guards.
    pub fn transaction(&self) -> std::sync::MutexGuard<'_, ()> {
        self.transaction
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// One shared presentation boundary: advance the recording generation,
    /// snapshot the live evidence, and clear it in single-frame tracking mode.
    /// Callers hold the transaction while resolving native metadata.
    pub fn present<S>(&self, persistent: bool) -> super::GraphSnapshot<I, S> {
        let now = self
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let graph = self.graph(now);
        if !persistent {
            self.clear();
        }
        graph
    }

    pub fn record(&self, edge: DependencyEdge<I>) {
        let rendered_source = self.observe_use_before(edge.source, edge.generation, edge.order);
        if rendered_source && edge.source != edge.destination && edge.exact() {
            self.target(
                Target {
                    id: edge.destination,
                    // A transfer carries rendered content but is not a draw.
                    // Its destination bounds remain available on the edge.
                    score: 0,
                    order: edge.order,
                },
                edge.generation,
            );
        }
        let mut edges = self.edges.entry(edge.destination).or_default();
        if let Some(old) = edges.iter_mut().find(|old| {
            old.source == edge.source
                && old.evidence == edge.evidence
                && old.whole_source == edge.whole_source
                && old.mapping == edge.mapping
                && old.coverage == edge.coverage
        }) {
            if (edge.generation, edge.order) >= (old.generation, old.order) {
                *old = edge;
            }
        } else {
            edges.push(edge);
        }
    }

    /// Refresh known rendered content without inventing a write for a sampled
    /// asset or changing the generation used to compare draw-coverage scores.
    pub fn observe_use(&self, id: I, generation: u64) -> bool {
        self.observe_use_before(id, generation, u64::MAX)
    }

    fn observe_use_before(&self, id: I, generation: u64, order: u64) -> bool {
        self.targets.get_mut(&id).is_some_and(|mut target| {
            // A delayed batch must not borrow provenance from a later write.
            // Its dependency remains usable as explicit copy evidence, but we
            // cannot establish what rendered content existed at that time.
            if (generation, order) < (target.1, target.3) {
                return false;
            }
            target.2 = target.2.max(generation);
            true
        })
    }

    pub fn target(&self, target: Target<I>, generation: u64) {
        self.targets
            .entry(target.id)
            .and_modify(|(old, seen, used, write_order)| {
                if generation < *seen {
                    return;
                }
                old.score = if *seen == generation {
                    *write_order = (*write_order).max(target.order);
                    target.score.max(old.score)
                } else {
                    *write_order = target.order;
                    target.score
                };
                *seen = generation;
                *used = (*used).max(generation);
            })
            .or_insert((target, generation, generation, target.order));
    }

    /// Prunes evidence more than eight generations behind this clock. At a
    /// presentation boundary adapters advance the recording generation first,
    /// retaining the eight completed generations `[generation - 8, generation)`.
    /// Queries during recording also include observations in the current one.
    pub fn graph<S>(&self, generation: u64) -> super::GraphSnapshot<I, S> {
        let horizon = generation.saturating_sub(8);
        self.targets.retain(|_, (_, _, used, _)| *used >= horizon);
        self.edges.retain(|_, edges| {
            edges.retain(|edge| edge.generation >= horizon);
            !edges.is_empty()
        });
        let mut targets = self.targets.iter().map(|entry| entry.0).collect::<Vec<_>>();
        targets.sort_by_key(|target| target.order);
        super::GraphSnapshot {
            targets,
            edges: self
                .edges
                .iter()
                .map(|entry| (*entry.key(), entry.value().clone()))
                .collect(),
            ..Default::default()
        }
    }

    /// Flat diagnostics for observation tests; production selection consumes
    /// the grouped graph directly.
    #[cfg(test)]
    pub fn snapshot(&self, generation: u64) -> (Vec<Target<I>>, Vec<DependencyEdge<I>>) {
        let graph = self.graph::<()>(generation);
        (graph.targets, graph.edges.into_values().flatten().collect())
    }

    pub fn forget_matching(&self, matches: impl Fn(I) -> bool) {
        self.targets.retain(|id, _| !matches(*id));
        self.edges.retain(|id, edges| {
            if matches(*id) {
                return false;
            }
            edges.retain(|edge| !matches(edge.source));
            !edges.is_empty()
        });
    }

    pub fn forget(&self, id: I) {
        self.targets.remove(&id);
        self.edges.remove(&id);
        self.edges.retain(|_, edges| {
            edges.retain(|edge| edge.source != id);
            !edges.is_empty()
        });
    }

    pub fn clear(&self) {
        self.edges.clear();
        self.targets.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transferred_content_does_not_invent_or_inflate_draw_coverage() {
        let store = ObservationStore::<u32>::default();
        store.target(
            Target {
                id: 1,
                score: 0,
                order: 0,
            },
            0,
        );
        let copy = DependencyEdge {
            evidence: super::super::Evidence::Copy,
            whole_source: true,
            coverage: Some(
                crate::Extent2D {
                    width: 1024,
                    height: 1024,
                }
                .rect(),
            ),
            order: 1,
            ..DependencyEdge::sampled(1, 2, 0)
        };
        store.record(copy);
        assert_eq!(
            store
                .snapshot(0)
                .0
                .iter()
                .find(|target| target.id == 2)
                .unwrap()
                .score,
            0
        );
        store.target(
            Target {
                id: 2,
                score: 320 * 240,
                order: 2,
            },
            0,
        );
        store.record(DependencyEdge { order: 3, ..copy });
        assert_eq!(
            store
                .snapshot(0)
                .0
                .iter()
                .find(|target| target.id == 2)
                .unwrap()
                .score,
            320 * 240
        );
        assert_eq!(store.snapshot(0).1[0].coverage, copy.coverage);
    }

    #[test]
    fn presentation_boundary_retains_eight_completed_generations_and_refreshes_replay() {
        let store = ObservationStore::<u64>::default();
        for generation in 0..8 {
            store.target(
                Target {
                    id: generation,
                    score: 100,
                    order: generation,
                },
                generation,
            );
            store.record(DependencyEdge::sampled(generation, 100, generation));
        }
        let (targets, edges) = store.snapshot(8);
        assert_eq!(targets.len(), 8);
        assert_eq!(edges.len(), 8);
        // Replaying the oldest source before the next present keeps only that
        // source fresh; the other old source still expires at its boundary.
        store.record(DependencyEdge::sampled(0, 100, 8));
        let (targets, edges) = store.snapshot(10);
        assert_eq!(
            targets.iter().map(|target| target.id).collect::<Vec<_>>(),
            [0, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(edges.len(), 7);
        assert!(edges.iter().all(|edge| edge.generation >= 2));
        let (targets, edges) = store.snapshot(16);
        assert_eq!(
            targets.iter().map(|target| target.id).collect::<Vec<_>>(),
            [0]
        );
        assert_eq!(edges, [DependencyEdge::sampled(0, 100, 8)]);
        let (targets, edges) = store.snapshot(17);
        assert!(targets.is_empty() && edges.is_empty());
    }

    #[test]
    fn static_copied_content_stays_known_while_sampled_and_dies_with_its_resource() {
        let store = ObservationStore::<u32>::default();
        store.target(
            Target {
                id: 1,
                score: 100,
                order: 0,
            },
            0,
        );
        let copy = DependencyEdge {
            evidence: super::super::Evidence::Copy,
            whole_source: true,
            ..DependencyEdge::sampled(1, 2, 0)
        };
        store.record(copy);
        store.record(DependencyEdge {
            source: 3,
            destination: 4,
            ..copy
        });
        for generation in 1..=20 {
            store.record(DependencyEdge::sampled(2, 0, generation));
            store.record(DependencyEdge::sampled(4, 0, generation));
            let (targets, _) = store.snapshot(generation);
            assert!(targets.iter().any(|target| target.id == 2));
            assert!(!targets.iter().any(|target| target.id == 4));
        }
        store.target(
            Target {
                id: 2,
                score: 16,
                order: 20,
            },
            20,
        );
        assert_eq!(store.snapshot(20).0[0].score, 16);
        store.forget(2);
        store.record(DependencyEdge::sampled(2, 0, 21));
        assert!(!store.snapshot(21).0.iter().any(|target| target.id == 2));
    }

    #[test]
    fn late_duplicates_cannot_regress_evidence_or_target_recency() {
        let store = ObservationStore::<u32>::default();
        let current = DependencyEdge {
            order: 20,
            ..DependencyEdge::sampled(1, 2, 10)
        };
        store.record(current);
        store.target(
            Target {
                id: 2,
                score: 640 * 480,
                order: 20,
            },
            10,
        );
        // A late batch from an older generation and a lower-order replay in
        // the same generation must not displace the newer dependency.
        store.record(DependencyEdge {
            generation: 9,
            order: 99,
            ..current
        });
        store.record(DependencyEdge {
            order: 19,
            ..current
        });
        store.target(
            Target {
                id: 2,
                score: 1,
                order: 99,
            },
            9,
        );
        let (targets, edges) = store.snapshot(18);
        assert_eq!(edges, [current]);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].score, 640 * 480);
        assert_eq!(targets[0].order, 20);
        // A real replay refreshes freshness, but a new generation's smaller
        // draw coverage must replace the old score rather than accumulate it.
        store.record(DependencyEdge {
            generation: 18,
            order: 30,
            ..current
        });
        store.target(
            Target {
                id: 2,
                score: 32 * 48,
                order: 30,
            },
            18,
        );
        let (targets, edges) = store.snapshot(19);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].generation, 18);
        assert_eq!(targets[0].score, 32 * 48);
        let (targets, edges) = store.snapshot(27);
        assert!(targets.is_empty() && edges.is_empty());
    }

    #[test]
    fn late_copy_cannot_inherit_rendered_content_from_a_future_source_write() {
        let store = ObservationStore::<u32>::default();
        store.target(
            Target {
                id: 1,
                score: 80,
                order: 5,
            },
            10,
        );
        store.target(
            Target {
                id: 1,
                score: 100,
                order: 10,
            },
            10,
        );
        let copy = DependencyEdge {
            evidence: super::super::Evidence::Copy,
            whole_source: true,
            ..DependencyEdge::sampled(1, 2, 9)
        };
        store.record(copy);
        let (targets, edges) = store.snapshot(10);
        assert_eq!(edges, [copy]);
        assert!(
            !targets.iter().any(|target| target.id == 2),
            "a copy predating known rendered content cannot establish a rendered destination"
        );
        store.record(DependencyEdge {
            generation: 10,
            order: 9,
            ..copy
        });
        let (targets, _) = store.snapshot(10);
        assert!(!targets.iter().any(|target| target.id == 2));
        assert_eq!(
            targets[0].order, 5,
            "write freshness must preserve stable first-observation order"
        );
        store.record(DependencyEdge {
            generation: 10,
            order: 11,
            ..copy
        });
        assert!(store.snapshot(10).0.iter().any(|target| target.id == 2));
    }
}
