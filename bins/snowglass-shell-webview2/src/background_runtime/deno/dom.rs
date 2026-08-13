use std::collections::{BTreeMap, HashMap};

use blitz_dom::{BaseDocument, NodeData};
use deno_core::{OpState, op2};
use serde::{Deserialize, Serialize};

use super::state::BridgeState;

#[derive(Debug, Deserialize)]
pub enum Mutation {
    CreateElement {
        node: usize,
        tag: String,
    },
    CreateText {
        node: usize,
        text: String,
    },
    CreateComment {
        node: usize,
    },
    AppendChild {
        parent: usize,
        child: usize,
    },
    InsertBefore {
        parent: usize,
        child: usize,
        before: usize,
    },
    RemoveChild {
        parent: usize,
        child: usize,
    },
    SetText {
        node: usize,
        text: String,
    },
    SetAttribute {
        node: usize,
        name: String,
        value: String,
    },
    RemoveAttribute {
        node: usize,
        name: String,
    },
}

#[op2(fast)]
pub(super) fn op_dom_create_element(state: &mut OpState, node: u32, #[string] tag: String) {
    state
        .borrow_mut::<BridgeState>()
        .mutations
        .push(Mutation::CreateElement {
            node: node as usize,
            tag,
        });
}

#[op2(fast)]
pub(super) fn op_dom_create_text(state: &mut OpState, node: u32, #[string] text: String) {
    state
        .borrow_mut::<BridgeState>()
        .mutations
        .push(Mutation::CreateText {
            node: node as usize,
            text,
        });
}

#[op2(fast)]
pub(super) fn op_dom_create_comment(state: &mut OpState, node: u32) {
    state
        .borrow_mut::<BridgeState>()
        .mutations
        .push(Mutation::CreateComment {
            node: node as usize,
        });
}

#[op2(fast)]
pub(super) fn op_dom_append_child(state: &mut OpState, parent: u32, child: u32) {
    state
        .borrow_mut::<BridgeState>()
        .mutations
        .push(Mutation::AppendChild {
            parent: parent as usize,
            child: child as usize,
        });
}

#[op2(fast)]
pub(super) fn op_dom_insert_before(state: &mut OpState, parent: u32, child: u32, before: u32) {
    state
        .borrow_mut::<BridgeState>()
        .mutations
        .push(Mutation::InsertBefore {
            parent: parent as usize,
            child: child as usize,
            before: before as usize,
        });
}

#[op2(fast)]
pub(super) fn op_dom_remove_child(state: &mut OpState, parent: u32, child: u32) {
    state
        .borrow_mut::<BridgeState>()
        .mutations
        .push(Mutation::RemoveChild {
            parent: parent as usize,
            child: child as usize,
        });
}

#[op2(fast)]
pub(super) fn op_dom_set_text(state: &mut OpState, node: u32, #[string] text: String) {
    state
        .borrow_mut::<BridgeState>()
        .mutations
        .push(Mutation::SetText {
            node: node as usize,
            text,
        });
}

#[op2(fast)]
pub(super) fn op_dom_set_attribute(
    state: &mut OpState,
    node: u32,
    #[string] name: String,
    #[string] value: String,
) {
    state
        .borrow_mut::<BridgeState>()
        .mutations
        .push(Mutation::SetAttribute {
            node: node as usize,
            name,
            value,
        });
}

#[op2(fast)]
pub(super) fn op_dom_remove_attribute(state: &mut OpState, node: u32, #[string] name: String) {
    state
        .borrow_mut::<BridgeState>()
        .mutations
        .push(Mutation::RemoveAttribute {
            node: node as usize,
            name,
        });
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DomSnapshot {
    document: usize,
    width: u32,
    height: u32,
    nodes: Vec<NodeSnapshot>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeSnapshot {
    id: usize,
    parent: Option<usize>,
    children: Vec<usize>,
    kind: &'static str,
    tag: Option<String>,
    text: Option<String>,
    attributes: BTreeMap<String, String>,
    rect: RectSnapshot,
}

#[derive(Serialize)]
struct RectSnapshot {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

pub(super) fn snapshot(
    document: &BaseDocument,
    width: u32,
    height: u32,
    node_ids: Option<&HashMap<usize, usize>>,
) -> DomSnapshot {
    let map_id = |id: usize| {
        node_ids
            .and_then(|node_ids| node_ids.get(&id).copied())
            .unwrap_or(id)
    };
    let nodes = document
        .tree()
        .iter()
        .filter_map(|(id, node)| {
            let (kind, tag, text, attributes) = match &node.data {
                NodeData::Document => ("document", None, None, Default::default()),
                NodeData::Element(element) => (
                    "element",
                    Some(element.name.local.to_string()),
                    None,
                    element
                        .attrs()
                        .iter()
                        .map(|attribute| {
                            (
                                attribute.name.local.to_string(),
                                attribute.value.to_string(),
                            )
                        })
                        .collect(),
                ),
                NodeData::Text(text) => {
                    ("text", None, Some(text.content.clone()), Default::default())
                }
                NodeData::Comment => ("comment", None, None, Default::default()),
                _ => return None,
            };
            let position = node.absolute_position(0.0, 0.0);
            Some(NodeSnapshot {
                id: map_id(id),
                parent: node.parent.map(map_id),
                children: node.children.iter().copied().map(map_id).collect(),
                kind,
                tag,
                text,
                attributes,
                rect: RectSnapshot {
                    x: position.x,
                    y: position.y,
                    width: node.final_layout.size.width,
                    height: node.final_layout.size.height,
                },
            })
        })
        .collect();
    DomSnapshot {
        document: map_id(document.root_node().id),
        width,
        height,
        nodes,
    }
}
