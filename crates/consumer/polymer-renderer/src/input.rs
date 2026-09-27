use crate::layout::{
    AffineMapping, Extent, OutsideBehavior, Point, Rect, SurfaceLayout, VerticalOrder, aspect_fit,
    vertical_surface_layout,
};
use crate::{CompositingPlan, MappedPointer, Presentation, ReceivedSurface};
use polymer_protocol::{
    FrameReady, PRESENTATION_COORD_FRACTIONAL_BITS, PresentationMappingKind,
    input::{PointerInput, PointerPhase, PointerSource},
};
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
                tracing::warn!(
                    "polymer: pointer button was not routed at ({:.1}, {:.1}); displayed surfaces={}, mapped surfaces={}, pointer targets={}",
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
            tracing::debug!(
                "polymer: routed pointer to surface {} hwnd={:#x} presentation=({}, {})/{}x{}",
                destination.surface_id,
                destination.target_window,
                x,
                y,
                width,
                height,
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
    let point = |point: polymer_protocol::PresentationPoint| {
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
        None,
        outside,
        plan,
        presentation.surfaces(),
    )
}

pub(crate) fn map_pointer_to_surface(
    presentation: &Presentation,
    plan: &CompositingPlan,
    point: Point,
    selector: crate::pointer::SurfaceSelector,
) -> Option<MappedPointer> {
    pointer_destination(
        point,
        Some(selector.connection),
        Some(selector),
        OutsideBehavior::Clamp,
        plan,
        presentation.surfaces(),
    )
}

fn pointer_destination(
    point: Point,
    captured_connection: Option<u64>,
    selector: Option<crate::pointer::SurfaceSelector>,
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
        .flat_map(|(surface_index, (surface, presentation_mapping))| {
            plan.placements()
                .filter(move |placement| {
                    placement.connection == surface.connection
                        && placement.surface_id == surface.surface_id
                })
                .map(move |placement| (surface_index, *placement, presentation_mapping))
        });
    placements
        .into_iter()
        .find_map(|(surface_index, placement, presentation_mapping)| {
            let surface = surfaces.get(surface_index)?;
            if captured_connection.is_some_and(|connection| connection != surface.connection) {
                return None;
            }
            if selector.is_some_and(|selector| {
                selector.connection != surface.connection
                    || selector
                        .surface_id
                        .is_some_and(|id| id != surface.surface_id)
            }) {
                return None;
            }
            let (sample_position, normalized_position) =
                content_positions(placement, point, outside)?;
            let position = presentation_mapping?.map(sample_position.x, sample_position.y);
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

fn content_positions(
    placement: crate::SurfacePlacement,
    point: Point,
    outside: OutsideBehavior,
) -> Option<(Point, Point)> {
    let hit = placement.rectangle;
    if hit.is_empty() || (outside == OutsideBehavior::Reject && !hit.contains(point)) {
        return None;
    }
    let destination = placement.content.destination;
    let width = f64::from(destination[2] - destination[0]);
    let height = f64::from(destination[3] - destination[1]);
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let local_x = point.x - f64::from(destination[0]);
    let local_y = point.y - f64::from(destination[1]);
    let source = placement.content.source;
    let map_source = |u: f64, v: f64| {
        Point::new(
            f64::from(source[0]) + u.clamp(0.0, 1.0) * f64::from(source[2] - source[0]),
            f64::from(source[1]) + v.clamp(0.0, 1.0) * f64::from(source[3] - source[1]),
        )
    };
    let sample = map_source(local_x / width, local_y / height);
    // DirectInput's normalized axes use the existing pixel-endpoint
    // convention: the first and last destination pixels map exactly to 0 and
    // 1. Presentation transforms use continuous box coordinates above.
    let normalized = map_source(
        local_x / (width - 1.0).max(1.0),
        local_y / (height - 1.0).max(1.0),
    );
    Some((sample, normalized))
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
