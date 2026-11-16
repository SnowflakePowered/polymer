//! CDP box-model projection from Blitz layout geometry.

use blitz_dom::{BaseDocument, Node};
use serde_json::{Value, json};

pub(super) struct NodeGeometry<'a> {
    document: &'a BaseDocument,
    node: &'a Node,
}

impl<'a> NodeGeometry<'a> {
    pub(super) fn new(document: &'a BaseDocument, node: &'a Node) -> Self {
        Self { document, node }
    }

    pub(super) fn box_model(&self) -> Value {
        let layout = self.node.final_layout();
        let position = self.absolute_position();
        let border = Self::quad(
            position.x,
            position.y,
            layout.size.width,
            layout.size.height,
        );
        let padding = Self::inset_quad(
            position.x,
            position.y,
            layout.size.width,
            layout.size.height,
            layout.border.left,
            layout.border.top,
            layout.border.right,
            layout.border.bottom,
        );
        let content = Self::inset_quad(
            position.x,
            position.y,
            layout.size.width,
            layout.size.height,
            layout.border.left + layout.padding.left,
            layout.border.top + layout.padding.top,
            layout.border.right + layout.padding.right,
            layout.border.bottom + layout.padding.bottom,
        );
        let margin = Self::inset_quad(
            position.x,
            position.y,
            layout.size.width,
            layout.size.height,
            -layout.margin.left,
            -layout.margin.top,
            -layout.margin.right,
            -layout.margin.bottom,
        );
        json!({
            "content": content,
            "padding": padding,
            "border": border,
            "margin": margin,
            "width": layout.size.width.round() as i64,
            "height": layout.size.height.round() as i64,
        })
    }

    pub(super) fn content_quad(&self) -> Vec<f32> {
        let layout = self.node.final_layout();
        let position = self.absolute_position();
        Self::inset_quad(
            position.x,
            position.y,
            layout.size.width,
            layout.size.height,
            layout.border.left + layout.padding.left,
            layout.border.top + layout.padding.top,
            layout.border.right + layout.padding.right,
            layout.border.bottom + layout.padding.bottom,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn inset_quad(
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    ) -> Vec<f32> {
        Self::quad(
            x + left,
            y + top,
            width - left - right,
            height - top - bottom,
        )
    }

    fn quad(x: f32, y: f32, width: f32, height: f32) -> Vec<f32> {
        vec![x, y, x + width, y, x + width, y + height, x, y + height]
    }

    fn absolute_position(&self) -> blitz_dom::util::Point<f32> {
        let mut x = 0.0;
        let mut y = 0.0;
        let mut current = self.node;
        loop {
            let layout = current.final_layout();
            x += layout.location.x - current.scroll_offset().x as f32;
            y += layout.location.y - current.scroll_offset().y as f32;
            let Some(parent) = current
                .layout_parent
                .get()
                .and_then(|parent| self.document.get_node(parent))
            else {
                break;
            };
            current = parent;
        }
        blitz_dom::util::Point { x, y }
    }
}
