use blitz_dom::{BaseDocument, NodeId, ScrollBehavior};
use style::values::computed::Overflow;

use crate::shell::runtime::deno::{ScrollAlignment, ScrollCommand};

pub(super) struct Scroller;

impl Scroller {
    pub(super) fn apply(document: &mut BaseDocument, node: NodeId, command: ScrollCommand) {
        let Some(target) = document
            .get_node(node)
            .filter(|node| node.is_element() && node.has_boxes())
        else {
            return;
        };
        let root = document
            .try_root_element()
            .is_some_and(|root| root.id == node);
        let offset = if root {
            document.viewport_scroll()
        } else {
            *target.scroll_offset()
        };
        match command {
            ScrollCommand::To {
                left,
                top,
                behavior,
            } => document.scroll_to(
                node,
                left.unwrap_or(offset.x),
                top.unwrap_or(offset.y),
                behavior.into(),
            ),
            ScrollCommand::By {
                left,
                top,
                behavior,
            } => document.scroll_by(node, left, top, behavior.into()),
            ScrollCommand::IntoView {
                block,
                inline,
                nearest,
            } => Self::into_view(document, node, block, inline, nearest),
        }
    }

    // Blitz's own scroll_into_view currently targets only the viewport. Walk the
    // layout ancestors instead, resolving each outer target after inner scrolling.
    fn into_view(
        document: &mut BaseDocument,
        target: NodeId,
        block: ScrollAlignment,
        inline: ScrollAlignment,
        nearest: bool,
    ) {
        let mut parent = document
            .get_node(target)
            .and_then(|node| node.layout_parent.get());
        while let Some(id) = parent {
            let Some(node) = document.get_node(id) else {
                break;
            };
            parent = node.layout_parent.get();
            let Some(style) = node.primary_styles() else {
                continue;
            };
            let scrollable =
                |value| matches!(value, Overflow::Auto | Overflow::Scroll | Overflow::Hidden);
            let root = document
                .try_root_element()
                .is_some_and(|root| root.id == id);
            let axes = (
                root || scrollable(style.clone_overflow_x()),
                root || scrollable(style.clone_overflow_y()),
            );
            if !axes.0 && !axes.1 {
                continue;
            }
            drop(style);
            Self::reveal(document, target, id, axes, block, inline);
            if nearest {
                break;
            }
        }
    }

    fn reveal(
        document: &mut BaseDocument,
        target: NodeId,
        container: NodeId,
        axes: (bool, bool),
        block: ScrollAlignment,
        inline: ScrollAlignment,
    ) {
        let Some(item) = document.get_node(target) else {
            return;
        };
        let Some(node) = document.get_node(container) else {
            return;
        };
        // absolute_position subtracts the node's own scroll; add it back so we
        // compare border boxes, not the scroll origin inside either element.
        let item_pos =
            item.absolute_position(item.scroll_offset().x as f32, item.scroll_offset().y as f32);
        let port_pos =
            node.absolute_position(node.scroll_offset().x as f32, node.scroll_offset().y as f32);
        let layout = node.final_layout();
        let root = document
            .try_root_element()
            .is_some_and(|root| root.id == container);
        let viewport = document.viewport();
        let (x, y, client_width, client_height) = if root {
            let offset = document.viewport_scroll();
            (
                f64::from(item_pos.x) - offset.x,
                f64::from(item_pos.y) - offset.y,
                f64::from(viewport.window_size.0 as f32 / viewport.scale()),
                f64::from(viewport.window_size.1 as f32 / viewport.scale()),
            )
        } else {
            (
                f64::from(item_pos.x - port_pos.x - layout.border.left),
                f64::from(item_pos.y - port_pos.y - layout.border.top),
                f64::from(node.client_width()),
                f64::from(node.client_height()),
            )
        };
        let width = f64::from(item.final_layout().size.width);
        let height = f64::from(item.final_layout().size.height);
        let dx = if axes.0 {
            inline.delta(x, x + width, client_width)
        } else {
            0.0
        };
        let dy = if axes.1 {
            block.delta(y, y + height, client_height)
        } else {
            0.0
        };
        // Nested containers must settle inside-out. Blitz owns only one smooth
        // scroll animation at a time, so reveal atomically (also reduced-motion safe).
        document.scroll_by(container, dx, dy, ScrollBehavior::Instant);
    }
}
