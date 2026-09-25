use blitz_dom::{BaseDocument, Node};
use deno_core::{OpState, op2};
use serde::{Deserialize, Serialize};

use super::dom::ScriptNodeId;
use crate::shell_document::SharedShellDocument;

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrollBehavior {
    #[default]
    Auto,
    Instant,
    Smooth,
}

impl From<ScrollBehavior> for blitz_dom::ScrollBehavior {
    fn from(value: ScrollBehavior) -> Self {
        match value {
            ScrollBehavior::Auto => Self::Auto,
            ScrollBehavior::Instant => Self::Instant,
            ScrollBehavior::Smooth => Self::Smooth,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrollAlignment {
    Start,
    Center,
    End,
    Nearest,
}

impl ScrollAlignment {
    pub(crate) fn delta(self, start: f64, end: f64, extent: f64) -> f64 {
        match self {
            Self::Start => start,
            Self::Center => (start + end - extent) / 2.0,
            Self::End => end - extent,
            Self::Nearest if (start >= 0.0 && end <= extent) || (start <= 0.0 && end >= extent) => {
                0.0
            }
            Self::Nearest if start.abs() < (end - extent).abs() => start,
            Self::Nearest => end - extent,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ScrollCommand {
    To {
        left: Option<f64>,
        top: Option<f64>,
        behavior: ScrollBehavior,
    },
    By {
        left: f64,
        top: f64,
        behavior: ScrollBehavior,
    },
    IntoView {
        block: ScrollAlignment,
        inline: ScrollAlignment,
        nearest: bool,
    },
}

impl ScrollCommand {
    pub fn animates(&self) -> bool {
        matches!(
            self,
            Self::To { behavior, .. } | Self::By { behavior, .. }
                if !matches!(behavior, ScrollBehavior::Instant)
        )
    }
}

#[op2]
pub(super) fn op_dom_scroll(state: &mut OpState, node: f64, #[serde] command: ScrollCommand) {
    state
        .borrow::<SharedShellDocument>()
        .0
        .borrow_mut()
        .scroll(blitz_dom::NodeId::from_script(node), command);
}

#[derive(Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ScrollSnapshot {
    pub left: f64,
    pub top: f64,
    pub width: f32,
    pub height: f32,
    pub client_width: f32,
    pub client_height: f32,
    pub client_left: f32,
    pub client_top: f32,
}

impl ScrollSnapshot {
    pub(super) fn from_node(document: &BaseDocument, node: &Node) -> Self {
        if !node.is_element() || !node.has_boxes() {
            return Self::default();
        }
        let root = document
            .try_root_element()
            .is_some_and(|root| root.id == node.id);
        let offset = if root {
            document.viewport_scroll()
        } else {
            *node.scroll_offset()
        };
        let viewport = document.viewport();
        Self {
            left: offset.x,
            top: offset.y,
            width: node.scroll_width(),
            height: node.scroll_height(),
            client_width: if root {
                viewport.window_size.0 as f32 / viewport.scale()
            } else {
                node.client_width()
            },
            client_height: if root {
                viewport.window_size.1 as f32 / viewport.scale()
            } else {
                node.client_height()
            },
            client_left: node.final_layout().border.left,
            client_top: node.final_layout().border.top,
        }
    }
}
