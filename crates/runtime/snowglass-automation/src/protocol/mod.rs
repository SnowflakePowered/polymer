use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct WindowId(pub String);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextMatch {
    #[serde(default)]
    pub equals: Option<String>,
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default)]
    pub case_sensitive: bool,
}

impl TextMatch {
    pub fn matches(&self, candidate: &str) -> bool {
        let normalize = |value: &str| {
            if self.case_sensitive {
                value.to_owned()
            } else {
                value.to_lowercase()
            }
        };
        let candidate = normalize(candidate);
        self.equals
            .as_deref()
            .is_none_or(|expected| candidate == normalize(expected))
            && self
                .contains
                .as_deref()
                .is_none_or(|expected| candidate.contains(&normalize(expected)))
    }

    pub fn validate(&self) -> Result<(), AutomationError> {
        if self.equals.is_some() == self.contains.is_some() {
            return Err(AutomationError::invalid_argument(
                "text match requires exactly one of equals or contains",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowQuery {
    pub title: Option<TextMatch>,
    pub class_name: Option<TextMatch>,
    pub visible: Option<bool>,
}

impl WindowQuery {
    pub fn validate(&self) -> Result<(), AutomationError> {
        if let Some(value) = &self.title {
            value.validate()?;
        }
        if let Some(value) = &self.class_name {
            value.validate()?;
        }
        Ok(())
    }

    pub fn matches(&self, window: &WindowSnapshot) -> bool {
        self.title
            .as_ref()
            .is_none_or(|query| query.matches(&window.title))
            && self
                .class_name
                .as_ref()
                .is_none_or(|query| query.matches(&window.class_name))
            && self.visible.is_none_or(|visible| window.visible == visible)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowSnapshot {
    pub id: WindowId,
    pub process_id: u32,
    pub title: String,
    pub class_name: String,
    pub visible: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeSnapshot {
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    pub bounds: Option<Bounds>,
    pub actions: Vec<String>,
    pub states: serde_json::Value,
    pub numeric_value: Option<f64>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub stable_id: Option<String>,
    pub process_id: Option<u32>,
    pub raw: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<NodeSnapshot>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeOptions {
    #[serde(default = "TreeOptions::default_depth")]
    pub max_depth: usize,
    #[serde(default = "TreeOptions::default_nodes")]
    pub max_nodes: usize,
}

impl TreeOptions {
    const fn default_depth() -> usize {
        8
    }
    const fn default_nodes() -> usize {
        1_000
    }

    pub fn validate(self) -> Result<Self, AutomationError> {
        if self.max_nodes == 0 || self.max_nodes > 10_000 {
            return Err(AutomationError::invalid_argument(
                "maxNodes must be between 1 and 10000",
            ));
        }
        Ok(self)
    }
}

impl Default for TreeOptions {
    fn default() -> Self {
        Self {
            max_depth: Self::default_depth(),
            max_nodes: Self::default_nodes(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WaitCondition {
    Attached,
    Detached,
    Visible,
    Hidden,
    Enabled,
    Focused,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LocatorAction {
    Press,
    Focus,
    Blur,
    Toggle,
    Select,
    Expand,
    Collapse,
    ShowMenu,
    Increment,
    Decrement,
    ScrollIntoView,
    SetValue(String),
    SetNumericValue(f64),
    TypeText(String),
    SetTextSelection { start: u32, end: u32 },
    Perform(String),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum InputCommand {
    LogicalFocus,
    KeyDown {
        code: String,
    },
    KeyUp {
        code: String,
    },
    KeyPress {
        code: String,
        #[serde(default)]
        modifiers: Vec<String>,
        #[serde(default)]
        repeat: bool,
    },
    TypeText {
        text: String,
    },
    MouseMove {
        x: i32,
        y: i32,
    },
    MouseDown {
        button: String,
        x: i32,
        y: i32,
    },
    MouseUp {
        button: String,
        x: i32,
        y: i32,
    },
    MouseClick {
        button: String,
        count: u32,
        x: i32,
        y: i32,
    },
    MouseWheel {
        delta_x: i32,
        delta_y: i32,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventFilter {
    #[serde(default)]
    pub types: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiEvent {
    pub event_type: String,
    pub target: Option<NodeSnapshot>,
    pub state: Option<String>,
    pub value: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AutomationRequest {
    Windows {
        #[serde(default)]
        query: WindowQuery,
    },
    WaitForWindow {
        query: WindowQuery,
        timeout_ms: u64,
    },
    WindowSnapshot {
        window: WindowId,
    },
    Tree {
        window: WindowId,
        #[serde(default)]
        options: TreeOptions,
    },
    LocatorCount {
        window: WindowId,
        selector: String,
    },
    LocatorAll {
        window: WindowId,
        selector: String,
        limit: usize,
    },
    LocatorWait {
        window: WindowId,
        selector: String,
        nth: Option<usize>,
        condition: WaitCondition,
        timeout_ms: u64,
    },
    LocatorAction {
        window: WindowId,
        selector: String,
        nth: Option<usize>,
        action: LocatorAction,
        timeout_ms: u64,
    },
    Input {
        window: WindowId,
        command: InputCommand,
    },
    Subscribe {
        window: WindowId,
        #[serde(default)]
        filter: EventFilter,
    },
    NextEvent {
        subscription: u64,
        timeout_ms: u64,
    },
    CloseEvents {
        subscription: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum AutomationResponse {
    Windows(Vec<WindowSnapshot>),
    Window(WindowSnapshot),
    Node(NodeSnapshot),
    Nodes(Vec<NodeSnapshot>),
    Count(usize),
    Tree { root: NodeSnapshot, truncated: bool },
    Subscription(u64),
    Event(Option<UiEvent>),
    Unit,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnosis {
    pub selector: Option<String>,
    pub condition: Option<String>,
    pub last_observed: Option<String>,
    #[serde(default)]
    pub candidates: Vec<String>,
    pub scope: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, thiserror::Error)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub struct AutomationError {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnosis: Option<Box<Diagnosis>>,
}

impl AutomationError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            diagnosis: None,
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new("invalidArgument", message)
    }

    pub fn platform(message: impl Into<String>) -> Self {
        Self::new("platform", message)
    }

    pub fn process_exited() -> Self {
        Self::new("processExited", "the emulator automation session is closed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn javascript_request_shape_round_trips_camel_case_fields_and_actions() {
        let request = serde_json::json!({
            "type": "locatorAction",
            "window": "window-2",
            "selector": "combo_box[name=Controller]",
            "nth": 2,
            "action": { "setTextSelection": { "start": 1, "end": 4 } },
            "timeoutMs": 7500,
        });
        let decoded: AutomationRequest =
            serde_json::from_value(request.clone()).expect("decode JavaScript request");
        let AutomationRequest::LocatorAction {
            window,
            nth,
            action,
            timeout_ms,
            ..
        } = &decoded
        else {
            panic!("decoded the wrong request variant");
        };
        assert_eq!(window.0, "window-2");
        assert_eq!(*nth, Some(2));
        assert_eq!(*timeout_ms, 7_500);
        assert!(matches!(
            action,
            LocatorAction::SetTextSelection { start: 1, end: 4 }
        ));
        assert_eq!(
            serde_json::to_value(decoded).expect("encode JavaScript request"),
            request
        );
    }

    #[test]
    fn response_tag_keeps_tree_metadata_beside_the_root() {
        let response = AutomationResponse::Tree {
            root: NodeSnapshot {
                role: "window".to_owned(),
                name: Some("Settings".to_owned()),
                value: None,
                description: None,
                bounds: None,
                actions: Vec::new(),
                states: serde_json::json!({}),
                numeric_value: None,
                min_value: None,
                max_value: None,
                stable_id: None,
                process_id: Some(42),
                raw: serde_json::json!({}),
                children: Vec::new(),
            },
            truncated: true,
        };
        let encoded = serde_json::to_value(response).expect("encode tree response");
        assert_eq!(encoded["type"], "tree");
        assert_eq!(encoded["value"]["root"]["name"], "Settings");
        assert_eq!(encoded["value"]["truncated"], true);
    }
}
