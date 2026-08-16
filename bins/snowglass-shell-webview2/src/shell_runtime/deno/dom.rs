use std::collections::{BTreeMap, HashMap};

use blitz_dom::{BaseDocument, NodeData, local_name};
use deno_core::{OpState, op2};
use serde::{Deserialize, Serialize};

use super::state::BridgeState;

#[derive(Debug, Deserialize)]
pub enum Mutation {
    CreateElement {
        node: usize,
        tag: String,
        namespace: Option<String>,
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
    Focus {
        node: usize,
    },
}

#[op2]
pub(super) fn op_dom_create_element(
    state: &mut OpState,
    node: u32,
    #[string] tag: String,
    #[string] namespace: Option<String>,
) {
    state
        .borrow_mut::<BridgeState>()
        .mutations
        .push(Mutation::CreateElement {
            node: node as usize,
            tag,
            namespace,
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

#[op2(fast)]
pub(super) fn op_dom_focus(state: &mut OpState, node: u32) {
    state
        .borrow_mut::<BridgeState>()
        .mutations
        .push(Mutation::Focus {
            node: node as usize,
        });
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FragmentNode {
    kind: &'static str,
    tag: Option<String>,
    namespace: Option<String>,
    text: Option<String>,
    attributes: BTreeMap<String, String>,
    children: Vec<FragmentNode>,
}

fn fragment_node(document: &BaseDocument, id: usize) -> Option<FragmentNode> {
    let node = document.tree().get(id)?;
    let (kind, tag, namespace, text, attributes) = match &node.data {
        NodeData::Element(element) => (
            "element",
            Some(element.name.local.to_string()),
            Some(element.name.ns.to_string()),
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
        NodeData::Text(text) => (
            "text",
            None,
            None,
            Some(text.content.clone()),
            BTreeMap::new(),
        ),
        NodeData::Comment => ("comment", None, None, None, BTreeMap::new()),
        _ => return None,
    };
    Some(FragmentNode {
        kind,
        tag,
        namespace,
        text,
        attributes,
        children: node
            .children
            .iter()
            .filter_map(|child| fragment_node(document, *child))
            .collect(),
    })
}

/// Parse Solid's compiled template strings with the same HTML parser used by
/// the authoritative Blitz document. The returned tree is deliberately
/// identity-free; JavaScript allocates wrapper IDs while mirroring it.
#[op2]
#[serde]
pub(super) fn op_dom_parse_fragment(#[string] html: String) -> Vec<FragmentNode> {
    let source = format!("<!doctype html><html><body>{html}</body></html>");
    let document =
        blitz_html::HtmlDocument::from_html(&source, blitz_dom::DocumentConfig::default());
    let Some(body) = document.as_ref().tree().iter().find_map(|(id, node)| {
        node.element_data()
            .is_some_and(|element| element.name.local == local_name!("body"))
            .then_some(id)
    }) else {
        return Vec::new();
    };
    document
        .as_ref()
        .tree()
        .get(body)
        .into_iter()
        .flat_map(|node| node.children.iter())
        .filter_map(|child| fragment_node(document.as_ref(), *child))
        .collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DomSnapshot {
    document: usize,
    active_element: Option<usize>,
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

#[derive(Clone, Copy, PartialEq, Serialize)]
struct RectSnapshot {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// The JavaScript mirror already applies script-originated topology,
/// attributes, and text synchronously. After Blitz resolves layout, only
/// authoritative geometry and focus need to travel back across the bridge.
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LayoutSnapshot {
    active_element: Option<usize>,
    width: u32,
    height: u32,
    nodes: Vec<LayoutNodeSnapshot>,
}

impl LayoutSnapshot {
    pub(super) fn has_geometry_changes(&self) -> bool {
        !self.nodes.is_empty()
    }

    pub(super) fn view_state(&self) -> (Option<usize>, u32, u32) {
        (self.active_element, self.width, self.height)
    }
}

#[derive(Serialize)]
struct LayoutNodeSnapshot {
    id: usize,
    rect: RectSnapshot,
}

pub(super) fn layout_snapshot(
    document: &BaseDocument,
    width: u32,
    height: u32,
    node_ids: &HashMap<usize, usize>,
    previous: &mut HashMap<usize, [f32; 4]>,
    roots: Option<&[usize]>,
) -> LayoutSnapshot {
    fn same_geometry(previous: [f32; 4], current: [f32; 4]) -> bool {
        previous
            .into_iter()
            .zip(current)
            .all(|(previous, current)| {
                previous == current || (previous.is_nan() && current.is_nan())
            })
    }

    let full_snapshot = roots.is_none();
    let mut present = std::collections::HashSet::new();
    let candidates = if let Some(roots) = roots {
        let mut candidates = Vec::new();
        let mut pending = roots.to_vec();
        while let Some(id) = pending.pop() {
            if !present.insert(id) {
                continue;
            }
            candidates.push(id);
            if let Some(node) = document.get_node(id) {
                pending.extend(node.children.iter().copied());
            }
        }
        candidates
    } else {
        document.tree().iter().map(|(id, _)| id).collect()
    };
    present.clear();
    let nodes: Vec<LayoutNodeSnapshot> = candidates
        .into_iter()
        .filter_map(|tree_id| {
            let node = document.get_node(tree_id)?;
            let id = node_ids.get(&tree_id).copied()?;
            present.insert(id);
            let position = node.absolute_position(0.0, 0.0);
            let values = [
                position.x,
                position.y,
                node.final_layout.size.width,
                node.final_layout.size.height,
            ];
            let unchanged = previous
                .insert(id, values)
                .is_some_and(|old| same_geometry(old, values));
            if unchanged {
                return None;
            }
            Some(LayoutNodeSnapshot {
                id,
                rect: RectSnapshot {
                    x: values[0],
                    y: values[1],
                    width: values[2],
                    height: values[3],
                },
            })
        })
        .collect();
    if full_snapshot {
        previous.retain(|id, _| present.contains(id));
    }
    LayoutSnapshot {
        active_element: document
            .get_focussed_node_id()
            .and_then(|id| node_ids.get(&id).copied()),
        width,
        height,
        nodes,
    }
}

#[op2]
#[serde]
pub(super) fn op_dom_take_layout(state: &mut OpState) -> LayoutSnapshot {
    state
        .borrow_mut::<BridgeState>()
        .layout
        .take()
        .unwrap_or_default()
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
        active_element: document.get_focussed_node_id().map(map_id),
        width,
        height,
        nodes,
    }
}
