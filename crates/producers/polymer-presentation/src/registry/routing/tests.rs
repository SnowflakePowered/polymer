use super::*;

struct Fixture {
    registry: PresentationRegistry,
    sources: [RoutingSource; 2],
    windows: Vec<WindowPresentation>,
}
impl Fixture {
    fn new() -> Self {
        let extent = Extent2D {
            width: 320,
            height: 240,
        };
        let sources = [0, 1].map(|slot| RoutingSource {
            identity: SourceIdentity {
                domain: 1,
                resource: 10 + slot as u64,
                subresource: slot as u32,
            },
            logical_slot: Some(slot),
            extent,
            placement: Some(Rect {
                x: 20 + slot as i32 * 30,
                y: -10,
                extent,
            }),
        });
        let windows = [100, 200]
            .into_iter()
            .enumerate()
            .map(|(index, window)| WindowPresentation {
                window,
                extent,
                creation_order: index as u64,
                window_order: index,
                seen: 100,
                sources: sources.to_vec(),
            })
            .collect();
        Self {
            registry: PresentationRegistry::default(),
            sources,
            windows,
        }
    }
    fn targets(&self, sources: &[RoutingSource], now: u64) -> Vec<PresentationTarget> {
        self.registry
            .route_presentations(
                PresentationTarget {
                    window: 300,
                    extent: self.sources[0].extent,
                    placement: None,
                },
                sources,
                &self.windows,
                &[0, 1],
                now,
                |_, _| None,
            )
            .1
    }
}

#[test]
fn replacement_window_observation_cannot_resurrect_an_older_matching_layout() {
    let mut fixture = Fixture::new();
    let mut replacement = fixture.windows[1].clone();
    replacement.creation_order += 10;
    replacement.seen += 1;
    for source in &mut replacement.sources {
        source.identity.resource += 100;
    }
    fixture.windows.push(replacement);
    for _ in 0..2 {
        assert!(
            fixture
                .targets(&fixture.sources, 101)
                .iter()
                .all(|target| target.window == 300)
        );
        fixture.windows.reverse();
    }
}

#[test]
fn one_shared_source_does_not_make_equal_sized_siblings_the_same_layout() {
    let mut fixture = Fixture::new();
    fixture.windows[1].sources[1].identity.resource = 99;
    assert!(
        fixture
            .targets(&fixture.sources, 100)
            .iter()
            .all(|target| target.window == 300)
    );
}

#[test]
fn equal_extents_keep_identity_placement_through_reorder_page_flip_and_resize() {
    let mut fixture = Fixture::new();
    let initial = fixture.targets(&fixture.sources, 100);
    assert_eq!(
        initial
            .iter()
            .map(|target| target.window)
            .collect::<Vec<_>>(),
        [100, 200]
    );
    assert_eq!(initial[0].placement.unwrap().x, 20);
    assert_eq!(initial[1].placement.unwrap().x, 50);
    fixture.windows.reverse();
    let reversed = [fixture.sources[1], fixture.sources[0]];
    assert_eq!(fixture.targets(&reversed, 101), initial);
    // New resource incarnations retain established logical slots; equal size
    // does not merge array layers or substitute a sibling's placement.
    for source in &mut fixture.sources {
        source.identity.resource += 100;
    }
    for window in &mut fixture.windows {
        window.sources = fixture.sources.to_vec();
    }
    let resized = Extent2D {
        width: 800,
        height: 600,
    };
    fixture.windows[0].extent = resized;
    fixture.windows[0].sources[1].placement = Some(resized.rect());
    let refreshed = fixture.targets(&fixture.sources, 102);
    assert_eq!(refreshed[0].window, 100);
    assert_eq!(refreshed[1].window, 200);
    assert_eq!(refreshed[1].extent, resized);
    assert_eq!(refreshed[1].placement, Some(resized.rect()));
}

#[test]
fn routing_requires_shared_identity_and_expires_gaps_without_stealing_title_requests() {
    let mut fixture = Fixture::new();
    let initial = fixture.targets(&fixture.sources, 100);
    let owner = PresentationTarget {
        window: 300,
        extent: fixture.sources[0].extent,
        placement: None,
    };
    let (requests, targets) = fixture.registry.route_presentations(
        owner,
        &fixture.sources,
        &fixture.windows,
        &[0, 0],
        101,
        |slot, window| (slot == 1).then_some(window == 100),
    );
    assert_eq!(requests, [(0, 1), (1, 0)]);
    assert_eq!(targets[0].window, 200);
    fixture.windows.clear();
    assert_eq!(fixture.targets(&fixture.sources, 101), initial);
    assert_eq!(fixture.targets(&fixture.sources, 340), initial);
    assert!(
        fixture
            .targets(&fixture.sources, 341)
            .iter()
            .all(|target| target.window == 300)
    );
    let unrelated = Fixture::new();
    let other_domain = unrelated.sources.map(|mut source| {
        source.identity.domain = 2;
        source
    });
    assert!(
        unrelated
            .targets(&other_domain, 100)
            .iter()
            .all(|target| target.window == 300)
    );
    unrelated.registry.forget_routing_domain(2);
    unrelated.targets(&unrelated.sources, 100);
    unrelated.registry.forget_routing_window(200);
    let (_, targets) = unrelated.registry.route_presentations(
        owner,
        &unrelated.sources,
        &[],
        &[0, 1],
        101,
        |_, _| None,
    );
    assert_eq!(targets[0].window, 100);
    assert_eq!(targets[1].window, 300);
    unrelated.registry.forget_routing_domain(1);
    let (_, targets) = unrelated.registry.route_presentations(
        owner,
        &unrelated.sources,
        &[],
        &[0, 1],
        102,
        |_, _| None,
    );
    assert!(targets.iter().all(|target| target.window == 300));
}

#[test]
fn independent_presenters_have_global_indices_that_survive_page_flips_and_gaps() {
    let fixture = Fixture::new();
    let mut sources = fixture.sources;
    sources[1].logical_slot = Some(0);
    let route = |window, source: RoutingSource, now| {
        fixture
            .registry
            .route_presentations(
                PresentationTarget {
                    window,
                    extent: source.extent,
                    placement: None,
                },
                &[source],
                &[],
                &[0, 1],
                now,
                |_, _| None,
            )
            .0
    };
    assert_eq!(route(100, sources[0], 0), [(0, 0)]);
    assert_eq!(route(200, sources[1], 0), [(1, 0)]);
    // Identical dimensions and identical local slots do not imply shared identity.
    sources[1].identity.resource += 100;
    assert_eq!(route(200, sources[1], 120), [(1, 0)]);
    sources[0].identity.resource += 100;
    assert_eq!(route(100, sources[0], 120), [(0, 0)]);
    // An explicit duplicate presentation shares the original global index.
    assert_eq!(route(300, sources[0], 121), [(0, 0)]);
    fixture.registry.forget_routing_window(100);
    assert_eq!(route(300, sources[0], 122), [(0, 0)]);
    assert_eq!(route(200, sources[1], 122), [(1, 0)]);
}

#[test]
fn title_indices_span_presenters_and_removed_sources_stop_claiming_requests() {
    let fixture = Fixture::new();
    let mut second = fixture.sources[1];
    second.logical_slot = Some(0);
    let route = |window, sources: &[RoutingSource], requested: &[usize], now, pinned: bool| {
        fixture
            .registry
            .route_presentations(
                PresentationTarget {
                    window,
                    extent: fixture.sources[0].extent,
                    placement: None,
                },
                sources,
                &[],
                requested,
                now,
                |slot, window| {
                    (!(pinned && slot == 0)).then_some(window != 0 && (!pinned || window == 100))
                },
            )
            .0
    };
    assert_eq!(
        route(100, &fixture.sources[..1], &[0, 1], 0, false),
        [(0, 0)]
    );
    assert_eq!(route(200, &[second], &[0, 1], 0, false), [(1, 0)]);
    assert_eq!(
        route(100, &fixture.sources[..1], &[0, 1], 1, false),
        [(0, 0)]
    );
    // A title request pinned to the first window takes precedence over a
    // positional request, including when the other presenter is evaluated.
    assert_eq!(route(200, &[second], &[0, 0], 2, true), [(0, 0)]);
    assert_eq!(
        route(100, &fixture.sources[..1], &[0, 0], 2, true),
        [(1, 0)]
    );
    assert!(route(100, &[], &[0, 1], 3, false).is_empty());
    assert_eq!(route(200, &[second], &[0, 1], 3, false), [(0, 0)]);
    // The absent first source does not reserve a title match, but its positional
    // slot stays reserved so that a page flip does not renumber the survivor.
    let positional = fixture
        .registry
        .route_presentations(
            PresentationTarget {
                window: 200,
                extent: second.extent,
                placement: None,
            },
            &[second],
            &[],
            &[0, 1],
            4,
            |_, _| None,
        )
        .0;
    assert_eq!(positional, [(1, 0)]);
}

#[test]
fn unnumbered_reordered_sources_reserve_both_previous_identities_before_writing_slots() {
    let fixture = Fixture::new();
    let sources = fixture.sources.map(|mut source| {
        source.logical_slot = None;
        source
    });
    let route = |sources: &[RoutingSource]| {
        fixture
            .registry
            .route_presentations(
                PresentationTarget {
                    window: 100,
                    extent: sources[0].extent,
                    placement: None,
                },
                sources,
                &[],
                &[0, 1],
                1,
                |_, _| None,
            )
            .0
    };
    assert_eq!(route(&sources), [(0, 0), (1, 1)]);
    assert_eq!(route(&[sources[1], sources[0]]), [(0, 1), (1, 0)]);
    assert_eq!(route(&sources), [(0, 0), (1, 1)]);
    let replacement = RoutingSource {
        identity: SourceIdentity {
            resource: 99,
            ..sources[1].identity
        },
        ..sources[1]
    };
    assert_eq!(route(&[replacement, sources[0]]), [(0, 1), (1, 0)]);
}

#[test]
fn duplicated_presenters_get_independent_indices_when_their_sources_diverge() {
    let fixture = Fixture::new();
    let original = fixture.sources[0];
    let replacement = RoutingSource {
        identity: SourceIdentity {
            resource: 99,
            ..original.identity
        },
        ..original
    };
    let route = |window, source: RoutingSource, now| {
        fixture
            .registry
            .route_presentations(
                PresentationTarget {
                    window,
                    extent: source.extent,
                    placement: None,
                },
                &[source],
                &[],
                &[0, 1],
                now,
                |_, _| None,
            )
            .0
    };
    assert_eq!(route(100, original, 0), [(0, 0)]);
    assert_eq!(route(200, original, 1), [(0, 0)]);
    // A local page flip must not repurpose an index still used by another
    // presenter for different content, even across a short observation gap.
    assert_eq!(route(200, replacement, 120), [(1, 0)]);
    assert_eq!(route(100, original, 121), [(0, 0)]);
    assert_eq!(route(200, replacement, 121), [(1, 0)]);
    assert_eq!(route(200, original, 122), [(0, 0)]);
}
