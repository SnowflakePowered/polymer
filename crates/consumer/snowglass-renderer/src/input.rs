use crate::{CompositingPlan, MappedPointer, Presentation, ReceivedSurface};
use snowglass_input::mouse::{
    PointerInput, PointerPhase, PointerSource,
    layout::{
        AffineMapping, Extent, OutsideBehavior, Point, Rect, SurfaceLayout, VerticalOrder,
        aspect_fit, vertical_surface_layout,
    },
};
#[cfg(test)]
use snowglass_ipc::Message;
use snowglass_protocol::{FrameReady, PRESENTATION_COORD_FRACTIONAL_BITS, PresentationMappingKind};
use std::time::Instant;

pub trait PointerEventSink {
    fn send_pointer(&self, connection: u64, event: PointerInput) -> bool;
}

#[derive(Clone, Copy)]
pub struct PointerSendEvent {
    pub count: u64,
    pub phase: PointerPhase,
    pub surface_id: u64,
    pub x: i32,
    pub y: i32,
    pub coordinate_width: u32,
    pub coordinate_height: u32,
    pub normalized_x: f64,
    pub normalized_y: f64,
    pub sent_at: Instant,
}

pub struct PointerRoutingState {
    cursor: Point,
    captured_connection: Option<u64>,
    buttons: u32,
    sequence: u32,
    started: Instant,
    sent_count: u64,
    last_sent: Option<PointerSendEvent>,
    logged_route: bool,
    logged_button_rejection: bool,
}

impl PointerRoutingState {
    pub fn new(started: Instant) -> Self {
        Self {
            cursor: Point::default(),
            captured_connection: None,
            buttons: 0,
            sequence: 0,
            started,
            sent_count: 0,
            last_sent: None,
            logged_route: false,
            logged_button_rejection: false,
        }
    }

    pub fn captured_connection(&self) -> Option<u64> {
        self.captured_connection
    }

    pub fn last_sent(&self) -> Option<PointerSendEvent> {
        self.last_sent
    }

    pub fn set_cursor(&mut self, x: f64, y: f64) {
        self.cursor = Point::new(x, y);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn send(
        &mut self,
        phase: PointerPhase,
        button: u32,
        wheel_horizontal: i32,
        wheel_vertical: i32,
        presentation: &Presentation,
        plan: &CompositingPlan,
        sender: &impl PointerEventSink,
    ) -> bool {
        let captured = self.captured_connection;
        let Some(destination) = map_pointer(presentation, plan, self.cursor, captured) else {
            if phase == PointerPhase::ButtonDown && !self.logged_button_rejection {
                self.logged_button_rejection = true;
                let mapped = presentation
                    .surfaces()
                    .iter()
                    .filter(|surface| {
                        surface.frame.presentation_transform.kind
                            != PresentationMappingKind::Unavailable
                    })
                    .count();
                eprintln!(
                    "snowglass: pointer button was not routed at ({:.1}, {:.1}); displayed surfaces={}, mapped surfaces={}, pointer targets={}",
                    self.cursor.x,
                    self.cursor.y,
                    presentation.surfaces().len(),
                    mapped,
                    presentation.surfaces().len(),
                );
            }
            return false;
        };

        let bit = pointer_button_bit(button);
        match phase {
            PointerPhase::ButtonDown => {
                self.captured_connection = Some(destination.connection);
                self.buttons |= bit;
            }
            PointerPhase::ButtonUp => self.buttons &= !bit,
            PointerPhase::Cancel => self.buttons = 0,
            PointerPhase::Move | PointerPhase::Wheel => {}
        }
        self.sequence = self.sequence.wrapping_add(1);
        let width = destination.frame.presentation_width.max(1);
        let height = destination.frame.presentation_height.max(1);
        let x = destination
            .position
            .x
            .round()
            .clamp(0.0, f64::from(width.saturating_sub(1))) as i32;
        let y = destination
            .position
            .y
            .round()
            .clamp(0.0, f64::from(height.saturating_sub(1))) as i32;
        let sent = sender.send_pointer(
            destination.connection,
            PointerInput {
                surface_id: destination.surface_id,
                target_window: destination.target_window,
                frame_number: destination.frame.frame_number,
                source: PointerSource::Mouse,
                phase,
                pointer_id: 0,
                x,
                y,
                coordinate_width: width,
                coordinate_height: height,
                button,
                buttons: self.buttons,
                wheel_horizontal,
                wheel_vertical,
                timestamp: self.started.elapsed().as_millis() as u32,
                sequence: self.sequence,
            },
        );
        if sent {
            self.sent_count = self.sent_count.wrapping_add(1);
            self.last_sent = Some(PointerSendEvent {
                count: self.sent_count,
                phase,
                surface_id: destination.surface_id,
                x,
                y,
                coordinate_width: width,
                coordinate_height: height,
                normalized_x: destination.normalized_position.x,
                normalized_y: destination.normalized_position.y,
                sent_at: Instant::now(),
            });
        }
        if sent && (!self.logged_route || phase == PointerPhase::ButtonDown) {
            self.logged_route = true;
            eprintln!(
                "snowglass: routed pointer to surface {} hwnd={:#x} presentation=({}, {})/{}x{}",
                destination.surface_id, destination.target_window, x, y, width, height,
            );
        }
        if matches!(phase, PointerPhase::ButtonUp | PointerPhase::Cancel) && self.buttons == 0 {
            self.captured_connection = None;
        }
        sent
    }

    pub fn cancel(
        &mut self,
        connection: u64,
        presentation: &Presentation,
        plan: &CompositingPlan,
        sender: &impl PointerEventSink,
    ) -> Option<u32> {
        if self.captured_connection != Some(connection) {
            return None;
        }
        let buttons = self.buttons;
        self.send(PointerPhase::Cancel, 0, 0, 0, presentation, plan, sender);
        self.buttons = 0;
        self.captured_connection = None;
        Some(buttons)
    }
}

fn presentation_mapping(frame: FrameReady) -> Option<AffineMapping> {
    let transform = frame.presentation_transform;
    if transform.kind == PresentationMappingKind::Unavailable {
        return None;
    }
    let scale = (1u64 << PRESENTATION_COORD_FRACTIONAL_BITS) as f64;
    let point = |point: snowglass_protocol::PresentationPoint| {
        Point::new(point.x as f64 / scale, point.y as f64 / scale)
    };
    Some(AffineMapping {
        upper_left: point(transform.upper_left),
        upper_right: point(transform.upper_right),
        lower_left: point(transform.lower_left),
    })
}

pub fn map_pointer(
    presentation: &Presentation,
    plan: &CompositingPlan,
    point: Point,
    captured_connection: Option<u64>,
) -> Option<MappedPointer> {
    let outside = if captured_connection.is_some() {
        OutsideBehavior::Clamp
    } else {
        OutsideBehavior::Reject
    };
    pointer_destination(
        point,
        captured_connection,
        outside,
        plan,
        presentation.surfaces(),
    )
}

fn pointer_destination(
    point: Point,
    captured_connection: Option<u64>,
    outside: OutsideBehavior,
    plan: &CompositingPlan,
    surfaces: &[ReceivedSurface],
) -> Option<MappedPointer> {
    let order = if surfaces.len() > 1 && surfaces.iter().all(|surface| surface.flip_y) {
        VerticalOrder::BottomToTop
    } else {
        VerticalOrder::TopToBottom
    };
    let presentation_mappings = presentation_mappings(surfaces, order);
    let placements = surfaces
        .iter()
        .zip(presentation_mappings)
        .enumerate()
        .filter_map(|(surface_index, (surface, presentation_mapping))| {
            Some(snowglass_input::mouse::layout::CompositePlacement {
                surface_index,
                viewer_rect: plan.rectangle(surface.connection, surface.surface_id)?,
                presentation_extent: Extent::new(
                    surface.frame.presentation_width,
                    surface.frame.presentation_height,
                ),
                presentation_mapping,
            })
        });
    placements.into_iter().find_map(|placement| {
        let surface = surfaces.get(placement.surface_index)?;
        if captured_connection.is_some_and(|connection| connection != surface.connection) {
            return None;
        }
        let position = placement.map_to_presentation(point, outside)?;
        let normalized_position =
            normalized_surface_position(placement.viewer_rect, point, outside)?;
        Some(MappedPointer {
            connection: surface.connection,
            surface_id: surface.surface_id,
            target_window: surface.target_window,
            frame: surface.frame,
            position,
            normalized_position,
        })
    })
}

fn normalized_surface_position(
    rect: Rect,
    point: Point,
    outside: OutsideBehavior,
) -> Option<Point> {
    if rect.is_empty() || (outside == OutsideBehavior::Reject && !rect.contains(point)) {
        return None;
    }
    let x_range = f64::from(rect.width.saturating_sub(1).max(1));
    let y_range = f64::from(rect.height.saturating_sub(1).max(1));
    let x = (point.x - f64::from(rect.x)) / x_range;
    let y = (point.y - f64::from(rect.y)) / y_range;
    Some(Point::new(x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
}

fn presentation_mappings(
    surfaces: &[ReceivedSurface],
    order: VerticalOrder,
) -> Vec<Option<AffineMapping>> {
    let explicit = surfaces
        .iter()
        .map(|surface| presentation_mapping(surface.frame))
        .collect::<Vec<_>>();
    let letterboxed = |explicit: Vec<Option<AffineMapping>>| {
        surfaces
            .iter()
            .zip(explicit)
            .map(|(surface, mapping)| letterboxed_mapping(surface, mapping))
            .collect()
    };
    if surfaces.len() < 2 {
        return letterboxed(explicit);
    }
    let Some(first_surface) = surfaces.first() else {
        return explicit;
    };
    let presentation_extent = Extent::new(
        first_surface.frame.presentation_width,
        first_surface.frame.presentation_height,
    );
    let Some(target_window) =
        (first_surface.target_window != 0).then_some(first_surface.target_window)
    else {
        return letterboxed(explicit);
    };
    let same_target = surfaces.iter().all(|surface| {
        surface.target_window == target_window
            && Extent::new(
                surface.frame.presentation_width,
                surface.frame.presentation_height,
            ) == presentation_extent
    });
    let indistinguishable = explicit
        .first()
        .is_some_and(|first| explicit.iter().all(|mapping| mapping == first));
    if !same_target || !indistinguishable || presentation_extent.is_empty() {
        return letterboxed(explicit);
    }

    let inferred_layouts = surfaces
        .iter()
        .map(|surface| SurfaceLayout {
            image_extent: surface.image_extent,
            presentation_extent,
            presentation_mapping: None,
        })
        .collect::<Vec<_>>();
    let mut inferred = vec![None; surfaces.len()];
    for placement in vertical_surface_layout(presentation_extent, &inferred_layouts, order) {
        inferred[placement.surface_index] = Some(AffineMapping::axis_aligned(Rect::new(
            placement.viewer_rect.x,
            placement.viewer_rect.y,
            placement.viewer_rect.width,
            placement.viewer_rect.height,
        )));
    }
    inferred
}

/// Graph placements resolved from present draws often claim the whole window
/// even though the application letterboxes the screen inside it (the draw
/// carries no placement, so the rect degenerates to the render area). When a
/// mapping covers the full presentation extent but the displayed image has a
/// different aspect, target the centered aspect-fit rectangle instead — the
/// same fit the application itself uses — so pointer coordinates land on the
/// content rather than being stretched across the black bars.
fn letterboxed_mapping(
    surface: &ReceivedSurface,
    mapping: Option<AffineMapping>,
) -> Option<AffineMapping> {
    let mapping = mapping?;
    let full = Rect::new(
        0,
        0,
        surface.frame.presentation_width,
        surface.frame.presentation_height,
    );
    if full.is_empty() || mapping != AffineMapping::axis_aligned(full) {
        return Some(mapping);
    }
    Some(aspect_fit(surface.image_extent, full).map_or(mapping, AffineMapping::axis_aligned))
}

fn pointer_button_bit(button: u32) -> u32 {
    button
        .checked_sub(1)
        .and_then(|shift| 1u32.checked_shl(shift))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SurfacePlacement;
    use parking_lot::Mutex;
    use snowglass_protocol::PresentationTransform;
    use std::{collections::VecDeque, sync::Arc};

    #[derive(Clone, Default)]
    struct MessageCollector(Arc<Mutex<VecDeque<Message>>>);

    impl PointerEventSink for MessageCollector {
        fn send_pointer(&self, _connection: u64, event: PointerInput) -> bool {
            self.0.lock().push_back(Message::PointerInput(event));
            true
        }
    }

    impl MessageCollector {
        fn recv(&self) -> Message {
            self.0.lock().pop_front().expect("expected message")
        }
    }

    fn frame(transform: PresentationTransform) -> FrameReady {
        FrameReady {
            stream_id: snowglass_protocol::StreamId(1),
            slot: 0,
            render_width: 240,
            render_height: 400,
            capture_x: 80,
            capture_y: 120,
            capture_width: 960,
            capture_height: 576,
            presentation_width: 1280,
            presentation_height: 720,
            presentation_transform: transform,
            frame_number: 42,
            sync_value: 0,
            timestamp_qpc: 0,
        }
    }

    fn plan(placements: impl IntoIterator<Item = SurfacePlacement>) -> CompositingPlan {
        CompositingPlan::new([0.0; 4], placements.into_iter().collect())
    }

    #[test]
    fn normalized_surface_maps_into_an_offset_presentation_rectangle() {
        let surfaces = [ReceivedSurface {
            connection: 7,
            surface_id: 99,
            target_window: 123,
            // The compositor has already applied the configured quarter turn.
            image_extent: Extent::new(400, 240),
            quarter_turns: 0,
            flip_y: false,
            frame: frame(PresentationTransform::axis_aligned(80, 120, 960, 576)),
        }];
        let destination = pointer_destination(
            Point::new(200.0, 420.0),
            None,
            OutsideBehavior::Reject,
            &plan([SurfacePlacement {
                connection: 7,
                surface_id: 99,
                rectangle: Rect::new(0, 60, 800, 480),
                corner_radius: 0.0,
            }]),
            &surfaces,
        )
        .expect("point is inside the displayed surface");

        assert_eq!(destination.connection, 7);
        assert_eq!(destination.surface_id, 99);
        assert_eq!(destination.frame.frame_number, 42);
        assert_eq!(destination.position, Point::new(320.0, 552.0));
        assert!((destination.normalized_position.x - 200.0 / 799.0).abs() < f64::EPSILON);
        assert!((destination.normalized_position.y - 360.0 / 479.0).abs() < f64::EPSILON);
    }

    #[test]
    fn surface_edge_pixels_reach_normalized_axis_endpoints() {
        let rect = Rect::new(10, 20, 100, 200);
        assert_eq!(
            normalized_surface_position(rect, Point::new(10.0, 20.0), OutsideBehavior::Reject),
            Some(Point::new(0.0, 0.0))
        );
        assert_eq!(
            normalized_surface_position(rect, Point::new(109.0, 219.0), OutsideBehavior::Reject),
            Some(Point::new(1.0, 1.0))
        );
    }

    #[test]
    fn unavailable_presentation_mapping_does_not_route_input() {
        let surfaces = [ReceivedSurface {
            connection: 7,
            surface_id: 99,
            target_window: 123,
            image_extent: Extent::new(400, 240),
            quarter_turns: 0,
            flip_y: false,
            frame: frame(PresentationTransform::UNAVAILABLE),
        }];
        assert!(
            pointer_destination(
                Point::new(200.0, 420.0),
                None,
                OutsideBehavior::Reject,
                &plan([SurfacePlacement {
                    connection: 7,
                    surface_id: 99,
                    rectangle: Rect::new(0, 60, 800, 480),
                    corner_radius: 0.0,
                }]),
                &surfaces,
            )
            .is_none()
        );
    }

    #[test]
    fn identical_graph_mappings_for_one_window_use_the_normalized_surface_layout() {
        let mut top = frame(PresentationTransform::axis_aligned(0, 0, 400, 480));
        top.presentation_width = 400;
        top.presentation_height = 480;
        let mut bottom = top;
        bottom.stream_id = snowglass_protocol::StreamId(2);
        let surfaces = [
            ReceivedSurface {
                connection: 7,
                surface_id: 99,
                target_window: 123,
                image_extent: Extent::new(400, 240),
                quarter_turns: 0,
                flip_y: false,
                frame: top,
            },
            ReceivedSurface {
                connection: 8,
                surface_id: 100,
                target_window: 123,
                image_extent: Extent::new(320, 240),
                quarter_turns: 0,
                flip_y: false,
                frame: bottom,
            },
        ];
        let destination = pointer_destination(
            Point::new(400.0, 720.0),
            None,
            OutsideBehavior::Reject,
            &plan([
                SurfacePlacement {
                    connection: 7,
                    surface_id: 99,
                    rectangle: Rect::new(0, 0, 800, 480),
                    corner_radius: 0.0,
                },
                SurfacePlacement {
                    connection: 8,
                    surface_id: 100,
                    rectangle: Rect::new(80, 480, 640, 480),
                    corner_radius: 0.0,
                },
            ]),
            &surfaces,
        )
        .expect("point is inside the normalized bottom screen");

        assert_eq!(destination.connection, 8);
        assert_eq!(destination.position, Point::new(200.0, 360.0));
    }

    #[test]
    fn full_window_graph_mappings_letterbox_to_the_displayed_aspect() {
        // Two graph streams target separate windows; each mapping claims the
        // whole 400x480 window even though the application letterboxes the
        // screen inside it (azahar separate-windows).
        let mut top = frame(PresentationTransform::axis_aligned(0, 0, 400, 480));
        top.presentation_width = 400;
        top.presentation_height = 480;
        let mut bottom = top;
        bottom.stream_id = snowglass_protocol::StreamId(2);
        let surfaces = [
            ReceivedSurface {
                connection: 7,
                surface_id: 99,
                target_window: 123,
                image_extent: Extent::new(400, 240),
                quarter_turns: 0,
                flip_y: false,
                frame: top,
            },
            ReceivedSurface {
                connection: 8,
                surface_id: 100,
                target_window: 456,
                image_extent: Extent::new(320, 240),
                quarter_turns: 0,
                flip_y: false,
                frame: bottom,
            },
        ];
        // The 320x240 bottom screen aspect-fits to 400x300 centered in its
        // 400x480 window, so its top edge sits at presentation y = 90.
        let destination = pointer_destination(
            Point::new(400.0, 480.0),
            None,
            OutsideBehavior::Reject,
            &plan([
                SurfacePlacement {
                    connection: 7,
                    surface_id: 99,
                    rectangle: Rect::new(0, 0, 800, 480),
                    corner_radius: 0.0,
                },
                SurfacePlacement {
                    connection: 8,
                    surface_id: 100,
                    rectangle: Rect::new(80, 480, 640, 480),
                    corner_radius: 0.0,
                },
            ]),
            &surfaces,
        )
        .expect("point is at the top edge of the displayed bottom screen");
        assert_eq!(destination.connection, 8);
        assert_eq!(destination.position, Point::new(200.0, 90.0));

        // The 400x240 top screen aspect-fits to y = 120..360; its center maps
        // to the window center.
        let destination = pointer_destination(
            Point::new(400.0, 240.0),
            None,
            OutsideBehavior::Reject,
            &plan([
                SurfacePlacement {
                    connection: 7,
                    surface_id: 99,
                    rectangle: Rect::new(0, 0, 800, 480),
                    corner_radius: 0.0,
                },
                SurfacePlacement {
                    connection: 8,
                    surface_id: 100,
                    rectangle: Rect::new(80, 480, 640, 480),
                    corner_radius: 0.0,
                },
            ]),
            &surfaces,
        )
        .expect("point is inside the displayed top screen");
        assert_eq!(destination.connection, 7);
        assert_eq!(destination.position, Point::new(200.0, 240.0));
    }

    #[test]
    fn button_capture_clamps_moves_until_release() {
        let surfaces = [ReceivedSurface {
            connection: 7,
            surface_id: 99,
            target_window: 123,
            image_extent: Extent::new(400, 240),
            quarter_turns: 0,
            flip_y: false,
            frame: frame(PresentationTransform::axis_aligned(80, 120, 960, 576)),
        }];
        let presentation = Presentation::new(Extent::new(800, 600), surfaces.to_vec());
        let plan = plan([SurfacePlacement {
            connection: 7,
            surface_id: 99,
            rectangle: Rect::new(0, 60, 800, 480),
            corner_radius: 0.0,
        }]);
        let sender = MessageCollector::default();
        let mut routing = PointerRoutingState::new(Instant::now());
        routing.set_cursor(200.0, 420.0);
        assert!(routing.send(
            PointerPhase::ButtonDown,
            1,
            0,
            0,
            &presentation,
            &plan,
            &sender,
        ));
        let Message::PointerInput(down) = sender.recv() else {
            panic!("expected pointer input");
        };
        assert_eq!((down.x, down.y, down.buttons), (320, 552, 1));
        assert_eq!(routing.captured_connection, Some(7));
        let sent = routing.last_sent().expect("successful send is recorded");
        assert_eq!(sent.count, 1);
        assert_eq!(sent.phase, PointerPhase::ButtonDown);
        assert_eq!(sent.surface_id, 99);
        assert_eq!((sent.x, sent.y), (320, 552));
        assert_eq!((sent.coordinate_width, sent.coordinate_height), (1280, 720));

        routing.set_cursor(-100.0, -100.0);
        assert!(routing.send(PointerPhase::Move, 0, 0, 0, &presentation, &plan, &sender,));
        let Message::PointerInput(moved) = sender.recv() else {
            panic!("expected pointer input");
        };
        assert_eq!((moved.x, moved.y), (80, 120));

        assert!(routing.send(
            PointerPhase::ButtonUp,
            1,
            0,
            0,
            &presentation,
            &plan,
            &sender,
        ));
        let Message::PointerInput(up) = sender.recv() else {
            panic!("expected pointer input");
        };
        assert_eq!(up.buttons, 0);
        assert_eq!(routing.captured_connection, None);
    }
}
