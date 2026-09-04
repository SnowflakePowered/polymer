use std::{collections::BTreeMap, sync::Arc};

use deno_core::{OpState, op2};
use deno_error::JsErrorBox;

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ParsedHotkey {
    Keyboard {
        code: String,
        #[serde(default)]
        modifiers: Vec<String>,
    },
    Binding {
        player: u8,
        keys: Vec<String>,
    },
}

pub(crate) type ActionSink = Arc<dyn Fn(&ParsedHotkey) -> Result<(), String> + Send + Sync>;

#[derive(Default)]
pub(crate) struct ActionsState {
    actions: BTreeMap<String, ParsedHotkey>,
    sink: Option<ActionSink>,
}

impl ActionsState {
    pub(crate) fn attach(&mut self, sink: ActionSink) {
        self.sink = Some(sink);
    }

    pub(crate) fn close(&mut self) {
        self.sink = None;
    }
}

pub(crate) fn attach(runtime: &mut deno_core::JsRuntime, sink: ActionSink) {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<ActionsState>()
        .attach(sink);
}

pub(crate) fn close(runtime: &mut deno_core::JsRuntime) {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<ActionsState>()
        .close();
}

#[op2]
pub(crate) fn op_actions_register(
    state: &mut OpState,
    #[serde] actions: BTreeMap<String, ParsedHotkey>,
) -> Result<(), JsErrorBox> {
    for (name, hotkey) in &actions {
        if name.is_empty() {
            return Err(JsErrorBox::type_error("action names cannot be empty"));
        }
        match hotkey {
            ParsedHotkey::Keyboard { code, modifiers } => {
                if code.is_empty() || modifiers.iter().any(String::is_empty) {
                    return Err(JsErrorBox::type_error(format!(
                        "action '{name}' contains an empty keyboard code"
                    )));
                }
            }
            ParsedHotkey::Binding { player, keys } => {
                if *player >= 8 {
                    return Err(JsErrorBox::type_error(format!(
                        "action '{name}' binding player must be between 0 and 7"
                    )));
                }
                if keys.is_empty() {
                    return Err(JsErrorBox::type_error(format!(
                        "action '{name}' contains an empty binding chord"
                    )));
                }
                for key in keys {
                    let parsed =
                        key.parse::<consolespec::input::BindingKey>()
                            .map_err(|error| {
                                JsErrorBox::type_error(format!("action '{name}': {error}"))
                            })?;
                    if !matches!(parsed, consolespec::input::BindingKey::Button(_)) {
                        return Err(JsErrorBox::type_error(format!(
                            "action '{name}' binding '{key}' is not a button"
                        )));
                    }
                }
            }
        }
    }
    state.borrow_mut::<ActionsState>().actions = actions;
    Ok(())
}

#[op2(fast)]
pub(crate) fn op_action_activate(
    state: &mut OpState,
    #[string] name: String,
) -> Result<(), JsErrorBox> {
    let actions = state.borrow::<ActionsState>();
    let hotkey = actions
        .actions
        .get(&name)
        .ok_or_else(|| JsErrorBox::type_error(format!("unknown action '{name}'")))?;
    let sink = actions
        .sink
        .as_ref()
        .ok_or_else(|| JsErrorBox::generic("emulator action endpoint is unavailable"))?;
    sink(hotkey).map_err(JsErrorBox::generic)
}

deno_core::extension!(
    snowglass_actions,
    ops = [op_actions_register, op_action_activate],
    esm_entry_point = "ext:snowglass_actions/00_actions.js",
    esm = [dir "actions/js", "00_actions.js"],
    state = |state| state.put(ActionsState::default()),
);

pub(crate) fn extension() -> deno_core::Extension {
    snowglass_actions::init()
}
