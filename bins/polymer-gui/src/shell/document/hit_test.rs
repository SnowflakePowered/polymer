use super::tracked_surface::is_tracked_surface_element;
use blitz_dom::node::Node;
use blitz_dom::{NodeData, NodeId, local_name};
use style::values::computed::{Overflow, PointerEvents};

struct PointerGeometry<'a> {
    document: &'a blitz_dom::BaseDocument,
}

impl<'a> PointerGeometry<'a> {
    fn new(document: &'a blitz_dom::BaseDocument) -> Self {
        Self { document }
    }

    /// Blitz can briefly retain a layout parent that was an anonymous box
    /// from the preceding construction pass. Its geometry API assumes every
    /// parent is still present and unwraps that lookup, so reject an invalid
    /// chain before asking it for the bounding rectangle.
    fn contains(&self, node_id: NodeId, x: f32, y: f32) -> bool {
        let mut current = Some(node_id);
        while let Some(node_id) = current {
            let Some(node) = self.document.get_node(node_id) else {
                return false;
            };
            current = node.layout_parent.get();
        }
        self.document
            .get_client_bounding_rect(node_id)
            .is_some_and(|rect| {
                rect.width > 0.0
                    && rect.height > 0.0
                    && f64::from(x) >= rect.x
                    && f64::from(x) <= rect.x + rect.width
                    && f64::from(y) >= rect.y
                    && f64::from(y) <= rect.y + rect.height
            })
    }
}

/// Hit-tests in the same stacking order Blitz paints while allowing a child
/// to restore `pointer-events: auto` beneath a transparent parent.
///
/// Blitz 0.3 hoists non-zero z-index descendants out of `paint_children`.
/// Those lists therefore have to be traversed explicitly around the ordinary
/// children, just as the painter does.
pub(super) fn pointer_hit(document: &blitz_dom::BaseDocument, x: f32, y: f32) -> Option<NodeId> {
    let geometry = PointerGeometry::new(document);
    let painted = pointer_hit_node(document, document.root_element(), x, y).and_then(|node| {
        std::iter::successors(Some(node), |node| {
            document.get_node(*node).and_then(|node| node.parent)
        })
        .find(|node| {
            document
                .get_node(*node)
                .and_then(blitz_dom::Node::element_data)
                .is_some()
        })
    });
    let root = document.root_element().id;
    if painted.is_some_and(|node| {
        node != root
            && !node_is_within_tracked_surface(document, node)
            && painted_pointer_target_is_valid(document, node, x, y)
    }) {
        return painted;
    }
    let mut geometry_hit = None;
    for (order, (node_id, node)) in document.tree().iter().enumerate() {
        if matches!(node.data, NodeData::Text(_) | NodeData::Comment { .. }) || !node.has_boxes() {
            continue;
        }
        let accepts_pointer = node
            .primary_styles()
            .is_none_or(|styles| styles.clone_pointer_events() != PointerEvents::None);
        if !accepts_pointer {
            continue;
        }
        if !geometry.contains(node_id, x, y) {
            continue;
        }
        let mut depth = 0usize;
        let mut ancestor = node_id;
        while ancestor != root {
            let Some(parent) = document.get_node(ancestor).and_then(|node| node.parent) else {
                depth = 0;
                break;
            };
            depth += 1;
            ancestor = parent;
        }
        if ancestor != root {
            continue;
        }
        let Some(interactive) = std::iter::successors(Some(node_id), |node| {
            document.get_node(*node).and_then(|node| node.parent)
        })
        .find(|node| {
            document
                .get_node(*node)
                .and_then(blitz_dom::Node::element_data)
                .is_some_and(|element| {
                    matches!(
                        &*element.name.local,
                        "button" | "input" | "textarea" | "select" | "a" | "label"
                    ) || matches!(element.attr(local_name!("role")), Some("button" | "link"))
                })
        }) else {
            continue;
        };
        if geometry_hit
            .as_ref()
            .is_none_or(|(best_depth, best_order, _)| (depth, order) > (*best_depth, *best_order))
        {
            geometry_hit = Some((depth, order, interactive));
        }
    }
    geometry_hit.map(|(_, _, node)| node).or(painted)
}

fn painted_pointer_target_is_valid(
    document: &blitz_dom::BaseDocument,
    node_id: NodeId,
    x: f32,
    y: f32,
) -> bool {
    let Some(node) = document.get_node(node_id) else {
        return false;
    };
    if !node.has_boxes() {
        return false;
    }
    if node.element_data().is_some_and(|element| {
        matches!(
            &*element.name.local,
            "head" | "style" | "script" | "link" | "meta" | "title"
        )
    }) {
        return false;
    }
    let clips_overflow = node.primary_styles().is_some_and(|styles| {
        let box_style = styles.get_box();
        !matches!(box_style.clone_overflow_x(), Overflow::Visible)
            || !matches!(box_style.clone_overflow_y(), Overflow::Visible)
    });
    if !clips_overflow || node.transform().is_some() {
        return true;
    }
    PointerGeometry::new(document).contains(node_id, x, y)
}

pub(super) fn node_is_within_tracked_surface(
    document: &blitz_dom::BaseDocument,
    node_id: NodeId,
) -> bool {
    std::iter::successors(Some(node_id), |node| {
        document.get_node(*node).and_then(|node| node.parent)
    })
    .any(|node| {
        document
            .get_node(node)
            .and_then(blitz_dom::Node::element_data)
            .is_some_and(is_tracked_surface_element)
    })
}

fn pointer_hit_node(
    document: &blitz_dom::BaseDocument,
    node: &Node,
    x: f32,
    y: f32,
) -> Option<NodeId> {
    if matches!(node.data, NodeData::Text(_) | NodeData::Comment { .. }) || !node.has_boxes() {
        return None;
    }
    let mut local_x = x - node.final_layout().location.x + node.scroll_offset().x as f32;
    let mut local_y = y - node.final_layout().location.y + node.scroll_offset().y as f32;
    if let Some(transform) = node.transform() {
        let point = transform.inverse() * kurbo::Point::new(local_x.into(), local_y.into());
        local_x = point.x as f32;
        local_y = point.y as f32;
    }

    let size = node.final_layout().size;
    let matches_self = local_x >= 0.0
        && local_x <= size.width + node.scroll_offset().x as f32
        && local_y >= 0.0
        && local_y <= size.height + node.scroll_offset().y as f32;
    let overflow = node.scrollable_overflow();
    let matches_overflow = local_x >= overflow.x0 as f32
        && local_x <= overflow.x1 as f32
        && local_y >= overflow.y0 as f32
        && local_y <= overflow.y1 as f32;
    let matches_hoisted = node.stacking_context.as_ref().is_some_and(|context| {
        let area =
            context.hoisted_content_bbox(document.tree(), node.id, document.viewport().scale_f64());
        local_x >= area.left
            && local_x <= area.right
            && local_y >= area.top
            && local_y <= area.bottom
    });
    if !matches_self && !matches_overflow && !matches_hoisted {
        return None;
    }
    let clips_overflow = node.primary_styles().is_some_and(|styles| {
        let box_style = styles.get_box();
        !matches!(box_style.clone_overflow_x(), Overflow::Visible)
            || !matches!(box_style.clone_overflow_y(), Overflow::Visible)
    });
    if clips_overflow && !matches_self {
        return None;
    }

    if let Some(context) = &node.stacking_context {
        for child in context.pos_z_hoisted_children().rev() {
            let position = child.position(document.tree(), node.id);
            if let Some(hit) = document.get_node(child.node_id).and_then(|node| {
                pointer_hit_node(document, node, local_x - position.x, local_y - position.y)
            }) {
                return Some(hit);
            }
        }
    }

    let children = node.paint_children.borrow().clone().unwrap_or_default();
    for child in children.into_iter().rev() {
        if let Some(hit) = document
            .get_node(child)
            .and_then(|child| pointer_hit_node(document, child, local_x, local_y))
        {
            return Some(hit);
        }
    }

    if let Some(context) = &node.stacking_context {
        for child in context.neg_z_hoisted_children().rev() {
            let position = child.position(document.tree(), node.id);
            if let Some(hit) = document.get_node(child.node_id).and_then(|node| {
                pointer_hit_node(document, node, local_x - position.x, local_y - position.y)
            }) {
                return Some(hit);
            }
        }
    }

    let accepts_pointer = node
        .primary_styles()
        .is_none_or(|styles| styles.clone_pointer_events() != PointerEvents::None);
    (matches_self && accepts_pointer).then_some(node.id)
}
