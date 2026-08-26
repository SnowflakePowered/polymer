//! Blitz-backed DOM, Page, and Schema protocol handling.

use super::{css::Stylesheets, geometry::NodeGeometry};
use crate::shell_dom::ShellSession;
use blitz_dom::{BaseDocument, Node, NodeData};
use serde_json::{Value, json};
use tokio::sync::oneshot;

const DOCUMENT_NODE: u64 = 9;
const ELEMENT_NODE: u64 = 1;
const TEXT_NODE: u64 = 3;
const COMMENT_NODE: u64 = 8;

#[derive(Debug)]
pub(crate) struct Command {
    pub(super) request: Value,
    pub(super) target_url: String,
    pub(super) include_whitespace: bool,
    pub(super) respond_to: oneshot::Sender<Reply>,
}

impl Command {
    pub(crate) fn handle(self, session: &ShellSession) {
        let document = session.inspector_document();
        let result = document
            .ok_or_else(|| ProtocolError::unavailable("The Blitz document is not active"))
            .and_then(|document| {
                DocumentProtocol::new(document, &self.target_url, self.include_whitespace)
                    .handle(&self.request)
            });
        let events = document
            .map(|document| {
                DocumentProtocol::new(document, &self.target_url, self.include_whitespace)
                    .events_for(&self.request)
            })
            .unwrap_or_default();
        let _ = self.respond_to.send(Reply { result, events });
    }
}

#[derive(Debug)]
pub(super) struct Reply {
    pub(super) result: Result<Value, ProtocolError>,
    pub(super) events: Vec<Value>,
}

#[derive(Debug)]
pub(super) struct ProtocolError {
    pub(super) code: i64,
    pub(super) message: String,
}

impl ProtocolError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }

    pub(super) fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: -32000,
            message: message.into(),
        }
    }
}

pub(super) struct DocumentProtocol<'a> {
    document: &'a BaseDocument,
    target_url: &'a str,
    include_whitespace: bool,
}

impl<'a> DocumentProtocol<'a> {
    fn new(document: &'a BaseDocument, target_url: &'a str, include_whitespace: bool) -> Self {
        Self {
            document,
            target_url,
            include_whitespace,
        }
    }

    fn handle(&self, request: &Value) -> Result<Value, ProtocolError> {
        let document = self.document;
        let target_url = self.target_url;
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| ProtocolError::invalid("CDP request has no method"))?;
        let params = RequestParams::new(request.get("params").unwrap_or(&Value::Null));
        tracing::debug!(method, "snowglass: Elements inspector command");
        match method {
            "DOM.enable"
            | "DOM.disable"
            | "CSS.enable"
            | "CSS.disable"
            | "Overlay.enable"
            | "Overlay.disable"
            | "Overlay.hideHighlight"
            | "Overlay.highlightNode"
            | "Overlay.highlightQuad"
            | "Page.enable"
            | "Page.disable"
            | "Page.setLifecycleEventsEnabled" => Ok(json!({})),
            "DOM.getDocument" => Ok(json!({
                "root": self.protocol_node(document.root_node(), true)
            })),
            "DOM.describeNode" => {
                let node = self.request_node(params.value())?;
                Ok(json!({"node": self.protocol_node(node, true)}))
            }
            "DOM.requestChildNodes" => Ok(json!({})),
            "DOM.getAttributes" => {
                let node = self.request_node(params.value())?;
                Ok(json!({"attributes": Self::attributes(node)}))
            }
            "DOM.getOuterHTML" => {
                let node = self.request_node(params.value())?;
                Ok(json!({"outerHTML": node.outer_html()}))
            }
            "DOM.querySelector" => {
                let selector = params.string("selector")?;
                let node_id = document
                    .query_selector(selector)
                    .map_err(|_| ProtocolError::invalid("Invalid CSS selector"))?
                    .map(Self::protocol_id)
                    .unwrap_or(0);
                Ok(json!({"nodeId": node_id}))
            }
            "DOM.querySelectorAll" => {
                let selector = params.string("selector")?;
                let node_ids = document
                    .query_selector_all(selector)
                    .map_err(|_| ProtocolError::invalid("Invalid CSS selector"))?
                    .into_iter()
                    .map(Self::protocol_id)
                    .collect::<Vec<_>>();
                Ok(json!({"nodeIds": node_ids}))
            }
            "DOM.pushNodesByBackendIdsToFrontend" => {
                let node_ids = params
                    .value()
                    .get("backendNodeIds")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_u64)
                    .filter(|id| {
                        Self::blitz_id(*id).is_some_and(|id| document.get_node(id).is_some())
                    })
                    .collect::<Vec<_>>();
                Ok(json!({"nodeIds": node_ids}))
            }
            "DOM.getBoxModel" => {
                let node = self.request_node(params.value())?;
                Ok(json!({"model": NodeGeometry::new(node).box_model()}))
            }
            "DOM.getContentQuads" => {
                let node = self.request_node(params.value())?;
                Ok(json!({"quads": [NodeGeometry::new(node).content_quad()]}))
            }
            "DOM.getNodeForLocation" => {
                let x = params.number("x")? as f32;
                let y = params.number("y")? as f32;
                let node = document
                    .hit(x, y)
                    .and_then(|hit| document.get_node(hit.node_id))
                    .ok_or_else(|| ProtocolError::unavailable("No node exists at that location"))?;
                Ok(json!({
                    "backendNodeId": Self::protocol_id(node.id),
                    "nodeId": Self::protocol_id(node.id),
                    "frameId": "snowglass-main"
                }))
            }
            "CSS.getComputedStyleForNode" => {
                let node = self.request_node(params.value())?;
                Ok(json!({"computedStyle": Stylesheets::new(document).computed_style(node)}))
            }
            "CSS.getInlineStylesForNode" => {
                let node = self.request_node(params.value())?;
                Ok(json!({"inlineStyle": Stylesheets::inline_style(node)}))
            }
            "CSS.getMatchedStylesForNode" => {
                let node = self.request_node(params.value())?;
                Ok(json!({
                "inlineStyle": Stylesheets::inline_style(node),
                "matchedCSSRules": Stylesheets::new(document).matched_styles(node),
                    "inherited": [],
                    "inheritedPseudoElements": [],
                    "pseudoElements": []
                }))
            }
            "CSS.getStyleSheetText" => {
                let id = params.string("styleSheetId")?;
                let sheet = Stylesheets::new(document)
                    .all()
                    .into_iter()
                    .find(|sheet| sheet.id == id)
                    .ok_or_else(|| ProtocolError::unavailable("The stylesheet no longer exists"))?;
                Ok(json!({"text": sheet.text}))
            }
            "CSS.getMediaQueries" => Ok(json!({"medias": []})),
            "CSS.getEnvironmentVariables" => Ok(json!({"environmentVariables": []})),
            "CSS.getPlatformFontsForNode" => Ok(json!({"fonts": []})),
            "CSS.getBackgroundColors" => Ok(json!({"backgroundColors": []})),
            "Page.getFrameTree" | "Page.getResourceTree" => {
                let frame_tree = self.frame_tree();
                if method == "Page.getResourceTree" {
                    Ok(json!({"frameTree": frame_tree}))
                } else {
                    Ok(json!({"frameTree": frame_tree}))
                }
            }
            "Page.getNavigationHistory" => Ok(json!({
                "currentIndex": 0,
                "entries": [{"id": 1, "url": target_url, "userTypedURL": target_url, "title": "Snowglass"}]
            })),
            "Page.getAppManifest" => Ok(json!({"url": "", "errors": [], "data": ""})),
            "Page.getInstallabilityErrors" => Ok(json!({"installabilityErrors": []})),
            "Schema.getDomains" => Ok(json!({"domains": Self::protocol_domains()})),
            method
                if method.starts_with("DOM.set")
                    || method.starts_with("DOM.remove")
                    || method.starts_with("DOM.move")
                    || method.starts_with("CSS.set") =>
            {
                Err(ProtocolError::unavailable(
                    "Editing the Blitz DOM from DevTools is not supported yet",
                ))
            }
            _ => Ok(json!({})),
        }
    }

    fn events_for(&self, request: &Value) -> Vec<Value> {
        if request.get("method").and_then(Value::as_str) != Some("CSS.enable") {
            return Vec::new();
        }
        Stylesheets::new(self.document)
            .all()
            .into_iter()
            .map(|sheet| {
                let end = sheet.position(sheet.text.len());
                json!({
                    "method": "CSS.styleSheetAdded",
                    "params": {
                        "header": {
                            "styleSheetId": sheet.id,
                            "frameId": "snowglass-main",
                            "sourceURL": sheet.source_url,
                            "sourceMapURL": "",
                            "origin": "regular",
                            "title": sheet.title,
                            "disabled": false,
                            "isInline": true,
                            "isMutable": false,
                            "isConstructed": false,
                            "startLine": 0,
                            "startColumn": 0,
                            "endLine": end.0,
                            "endColumn": end.1,
                            "length": sheet.text.len(),
                            "loadingFailed": false,
                            "ownerNode": Self::protocol_id(sheet.node_id),
                            "hasSourceURL": false,
                            "targetURL": self.target_url,
                        }
                    }
                })
            })
            .collect()
    }

    fn protocol_node(&self, node: &Node, deep: bool) -> Value {
        let (node_type, node_name, local_name, node_value) = match &node.data {
            NodeData::Document => (DOCUMENT_NODE, "#document".into(), "".into(), "".into()),
            NodeData::Element(element) | NodeData::AnonymousBlock(element) => (
                ELEMENT_NODE,
                element.name.local.as_ref().to_ascii_uppercase(),
                element.name.local.as_ref().to_owned(),
                String::new(),
            ),
            NodeData::Text(text) => (TEXT_NODE, "#text".into(), "".into(), text.content.clone()),
            NodeData::Comment => (COMMENT_NODE, "#comment".into(), "".into(), "".into()),
        };
        let visible_children = node
            .children
            .iter()
            .filter_map(|id| self.document.get_node(*id))
            .filter(|child| self.include_whitespace || !Self::is_whitespace_only(child))
            .collect::<Vec<_>>();
        let child_node_count = visible_children.len();
        let children = deep
            .then(|| {
                visible_children
                    .into_iter()
                    .map(|child| self.protocol_node(child, true))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut value = json!({
            "nodeId": Self::protocol_id(node.id),
            "backendNodeId": Self::protocol_id(node.id),
            "nodeType": node_type,
            "nodeName": node_name,
            "localName": local_name,
            "nodeValue": node_value,
            "childNodeCount": child_node_count,
            "children": children,
            "attributes": Self::attributes(node),
        });
        if matches!(node.data, NodeData::Document) {
            value["documentURL"] = Value::String(self.target_url.to_owned());
            value["baseURL"] = Value::String(self.target_url.to_owned());
        }
        value
    }

    fn is_whitespace_only(node: &Node) -> bool {
        matches!(&node.data, NodeData::Text(text) if text.content.trim().is_empty())
    }

    pub(super) fn protocol_id(id: usize) -> u64 {
        id as u64 + 1
    }

    fn blitz_id(id: u64) -> Option<usize> {
        id.checked_sub(1).and_then(|id| usize::try_from(id).ok())
    }

    fn request_node(&self, params: &Value) -> Result<&'a Node, ProtocolError> {
        let id = params
            .get("nodeId")
            .or_else(|| params.get("backendNodeId"))
            .and_then(Value::as_u64)
            .and_then(Self::blitz_id)
            .ok_or_else(|| ProtocolError::invalid("A valid nodeId is required"))?;
        self.document
            .get_node(id)
            .ok_or_else(|| ProtocolError::unavailable("The requested DOM node no longer exists"))
    }

    fn attributes(node: &Node) -> Vec<String> {
        node.attrs()
            .into_iter()
            .flatten()
            .flat_map(|attribute| {
                [
                    attribute.name.local.as_ref().to_owned(),
                    attribute.value.clone(),
                ]
            })
            .collect()
    }
}

impl DocumentProtocol<'_> {
    fn frame_tree(&self) -> Value {
        let target_url = self.target_url;
        json!({
            "frame": {
                "id": "snowglass-main",
                "loaderId": "snowglass-loader",
                "url": target_url,
                "domainAndRegistry": "",
                "securityOrigin": target_url,
                "mimeType": "text/html",
                "secureContextType": "Secure",
                "crossOriginIsolatedContextType": "NotIsolated",
                "gatedAPIFeatures": []
            },
            "resources": []
        })
    }

    fn protocol_domains() -> Vec<Value> {
        [
            "Runtime",
            "Debugger",
            "Profiler",
            "HeapProfiler",
            "Schema",
            "DOM",
            "CSS",
            "Overlay",
            "Page",
        ]
        .into_iter()
        .map(|name| json!({"name": name, "version": "1.3"}))
        .collect()
    }
}

struct RequestParams<'a> {
    value: &'a Value,
}

impl<'a> RequestParams<'a> {
    fn new(value: &'a Value) -> Self {
        Self { value }
    }

    fn value(&self) -> &'a Value {
        self.value
    }

    fn string(&self, name: &str) -> Result<&'a str, ProtocolError> {
        self.value
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| ProtocolError::invalid(format!("{name} must be a string")))
    }

    fn number(&self, name: &str) -> Result<f64, ProtocolError> {
        self.value
            .get(name)
            .and_then(Value::as_f64)
            .ok_or_else(|| ProtocolError::invalid(format!("{name} must be a number")))
    }
}
