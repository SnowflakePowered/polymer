use std::collections::HashMap;

use blitz_dom::{BaseDocument, Node, NodeData, NodeId, local_name};
use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use serde::Serialize;
use style::properties::{PropertyDeclarationId, PropertyId};

use super::scroll::ScrollSnapshot;
use super::state::BridgeState;
use crate::shell_document::SharedShellDocument;

pub(crate) trait ScriptNodeId {
    fn from_script(id: f64) -> Self;
    fn to_script(self) -> f64;
}

impl ScriptNodeId for NodeId {
    fn from_script(id: f64) -> Self {
        NodeId::from_u64(id as u64)
    }

    fn to_script(self) -> f64 {
        self.as_u64() as f64
    }
}

impl ScriptNodeId for Option<NodeId> {
    fn from_script(id: f64) -> Self {
        (id > 0.0).then(|| NodeId::from_script(id))
    }

    fn to_script(self) -> f64 {
        self.map_or(0.0, NodeId::to_script)
    }
}

fn selector_error(error: impl std::fmt::Debug) -> JsErrorBox {
    JsErrorBox::new("SyntaxError", format!("invalid selector: {error:?}"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct NodeInfo {
    kind: &'static str,
    tag: Option<String>,
    namespace: Option<String>,
}

#[op2]
#[serde]
pub(super) fn op_dom_node_info(state: &OpState, node: f64) -> Option<NodeInfo> {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    let node = shell.base().get_node(NodeId::from_script(node))?;
    let (kind, element) = match &node.data {
        NodeData::Document(_) => ("document", None),
        NodeData::Element(element) if &*element.name.local == "#document-fragment" => {
            ("fragment", None)
        }
        NodeData::Element(element) => ("element", Some(element)),
        NodeData::Text(_) => ("text", None),
        NodeData::Comment { .. } => ("comment", None),
        _ => return None,
    };
    Some(NodeInfo {
        kind,
        tag: element.map(|element| element.name.local.to_string()),
        namespace: element.map(|element| element.name.ns.to_string()),
    })
}

#[op2(fast)]
pub(super) fn op_dom_relative(state: &OpState, node: f64, relation: u32) -> f64 {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    let document = shell.base();
    let id = NodeId::from_script(node);
    let Some(node) = document.get_node(id) else {
        return 0.0;
    };
    let sibling = |offset: isize| {
        let siblings = &document.get_node(node.parent?)?.children;
        let index = siblings.iter().position(|child| *child == id)?;
        siblings.get(index.checked_add_signed(offset)?).copied()
    };
    match relation {
        0 => node.parent,
        1 => node.children.first().copied(),
        2 => node.children.last().copied(),
        3 => sibling(1),
        4 => sibling(-1),
        _ => None,
    }
    .to_script()
}

#[op2]
#[serde]
pub(super) fn op_dom_children(state: &OpState, node: f64) -> Vec<f64> {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    shell
        .base()
        .get_node(NodeId::from_script(node))
        .map(|node| {
            node.children
                .iter()
                .map(|child| child.to_script())
                .collect()
        })
        .unwrap_or_default()
}

#[op2]
#[string]
pub(super) fn op_dom_text(state: &OpState, node: f64) -> String {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    match shell
        .base()
        .get_node(NodeId::from_script(node))
        .map(|node| &node.data)
    {
        Some(NodeData::Text(text)) => text.content.clone(),
        Some(NodeData::Comment { contents }) => contents.clone(),
        _ => String::new(),
    }
}

#[op2]
#[string]
pub(super) fn op_dom_text_content(state: &OpState, node: f64) -> String {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    shell
        .base()
        .get_node(NodeId::from_script(node))
        .map(Node::text_content)
        .unwrap_or_default()
}

#[op2(fast)]
pub(super) fn op_dom_is_connected(state: &OpState, node: f64) -> bool {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    shell
        .base()
        .get_node(NodeId::from_script(node))
        .is_some_and(|node| node.flags.is_in_document())
}

#[op2]
#[serde]
pub(super) fn op_dom_attributes(state: &OpState, node: f64) -> Vec<(String, String)> {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    shell
        .base()
        .get_node(NodeId::from_script(node))
        .and_then(Node::element_data)
        .map(|element| {
            element
                .attrs()
                .iter()
                .map(|attribute| {
                    (
                        attribute.name.local.to_string(),
                        attribute.value.to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

#[op2]
#[string]
pub(super) fn op_dom_get_attribute(
    state: &OpState,
    node: f64,
    #[string] name: String,
) -> Option<String> {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    shell
        .base()
        .get_node(NodeId::from_script(node))
        .and_then(Node::element_data)
        .and_then(|element| element.attr(blitz_dom::LocalName::from(name)))
        .map(str::to_owned)
}

#[op2]
#[string]
pub(super) fn op_dom_computed_style(
    state: &OpState,
    node: f64,
    #[string] name: String,
) -> Option<String> {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    let node = shell.base().get_node(NodeId::from_script(node))?;
    let styles = node.primary_styles()?;
    let property = PropertyId::parse_enabled_for_all_content(&name).ok()?;
    let declaration = match &property {
        PropertyId::Custom(name) => PropertyDeclarationId::Custom(name),
        PropertyId::NonCustom(_) => PropertyDeclarationId::Longhand(property.longhand_id()?),
    };
    Some(styles.computed_value_to_string(declaration))
}

#[op2]
#[serde]
pub(super) fn op_dom_query(
    state: &OpState,
    scope: f64,
    #[string] selector: String,
    all: bool,
) -> Result<Vec<f64>, JsErrorBox> {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    let document = shell.base();
    let scope = NodeId::from_script(scope);
    if document.get_node(scope).is_none() {
        return Ok(Vec::new());
    }
    let nodes = if all {
        document
            .query_selector_all_in(scope, &selector)
            .map_err(selector_error)?
            .into_iter()
            .collect()
    } else {
        document
            .query_selector_in(scope, &selector)
            .map_err(selector_error)?
            .into_iter()
            .collect::<Vec<_>>()
    };
    Ok(nodes.into_iter().map(NodeId::to_script).collect())
}

#[op2(fast)]
pub(super) fn op_dom_matches(
    state: &OpState,
    node: f64,
    #[string] selector: &str,
) -> Result<bool, JsErrorBox> {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    let node = NodeId::from_script(node);
    if shell.base().get_node(node).is_none() {
        return Ok(false);
    }
    shell
        .base()
        .matches_selector(node, selector)
        .map_err(selector_error)
}

#[op2(fast)]
pub(super) fn op_dom_closest(
    state: &OpState,
    node: f64,
    #[string] selector: &str,
) -> Result<f64, JsErrorBox> {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    let node = NodeId::from_script(node);
    if shell.base().get_node(node).is_none() {
        return Ok(0.0);
    }
    Ok(shell
        .base()
        .closest(node, selector)
        .map_err(selector_error)?
        .to_script())
}

#[op2(fast)]
pub(super) fn op_dom_element_by_id(state: &OpState, #[string] id: &str) -> f64 {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    shell.base().get_element_by_id(id).to_script()
}

#[op2(fast)]
pub(super) fn op_dom_focused(state: &OpState) -> f64 {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    shell.base().get_focussed_node_id().to_script()
}

#[op2]
#[serde]
pub(super) fn op_dom_rect(state: &OpState, node: f64) -> Vec<f32> {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    let document = shell.base();
    document
        .get_node(NodeId::from_script(node))
        .filter(|node| node.is_element())
        .and_then(|node| RectSnapshot::from_layout(document, node))
        .map_or(vec![0.0; 4], |rect| {
            vec![rect.x, rect.y, rect.width, rect.height]
        })
}

#[op2]
#[serde]
pub(super) fn op_dom_scroll_metrics(state: &OpState, node: f64) -> ScrollSnapshot {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    let document = shell.base();
    document
        .get_node(NodeId::from_script(node))
        .map(|node| ScrollSnapshot::from_node(document, node))
        .unwrap_or_default()
}

#[op2]
#[serde]
pub(super) fn op_dom_image_size(state: &OpState, node: f64) -> Vec<f32> {
    let shell = state.borrow::<SharedShellDocument>().0.borrow();
    shell
        .base()
        .get_node(NodeId::from_script(node))
        .map_or(vec![0.0; 2], |node| {
            LayoutSnapshot::image_size(node).to_vec()
        })
}

#[op2(fast)]
pub(super) fn op_dom_clear_image_mip_chains(state: &OpState) {
    let shared = state.borrow::<SharedShellDocument>();
    shared.0.borrow_mut().clear_image_mip_chains();
}

#[op2(fast)]
pub(super) fn op_dom_create_element(
    state: &OpState,
    #[string] tag: &str,
    #[string] namespace: &str,
) -> f64 {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.create_element(tag, Some(namespace)).to_script()
}

#[op2(fast)]
pub(super) fn op_dom_create_text(state: &OpState, #[string] text: &str) -> f64 {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.create_text(text).to_script()
}

#[op2(fast)]
pub(super) fn op_dom_create_comment(state: &OpState, #[string] text: &str) -> f64 {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.create_comment(text).to_script()
}

#[op2(fast)]
pub(super) fn op_dom_create_fragment(state: &OpState) -> f64 {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.create_fragment().to_script()
}

#[op2(fast)]
pub(super) fn op_dom_template_content(state: &OpState, node: f64) -> f64 {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell
        .template_content(NodeId::from_script(node))
        .to_script()
}

#[op2(fast)]
pub(super) fn op_dom_clone(state: &OpState, node: f64, deep: bool) -> f64 {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell
        .clone_node(NodeId::from_script(node), deep)
        .to_script()
}

#[op2(fast)]
pub(super) fn op_dom_set_inner_html(state: &OpState, node: f64, #[string] html: &str) {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.set_inner_html(NodeId::from_script(node), html);
}

#[op2(fast)]
pub(super) fn op_dom_append_child(state: &OpState, parent: f64, child: f64) {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.append_child(NodeId::from_script(parent), NodeId::from_script(child));
}

#[op2(fast)]
pub(super) fn op_dom_insert_before(state: &OpState, parent: f64, child: f64, before: f64) {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.insert_before(
        NodeId::from_script(parent),
        NodeId::from_script(child),
        NodeId::from_script(before),
    );
}

#[op2(fast)]
pub(super) fn op_dom_remove_child(state: &OpState, parent: f64, child: f64) {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.remove_child(NodeId::from_script(parent), NodeId::from_script(child));
}

#[op2(fast)]
pub(super) fn op_dom_set_text(state: &OpState, node: f64, #[string] text: &str) {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.set_text(NodeId::from_script(node), text);
}

#[op2(fast)]
pub(super) fn op_dom_set_attribute(
    state: &OpState,
    node: f64,
    #[string] name: &str,
    #[string] value: &str,
) {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.set_attribute(NodeId::from_script(node), name, value);
}

#[op2(fast)]
pub(super) fn op_dom_remove_attribute(state: &OpState, node: f64, #[string] name: &str) {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.remove_attribute(NodeId::from_script(node), name);
}

#[op2(fast)]
pub(super) fn op_dom_focus(state: &OpState, node: f64) -> bool {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.focus(NodeId::from_script(node))
}

#[derive(Clone, Copy)]
struct RectSnapshot {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl RectSnapshot {
    const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    };

    fn from_layout(document: &BaseDocument, node: &Node) -> Option<Self> {
        if !node.has_boxes() {
            return Some(Self::ZERO);
        }
        let mut x = -document.viewport_scroll().x as f32;
        let mut y = -document.viewport_scroll().y as f32;
        let mut current = node;
        loop {
            let layout = current.final_layout();
            x += layout.location.x;
            y += layout.location.y;
            let Some(parent) = current.layout_parent.get() else {
                break;
            };
            current = document.get_node(parent)?;
            x -= current.scroll_offset().x as f32;
            y -= current.scroll_offset().y as f32;
        }
        Some(Self {
            x,
            y,
            width: node.final_layout().size.width,
            height: node.final_layout().size.height,
        })
    }
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LayoutSnapshot {
    width: u32,
    height: u32,
    device_pixel_ratio: f32,
    resized: Vec<f64>,
    scrolled: Vec<f64>,
    images: Vec<f64>,
}

pub(super) type LayoutCache = HashMap<NodeId, ([f32; 4], u64, ScrollSnapshot)>;

impl LayoutSnapshot {
    fn image_size(node: &Node) -> [f32; 2] {
        let NodeData::Element(element) = &node.data else {
            return [0.0; 2];
        };
        if let Some(data) = element.custom_widget_data() {
            let sizes = data.widget.intrinsic_sizes();
            return [sizes.width.unwrap_or(0.0), sizes.height.unwrap_or(0.0)];
        }
        if let Some(image) = element.raster_image_data() {
            return [image.width as f32, image.height as f32];
        }
        if let Some(image) = element.svg_data() {
            return [image.size().width(), image.size().height()];
        }
        [0.0; 2]
    }

    fn image_identity(node: &Node) -> u64 {
        match &node.data {
            NodeData::Element(element) if element.custom_widget_data().is_some() => element
                .attr(local_name!("src"))
                .map_or(0, |source| {
                    let mut hasher = std::hash::DefaultHasher::new();
                    std::hash::Hash::hash(source, &mut hasher);
                    std::hash::Hasher::finish(&hasher)
                }),
            NodeData::Element(element) => element
                .raster_image_data()
                .map_or(0, |image| image.data.id()),
            _ => 0,
        }
    }

    pub(super) fn has_changes(&self) -> bool {
        !self.resized.is_empty() || !self.scrolled.is_empty() || !self.images.is_empty()
    }

    pub(super) fn view_state(&self) -> (u32, u32, u32) {
        (self.width, self.height, self.device_pixel_ratio.to_bits())
    }

    pub(super) fn capture(
        document: &BaseDocument,
        width: u32,
        height: u32,
        device_pixel_ratio: f32,
        previous: &mut LayoutCache,
    ) -> Self {
        let mut snapshot = Self {
            width,
            height,
            device_pixel_ratio,
            ..Self::default()
        };
        let mut current = LayoutCache::new();
        for (id, node) in document.tree().iter() {
            if !node.is_element() || !node.flags.is_in_document() {
                continue;
            }
            let size = node.final_layout().size;
            let size = if node.has_boxes() {
                [size.width, size.height]
            } else {
                [0.0; 2]
            };
            let image = Self::image_size(node);
            let geometry = [size[0], size[1], image[0], image[1]];
            let scroll = ScrollSnapshot::from_node(document, node);
            let identity = Self::image_identity(node);
            let (old_geometry, old_identity, old_scroll) =
                previous.get(&id).copied().unwrap_or_default();
            let script_id = id.to_script();
            if old_geometry[..2] != geometry[..2] {
                snapshot.resized.push(script_id);
            }
            if (old_scroll.left, old_scroll.top) != (scroll.left, scroll.top) {
                snapshot.scrolled.push(script_id);
            }
            if old_geometry[2..] != geometry[2..] || old_identity != identity {
                snapshot.images.push(script_id);
            }
            current.insert(id, (geometry, identity, scroll));
        }
        *previous = current;
        snapshot
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
