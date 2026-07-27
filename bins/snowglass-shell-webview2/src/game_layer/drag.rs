//! Moving boxes in the game layer with the pointer.
//!
//! The layer has no scripting, so dragging is the shell's job. A document opts
//! an element in with `data-snowglass-drag`; the shell then writes
//! `--snowglass-drag-x` and `--snowglass-drag-y` onto it as the pointer moves,
//! and the document's own stylesheet decides what those mean. That keeps the
//! shell out of the business of how a thing moves, which is a layout question.
//!
//! Game views move with whatever contains them for free: the placement the
//! compositor is given comes from the laid-out document, so re-resolving after
//! a drag reports the view's new home along with everything else.

use blitz_dom::{BaseDocument, LocalName, QualName, local_name, ns};

/// Marks an element the pointer may move.
pub const DRAG_ATTRIBUTE: &str = "data-snowglass-drag";

/// A drag in progress.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    node: usize,
    /// Where the pointer went down, so movement is measured from there rather
    /// than from wherever the element happened to be.
    grabbed_at: (f32, f32),
    /// The element's offset when the drag started.
    origin: (f32, f32),
    /// How far the offset may travel before the element leaves the window.
    limit: (f32, f32, f32, f32),
}

/// Finds the draggable element under a point, if any.
///
/// A hit inside a game view is not a drag: the pointer belongs to the guest
/// there, and only the surrounding chrome is a handle.
pub fn find(document: &BaseDocument, x: f32, y: f32) -> Option<usize> {
    let hit = document.hit(x, y)?;
    let mut node = Some(hit.node_id);
    while let Some(id) = node {
        let current = document.get_node(id)?;
        if is_game_view(current) {
            return None;
        }
        if current.attr(LocalName::from(DRAG_ATTRIBUTE)).is_some() {
            return Some(id);
        }
        node = current.parent;
    }
    None
}

fn is_game_view(node: &blitz_dom::Node) -> bool {
    node.element_data()
        .is_some_and(|element| element.name.local == local_name!("canvas"))
}

impl Drag {
    /// Starts moving `node`, given where the pointer went down and the viewport
    /// it has to stay inside.
    pub fn start(
        document: &BaseDocument,
        node: usize,
        x: f32,
        y: f32,
        viewport: (u32, u32),
    ) -> Self {
        let origin = offset_of(document, node);
        // `absolute_position` reports where layout put the box, which is where
        // it sits before the drag transform moves it. Adding the offset back
        // gives where it is now, and the room left on each side.
        let placed = document
            .get_node(node)
            .map(|current| {
                let position = current.absolute_position(0.0, 0.0);
                let size = current.final_layout.size;
                (position.x, position.y, size.width, size.height)
            })
            .unwrap_or_default();
        let (left, top, width, height) = placed;
        let (right, bottom) = (left + width, top + height);
        Self {
            node,
            grabbed_at: (x, y),
            origin,
            limit: (
                origin.0 - left,
                origin.0 + viewport.0 as f32 - right,
                origin.1 - top,
                origin.1 + viewport.1 as f32 - bottom,
            ),
        }
    }

    pub fn node(&self) -> usize {
        self.node
    }

    /// Where the element should sit for a pointer at `x`, `y`.
    pub fn offset_for(&self, x: f32, y: f32) -> (f32, f32) {
        let (min_x, max_x, min_y, max_y) = self.limit;
        (
            (self.origin.0 + x - self.grabbed_at.0).clamp(min_x.min(max_x), max_x.max(min_x)),
            (self.origin.1 + y - self.grabbed_at.1).clamp(min_y.min(max_y), max_y.max(min_y)),
        )
    }
}

/// Writes an offset onto the element, replacing whatever it held.
///
/// Custom properties rather than a `transform`, so the document decides whether
/// being dragged means moving, tilting, or something else. A document that
/// contains a game view should spend them on layout: Blitz does not yet compose
/// an ancestor's transform onto its descendants, so a transform would move the
/// chrome and leave the view behind.
pub fn apply(document: &mut BaseDocument, node: usize, offset: (f32, f32)) {
    let style = format!(
        "--snowglass-drag-x: {:.1}px; --snowglass-drag-y: {:.1}px",
        offset.0, offset.1
    );
    document.mutate().set_attribute(
        node,
        QualName::new(None, ns!(html), local_name!("style")),
        &style,
    );
}

/// The offset an element currently carries, or zero if it has never moved.
fn offset_of(document: &BaseDocument, node: usize) -> (f32, f32) {
    let Some(style) = document
        .get_node(node)
        .and_then(|current| current.attr(local_name!("style")))
    else {
        return (0.0, 0.0);
    };
    let read = |name: &str| {
        style
            .split(';')
            .filter_map(|part| part.split_once(':'))
            .find(|(key, _)| key.trim() == name)
            .and_then(|(_, value)| value.trim().trim_end_matches("px").parse().ok())
            .unwrap_or(0.0)
    };
    (read("--snowglass-drag-x"), read("--snowglass-drag-y"))
}
