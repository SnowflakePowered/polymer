use crate::{DependencyEdge, Evidence, Extent2D, PresentationSelector, RegionMapping, Target};

use crate::testing::CpuBackend as Fixture;

#[test]
fn explicit_unrendered_sources_use_observation_order_before_limiting() {
    let mut graph = Fixture::default();
    let extent = Extent2D {
        width: 64,
        height: 64,
    };
    for id in [0, 1, 2] {
        graph.graph.surfaces.insert(id, extent);
    }
    for id in [2, 1] {
        graph
            .graph
            .edges
            .entry(0)
            .or_default()
            .push(DependencyEdge {
                order: id as u64,
                evidence: Evidence::Copy,
                mapping: Some(RegionMapping {
                    source: extent.rect(),
                    destination: extent.rect(),
                    flip_x: false,
                    flip_y: false,
                }),
                ..DependencyEdge::sampled(id, 0, 1)
            });
    }
    for traverse in [false, true] {
        for limit in [1, 2] {
            let selected = PresentationSelector::select(&graph.graph, 0, limit, traverse);
            assert_eq!(
                selected
                    .surfaces
                    .iter()
                    .map(|source| source.surface)
                    .collect::<Vec<_>>(),
                (1..=limit as u32).collect::<Vec<_>>()
            );
            assert!(selected.exact);
        }
    }
    // Ordering metadata must not turn uploaded assets into sampled screen roots.
    for edge in graph.graph.edges.get_mut(&0).unwrap() {
        edge.evidence = Evidence::SampledDraw;
        edge.mapping = None;
    }
    assert_eq!(graph.selected(2), vec![0]);
}

#[test]
fn allocation_clear_does_not_override_padded_backbuffer_draw_bounds() {
    let mut graph = Fixture::default();
    graph.target(0, 1024, 1024, 320 * 240);
    let content = Extent2D {
        width: 320,
        height: 240,
    }
    .rect();
    graph.graph.edges.insert(
        0,
        vec![
            DependencyEdge {
                evidence: Evidence::Clear,
                coverage: Some(
                    Extent2D {
                        width: 1024,
                        height: 1024,
                    }
                    .rect(),
                ),
                ..DependencyEdge::sampled(0, 0, 1)
            },
            DependencyEdge {
                coverage: Some(content),
                ..DependencyEdge::sampled(0, 0, 1)
            },
        ],
    );
    for traverse in [false, true] {
        let selection = PresentationSelector::select(&graph.graph, 0, 1, traverse);
        assert_eq!(selection.surfaces[0].surface, 0);
        assert_eq!(selection.surfaces[0].selection.source_rect, content);
        assert!(!selection.exact);
    }
}

#[test]
fn selection_does_not_recreate_provenance_rejected_by_observation_order() {
    let mut graph = Fixture::default();
    for id in [0, 1, 2] {
        graph.graph.surfaces.insert(
            id,
            Extent2D {
                width: 320,
                height: 240,
            },
        );
    }
    graph.observations.target(
        Target {
            id: 1,
            score: 320 * 240,
            order: 10,
        },
        10,
    );
    graph.observations.record(DependencyEdge {
        evidence: Evidence::Copy,
        whole_source: true,
        ..DependencyEdge::sampled(1, 2, 9)
    });
    graph.observations.record(DependencyEdge::sampled(2, 0, 10));
    graph.set_observations(graph.observations.graph(10));
    assert_eq!(graph.selected(1), [0]);
}

#[test]
fn uploaded_sampled_texture_is_not_a_screen_without_a_raster_write() {
    let mut graph = Fixture::default();
    graph.target(0, 320, 240, 0);
    for id in [1, 2] {
        graph.graph.surfaces.insert(
            id,
            Extent2D {
                width: 256,
                height: 256,
            },
        );
    }
    graph.edge(1, 2, 1, true);
    graph.edge(2, 0, 1, false);
    for traverse in [false, true] {
        let selected = PresentationSelector::select(&graph.graph, 0, 1, traverse);
        assert_eq!(selected.surfaces[0].surface, 0);
    }
    // Drawing into the same resource establishes the missing evidence; a
    // matching-size upload chain should still stop at this render target.
    graph.target(2, 256, 256, 256 * 256);
    assert_eq!(graph.selected(1), [2]);
    graph.graph.targets.retain(|target| target.id == 0);
    graph.graph.edges.get_mut(&0).unwrap()[0].evidence = Evidence::Blit;
    graph.graph.edges.get_mut(&0).unwrap()[0].whole_source = true;
    assert_eq!(graph.selected(1), [2]);
}

#[test]
fn backbuffer_content_tracks_smaller_layouts_and_expires_with_observations() {
    let mut graph = Fixture::default();
    graph.target(0, 640, 480, 0);
    let mut raster = DependencyEdge::sampled(0, 0, 1);
    raster.coverage = Some(graph.graph.surfaces[&0].rect());
    graph.observations.record(raster);
    raster.generation = 2;
    raster.coverage = Some(crate::Rect {
        x: -20,
        y: 40,
        extent: Extent2D {
            width: 320,
            height: 200,
        },
    });
    graph.observations.record(raster);
    graph.set_observations(graph.observations.graph(2));
    let selection = PresentationSelector::select(&graph.graph, 0, 1, true);
    assert_eq!(
        selection.surfaces[0].selection.source_rect,
        crate::Rect {
            x: 0,
            y: 40,
            extent: Extent2D {
                width: 300,
                height: 200
            },
        }
    );
    graph.set_observations(graph.observations.graph(11));
    let selection = PresentationSelector::select(&graph.graph, 0, 1, true);
    assert_eq!(
        selection.surfaces[0].selection.source_rect,
        graph.graph.surfaces[&0].rect()
    );
    assert!(!selection.exact);
}

#[test]
fn newer_sampled_observations_supersede_old_exact_root_evidence() {
    let mut graph = Fixture::default();
    graph.target(1, 1024, 1024, 100);
    graph.target(2, 320, 240, 200);
    graph.target(3, 320, 240, 200);
    graph.edge(1, 0, 1, true);
    graph.edge(1, 0, 2, false);
    graph.edge(2, 0, 2, false);
    graph.edge(3, 0, 2, false);
    assert_eq!(graph.selected(2), [2, 3]);
    // Exact evidence in the same generation takes precedence over the sampled
    // observation, while unrelated sources remain available to other roots.
    graph.edge(1, 0, 2, true);
    assert_eq!(graph.selected(2), [1]);
}

#[test]
fn exhausted_path_budget_cannot_hide_a_conflicting_placement() {
    let mut graph = Fixture::default();
    graph.target(1, 320, 240, 100);
    let mut exact = DependencyEdge::sampled(1, 0, 1);
    exact.evidence = Evidence::Blit;
    exact.mapping = Some(RegionMapping {
        source: graph.graph.surfaces[&1].rect(),
        destination: graph.graph.surfaces[&1].rect(),
        flip_x: false,
        flip_y: false,
    });
    let mut roots = vec![exact];
    // Spend the traversal budget on unrelated branches after finding the
    // first mapping. The final conflicting observation must not be ignored.
    roots.extend((2..4096).map(|id| DependencyEdge::sampled(id, 0, 1)));
    exact.mapping.as_mut().unwrap().destination.x = 100;
    roots.push(exact);
    graph.graph.edges.insert(0, roots);
    let selected = PresentationSelector::select(&graph.graph, 0, 1, false);
    assert_eq!(selected.surfaces.len(), 1);
    assert!(!selected.exact);
    assert_eq!(selected.surfaces[0].selection.presentation_rect, None);
    assert_eq!(
        selected.surfaces[0].selection.evidence,
        Evidence::SampledDraw
    );
    // If loss of proof makes a tiny source ineligible, validation must still
    // fall back to observed backbuffer content rather than returning nothing.
    graph.graph.surfaces.insert(
        1,
        Extent2D {
            width: 16,
            height: 16,
        },
    );
    graph.target(0, 640, 480, 0);
    let content = crate::Rect {
        x: 10,
        y: 20,
        extent: Extent2D {
            width: 320,
            height: 240,
        },
    };
    graph.graph.edges.get_mut(&0).unwrap()[0].coverage = Some(content);
    let selected = PresentationSelector::select(&graph.graph, 0, 1, false);
    assert_eq!(selected.surfaces.len(), 1);
    assert_eq!(selected.surfaces[0].surface, 0);
    assert_eq!(selected.surfaces[0].selection.source_rect, content);
    assert!(!selected.exact);
}

#[test]
fn composite_decomposes_even_with_one_requested_screen() {
    let mut graph = Fixture::default();
    graph.target(30, 1024, 1024, 100);
    graph.target(10, 400, 240, 100);
    graph.target(20, 320, 240, 100);
    graph.edge(30, 0, 1, true);
    graph.edge(10, 30, 1, true);
    graph.edge(20, 30, 1, true);
    assert_eq!(graph.selected(1), [10]);
    assert_eq!(graph.selected(2), [10, 20]);
}

#[test]
fn exact_composite_does_not_promote_uncertain_inputs_or_require_copy_source_writes() {
    let mut graph = Fixture::default();
    graph.target(30, 1024, 1024, 100);
    graph.target(10, 320, 240, 100);
    graph.target(11, 320, 240, 100);
    graph.target(20, 900, 800, 200);
    graph.edge(30, 0, 1, true);
    for source in [10, 11, 20] {
        graph.edge(source, 30, 1, false);
    }
    assert_eq!(graph.selected(2), [10, 11]);
    // Explicit transfer evidence preserves an unequal screen without a
    // recorded render-target write, even alongside uncertain cohort members.
    graph.graph.surfaces.insert(
        40,
        Extent2D {
            width: 16,
            height: 64,
        },
    );
    graph.edge(40, 30, 1, true);
    assert_eq!(graph.selected(3), [10, 11, 40]);
    let selection = PresentationSelector::select(&graph.graph, 0, 3, true);
    assert!(!selection.exact);
    assert_eq!(
        selection.surfaces[0].selection.evidence,
        Evidence::SampledDraw
    );
    assert_eq!(selection.surfaces[2].selection.evidence, Evidence::Blit);
}

#[test]
fn cohort_counts_distinct_sources_and_uses_draw_coverage() {
    let mut graph = Fixture::default();
    for id in [10, 11] {
        graph.target(id, 512, 384, 100);
        graph.edge(id, 0, 1, false);
    }
    graph.edge(10, 0, 2, false);
    graph.target(99, 32, 32, 10_000);
    graph.edge(99, 0, 1, false);
    assert_eq!(graph.selected(2), [10, 11]);
    assert_eq!(graph.selected(1), [99]);
}

#[test]
fn direct_input_is_not_counted_twice_and_copy_chains_stop() {
    let mut graph = Fixture::default();
    for id in [30, 10, 20, 5] {
        graph.target(id, 256, 192, 100);
    }
    graph.edge(30, 0, 1, false);
    graph.edge(10, 0, 1, false);
    graph.edge(10, 30, 1, false);
    graph.edge(20, 30, 1, false);
    graph.edge(5, 10, 1, false);
    assert_eq!(graph.selected(2), [10, 20]);
}

#[test]
fn page_flips_choose_fresh_face_without_merging_array_layers() {
    let mut graph = Fixture::default();
    for id in [23, 31, 40, 41] {
        graph.target(id, 240, 320, 100);
        graph.edge(id, 0, 1, true);
    }
    graph.edge(5, 23, 7, false);
    graph.edge(5, 31, 9, false);
    graph.edge(6, 40, 9, false);
    graph.edge(6, 41, 9, false);
    graph.graph.resources.insert(40, (40, 0, 0));
    graph.graph.resources.insert(41, (40, 0, 1));
    assert_eq!(graph.selected(4), [31, 40, 41]);
}

#[test]
fn independent_array_layers_are_not_a_single_input_scaling_chain() {
    let mut graph = Fixture::default();
    graph.target(3, 320, 240, 100);
    for id in [1, 2] {
        graph.target(id, 640, 480, 100);
        graph.graph.resources.insert(id, (1, 0, id - 1));
        graph.edge(id, 3, 1, true);
    }
    graph.edge(3, 0, 1, true);
    assert_eq!(graph.selected(2), [1, 2]);
}

#[test]
fn cycle_representative_preserves_whole_composite_evidence_at_the_output_limit() {
    let mut graph = Fixture::default();
    for id in [1, 2] {
        graph.target(id, 640, 480, 100);
    }
    for id in [10, 20] {
        graph.target(id, 320, 240, 100);
        graph.edge(id, 2, 1, false);
    }
    graph.edge(1, 2, 1, false);
    graph.edge(2, 1, 1, false);
    let crop = Extent2D {
        width: 320,
        height: 480,
    }
    .rect();
    graph
        .graph
        .edges
        .entry(0)
        .or_default()
        .push(DependencyEdge {
            evidence: Evidence::Blit,
            mapping: Some(RegionMapping {
                source: crop,
                destination: crop,
                flip_x: false,
                flip_y: false,
            }),
            ..DependencyEdge::sampled(1, 0, 1)
        });
    graph.edge(2, 0, 1, true);
    assert_eq!(graph.selected(1), [10]);
}

#[test]
fn decomposing_a_composite_does_not_emit_two_members_of_one_cycle() {
    let mut graph = Fixture::default();
    graph.target(3, 640, 480, 100);
    for id in [1, 2] {
        graph.target(id, 320, 240, 100);
        graph.edge(id, 3, 1, true);
    }
    graph.edge(1, 2, 1, false);
    graph.edge(2, 1, 1, false);
    graph.edge(3, 0, 1, true);
    assert_eq!(graph.selected(2), [1]);
}

#[test]
fn cycles_retain_a_representative_and_terminate() {
    let mut graph = Fixture::default();
    for id in [1, 2] {
        graph.target(id, 240, 320, 100);
        graph.edge(id, 0, 1, true);
    }
    graph.edge(1, 2, 1, false);
    graph.edge(2, 1, 1, false);
    assert_eq!(graph.selected(8), [1]);
}

#[test]
fn padded_source_crop_is_distinct_from_scaled_placement() {
    let mut graph = Fixture::default();
    graph.target(1, 2048, 2048, 100);
    graph.target(2, 1920, 1080, 100);
    let crop = crate::Rect {
        x: 0,
        y: 64,
        extent: Extent2D {
            width: 1920,
            height: 1080,
        },
    };
    let destination = crate::Rect {
        x: 10,
        y: 20,
        extent: Extent2D {
            width: 960,
            height: 540,
        },
    };
    let mut edge = DependencyEdge::sampled(1, 0, 1);
    edge.evidence = Evidence::RecoveredQuad;
    edge.mapping = Some(RegionMapping {
        source: crop,
        destination,
        flip_x: false,
        flip_y: true,
    });
    graph.graph.edges.insert(0, vec![edge]);
    let result = PresentationSelector::select(&graph.graph, 0, 1, true);
    assert_eq!(result.surfaces[0].selection.source_rect, crop);
    assert_eq!(
        result.surfaces[0].selection.presentation_rect,
        Some(destination)
    );
    assert!(result.surfaces[0].selection.flip_y);
    let mut other = edge;
    other.mapping.as_mut().unwrap().destination.x += 100;
    graph.graph.edges.get_mut(&0).unwrap().push(other);
    assert_eq!(
        PresentationSelector::select(&graph.graph, 0, 1, true).surfaces[0]
            .selection
            .presentation_rect,
        None
    );
}

#[test]
fn reflected_multihop_coverage_preserves_crop_and_requires_every_mapping() {
    let mut graph = Fixture::default();
    graph.target(0, 640, 480, 0);
    graph.target(1, 512, 256, 100);
    graph.target(2, 1000, 600, 100);
    let rect = |x, y, width, height| crate::Rect {
        x,
        y,
        extent: Extent2D { width, height },
    };
    graph.graph.edges.insert(
        2,
        vec![DependencyEdge {
            evidence: Evidence::Blit,
            mapping: Some(RegionMapping {
                source: rect(10, 20, 400, 200),
                destination: rect(100, 50, 800, 400),
                flip_x: true,
                flip_y: false,
            }),
            ..DependencyEdge::sampled(1, 2, 1)
        }],
    );
    graph.graph.edges.insert(
        0,
        vec![DependencyEdge {
            evidence: Evidence::Blit,
            mapping: Some(RegionMapping {
                source: rect(300, 50, 400, 400),
                destination: rect(20, 30, 200, 200),
                flip_x: true,
                flip_y: false,
            }),
            coverage: Some(rect(70, 30, 100, 200)),
            ..DependencyEdge::sampled(2, 0, 1)
        }],
    );
    let selection = PresentationSelector::select(&graph.graph, 0, 2, true);
    assert!(selection.exact);
    assert_eq!(selection.surfaces.len(), 1);
    let source = selection.surfaces[0];
    assert_eq!(source.surface, 1);
    assert_eq!(source.selection.source_rect, rect(160, 20, 100, 200));
    assert_eq!(
        source.selection.presentation_rect,
        Some(rect(70, 30, 100, 200))
    );
    assert!(!source.selection.flip_x && !source.selection.flip_y);
    // Destination raster coverage cannot recover a missing sampled-source map.
    let input = &mut graph.graph.edges.get_mut(&2).unwrap()[0];
    input.mapping = None;
    input.evidence = Evidence::SampledDraw;
    input.coverage = Some(rect(100, 50, 800, 400));
    let selection = PresentationSelector::select(&graph.graph, 0, 2, true);
    assert!(!selection.exact);
    assert_eq!(
        selection.surfaces[0].selection.source_rect,
        graph.graph.surfaces[&1].rect()
    );
    assert_eq!(selection.surfaces[0].selection.presentation_rect, None);
}

#[test]
fn intermediate_clipping_maps_back_to_the_source_crop() {
    let mut graph = Fixture::default();
    graph.target(1, 400, 240, 100);
    graph.target(2, 1280, 720, 100);
    let mut input = DependencyEdge::sampled(1, 2, 1);
    input.evidence = Evidence::RecoveredQuad;
    input.mapping = Some(RegionMapping {
        source: graph.graph.surfaces[&1].rect(),
        destination: crate::Rect {
            x: 200,
            y: 100,
            extent: graph.graph.surfaces[&1],
        },
        flip_x: false,
        flip_y: false,
    });
    let mut output = DependencyEdge::sampled(2, 0, 1);
    output.evidence = Evidence::Blit;
    output.mapping = Some(RegionMapping {
        source: crate::Rect {
            x: 300,
            y: 0,
            extent: Extent2D {
                width: 600,
                height: 720,
            },
        },
        destination: Extent2D {
            width: 300,
            height: 360,
        }
        .rect(),
        flip_x: false,
        flip_y: false,
    });
    graph.graph.edges.insert(2, vec![input]);
    graph.graph.edges.insert(0, vec![output]);
    // A cropped composite does not force decomposition after capacity is met.
    assert_eq!(
        PresentationSelector::select(&graph.graph, 0, 1, true).surfaces[0].surface,
        2
    );
    let result = PresentationSelector::select(&graph.graph, 0, 2, true);
    assert_eq!(
        result.surfaces[0].selection.source_rect,
        crate::Rect {
            x: 100,
            y: 0,
            extent: Extent2D {
                width: 300,
                height: 240
            }
        }
    );
    assert_eq!(
        result.surfaces[0].selection.presentation_rect,
        Some(crate::Rect {
            x: 0,
            y: 50,
            extent: Extent2D {
                width: 150,
                height: 120
            }
        })
    );
}

#[test]
fn missing_metadata_is_not_a_candidate() {
    let mut graph = Fixture::default();
    graph.target(1, 400, 240, 100);
    graph.target(2, 400, 240, 100);
    graph.graph.surfaces.remove(&2);
    graph.edge(1, 0, 0, false);
    graph.edge(2, 0, 0, false);
    graph.edge(1, 0, 1, false);
    assert_eq!(graph.selected(2), [1]);
}

#[test]
fn cycle_expands_external_inputs_from_every_member_once() {
    let mut graph = Fixture::default();
    for id in [1, 2] {
        graph.target(id, 1024, 1024, 100);
    }
    for id in [10, 20] {
        graph.target(id, 320, 240, 100);
    }
    graph.edge(1, 0, 1, true);
    graph.edge(1, 2, 1, false);
    graph.edge(2, 1, 1, false);
    graph.edge(10, 2, 1, false);
    graph.edge(20, 2, 1, false);
    assert_eq!(graph.selected(2), [10, 20]);
}

#[test]
fn reflected_mapping_clips_to_the_presented_extent_and_limits_use_crop_size() {
    let mut graph = Fixture::default();
    graph.target(0, 100, 100, 0);
    graph.target(1, 512, 512, 100);
    let mut edge = DependencyEdge::sampled(1, 0, 1);
    edge.evidence = Evidence::Blit;
    edge.mapping = Some(RegionMapping {
        source: Extent2D {
            width: 200,
            height: 200,
        }
        .rect(),
        destination: crate::Rect {
            x: -50,
            y: 0,
            extent: Extent2D {
                width: 100,
                height: 100,
            },
        },
        flip_x: true,
        flip_y: false,
    });
    graph.graph.edges.entry(0).or_default().push(edge);
    let selection = PresentationSelector::select(&graph.graph, 0, 1, true);
    let source = selection.surfaces[0];
    assert_eq!(
        source.selection.source_rect,
        Extent2D {
            width: 100,
            height: 200
        }
        .rect()
    );
    assert_eq!(
        source.selection.presentation_rect,
        Some(
            Extent2D {
                width: 50,
                height: 100
            }
            .rect()
        )
    );
    assert!(source.selection.flip_x);
    graph.target(2, 256, 256, 100);
    graph.edge(2, 0, 1, true);
    assert_eq!(graph.selected(1), [2]);
}
