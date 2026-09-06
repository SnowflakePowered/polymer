use super::*;
use crate::interop::GL_TEXTURE_2D;

fn source(texture: u32, width: u32, height: u32) -> DrawCandidate {
    DrawCandidate {
        mip: 0,
        incarnation: u64::from(texture),
        orientation: (false, false),
        source_origin: (0, 0),
        allocation_extent: None,
        texture,
        target: GL_TEXTURE_2D,
        layer: 0,
        width,
        height,
        bounds: QuadBounds {
            x: 0,
            y: 0,
            width,
            height,
        },
        window_bounds: false,
        score: 0,
    }
}

#[test]
fn transport_placement_preserves_negative_origin_reflection_and_unknown_evidence() {
    use polymer_presentation::{Extent2D, producer::CapturedFrame};
    use polymer_protocol::{PRESENTATION_COORD_FRACTIONAL_BITS, PresentationMappingKind};
    let extent = Extent2D {
        width: 640,
        height: 480,
    };
    let mut source = source(1, 320, 240);
    source.window_bounds = true;
    source.bounds = QuadBounds {
        x: 20,
        y: 300,
        width: 320,
        height: 240,
    };
    source.orientation = (true, false);
    let placement = DrawCandidate::frame_placement(Some(&source), extent);
    assert_eq!((placement.bounds.x, placement.bounds.y), (20, -60));
    let polymer_ipc::Message::FrameReady(frame) = (CapturedFrame {
        stream_id: StreamId(1),
        slot: 0,
        extent,
        sync_value: 42,
        timestamp_qpc: 17,
    })
    .present(placement) else {
        unreachable!()
    };
    let transform = frame.presentation_transform;
    assert_eq!(transform.kind, PresentationMappingKind::Affine);
    assert_eq!(
        transform.upper_left.x >> PRESENTATION_COORD_FRACTIONAL_BITS,
        340
    );
    assert_eq!(
        transform.upper_right.x >> PRESENTATION_COORD_FRACTIONAL_BITS,
        20
    );
    assert_eq!(
        transform.upper_left.y >> PRESENTATION_COORD_FRACTIONAL_BITS,
        -60
    );
    source.window_bounds = false;
    assert_eq!(
        DrawCandidate::frame_placement(Some(&source), extent)
            .transform
            .kind,
        PresentationMappingKind::Unavailable,
    );
    source.window_bounds = true;
    source.bounds.x = -20;
    source.bounds.y = -10;
    let placement = DrawCandidate::frame_placement(Some(&source), extent);
    assert_eq!((placement.bounds.x, placement.bounds.y), (-20, 250));
    assert_eq!(
        QuadBounds::from_presentation_rect(placement.bounds, extent),
        Some(source.bounds)
    );
    source.bounds.y = i32::MIN;
    assert_eq!(
        DrawCandidate::frame_placement(Some(&source), extent)
            .transform
            .kind,
        PresentationMappingKind::Unavailable,
    );
}

#[test]
fn history_and_registry_distinguish_layers_contexts_and_missing_slots() {
    let registry = polymer_presentation::registry::PresentationRegistry::default();
    let mut first = WindowCaptures {
        hwnd: 10,
        last_swap: 1,
        ..Default::default()
    };
    let mut layer = source(1, 320, 240);
    layer.layer = 1;
    let selection = first
        .history
        .reconcile(vec![source(1, 320, 240), layer], true, |_| true);
    first.tracked_sources = selection.surfaces;
    first.source_slots = selection.slots;
    registry.observe(
        first.owner_id,
        first.hwnd,
        first.registry_sources(100),
        first.last_swap,
    );
    assert_eq!(
        registry.requests(first.owner_id, &[0, 1], 1, |_, _| None),
        [(0, 0), (1, 1)]
    );
    let selection = first
        .history
        .reconcile(Vec::new(), false, |source| source.layer == 1);
    first.tracked_sources = selection.surfaces;
    first.source_slots = selection.slots;
    registry.observe(first.owner_id, first.hwnd, first.registry_sources(100), 2);
    assert_eq!(
        registry.requests(first.owner_id, &[0, 1], 2, |_, _| None),
        [(1, 0)]
    );
    let second = WindowCaptures {
        hwnd: 10,
        last_swap: 2,
        tracked_sources: vec![layer],
        source_slots: vec![0],
        ..Default::default()
    };
    registry.observe(
        second.owner_id,
        second.hwnd,
        second.registry_sources(200),
        2,
    );
    assert_eq!(
        registry.requests(second.owner_id, &[0, 1, 2], 2, |_, _| None),
        [(2, 0)]
    );
}

#[test]
fn graph_surface_identity_matches_the_vulkan_producer() {
    assert_eq!(graph_surface_id(0), SurfaceId(0x5347_0000_0000_0000));
    assert_eq!(graph_surface_id(1), SurfaceId(0x5347_0000_0000_0001));
}

#[test]
fn surface_match_pins_a_slot_and_unqualified_slots_take_remaining_sources() {
    let main = 1;
    let gamepad = 2;
    let selection = [(main, 10), (gamepad, 20)];
    let resolved = polymer_presentation::registry::PresentationRegistry::resolve(
        &[0, 1],
        &selection,
        |slot, hwnd| (slot == 0).then_some(hwnd == 20),
    );
    assert_eq!(resolved[0], (0, gamepad));
    assert_eq!(resolved[1], (1, main));
}

#[test]
fn one_request_resolves_only_one_cross_window_source() {
    let main = 1;
    let gamepad = 2;
    let selection = [(main, 10), (gamepad, 20)];

    let resolved =
        polymer_presentation::registry::PresentationRegistry::resolve(&[0], &selection, |_, _| {
            None
        });

    assert_eq!(resolved, [(0, main)]);
}

#[test]
fn one_allowed_opengl_surface_excludes_sibling_rings() {
    let allowed = HashMap::from([(10, vec![graph_surface_id(0)])]);

    assert!(surface_is_selected(&allowed, 10, graph_surface_id(0)));
    assert!(!surface_is_selected(&allowed, 20, graph_surface_id(0)));
    assert!(!surface_is_selected(&allowed, 10, graph_surface_id(1)));
}

fn quad(y: i32) -> QuadBounds {
    QuadBounds {
        x: 26,
        y,
        width: 312,
        height: 233,
    }
}

#[test]
fn repeated_presented_face_does_not_assign_a_historical_source_by_draw_order() {
    // Both quads sample the current face. Equal counts cannot prove which
    // destination belongs to the historical face retained by selection history.
    let selected = [(0, source(12, 256, 192)), (1, source(11, 256, 192))];
    let quads = [(11, quad(312)), (11, quad(0))];
    assert_eq!(
        polymer_presentation::registry::PresentationRegistry::pair_placements(
            selected.iter().map(|(_, source)| source.texture),
            &quads
        ),
        vec![None, None]
    );
}

#[test]
fn unpairable_present_quads_match_by_unique_texture_only() {
    let selected = [(0, source(12, 256, 192)), (1, source(11, 256, 192))];
    // An overlay draw joined the present: counts cannot pair, and only
    // the source drawn by exactly one quad resolves.
    let quads = [(11, quad(312)), (12, quad(0)), (12, quad(100))];
    assert_eq!(
        polymer_presentation::registry::PresentationRegistry::pair_placements(
            selected.iter().map(|(_, source)| source.texture),
            &quads
        ),
        vec![None, Some(quad(312))]
    );
    assert_eq!(
        polymer_presentation::registry::PresentationRegistry::pair_placements::<_, QuadBounds>(
            selected.iter().map(|(_, source)| source.texture),
            &[]
        ),
        vec![None, None]
    );
}
