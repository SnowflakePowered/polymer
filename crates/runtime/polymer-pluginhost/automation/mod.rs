use std::{cell::RefCell, rc::Rc};

use ::polymer_automation::{
    AutomationClient, AutomationError, AutomationRequest, MessageBoxes, WatchMessageBoxes,
};
use deno_core::{JsRuntime, OpState, op2};
use serde::Serialize;

#[derive(Default)]
pub(crate) struct AutomationState {
    client: Option<AutomationClient>,
    message_boxes: Rc<tokio::sync::Mutex<Option<MessageBoxes>>>,
}

impl AutomationState {
    fn client(state: &Rc<RefCell<OpState>>) -> Result<AutomationClient, AutomationError> {
        state
            .borrow()
            .borrow::<Self>()
            .client
            .clone()
            .ok_or_else(|| {
                AutomationError::new(
                    "unavailable",
                    "UI automation is not attached to an emulator process",
                )
            })
    }

    fn respond(result: Result<impl Serialize, AutomationError>) -> serde_json::Value {
        match result {
            Ok(value) => serde_json::json!({ "ok": true, "value": value }),
            Err(error) => serde_json::json!({ "ok": false, "error": error }),
        }
    }
}

#[op2]
#[serde]
pub(crate) async fn op_ui_automation_request(
    state: Rc<RefCell<OpState>>,
    #[serde] request: AutomationRequest,
) -> serde_json::Value {
    let result = match AutomationState::client(&state) {
        Ok(client) => client
            .send(request)
            .await
            .map_err(AutomationError::from)
            .and_then(|result| result),
        Err(error) => Err(error),
    };
    AutomationState::respond(result)
}

#[op2]
#[serde]
pub(crate) async fn op_ui_automation_next_message_box(
    state: Rc<RefCell<OpState>>,
) -> serde_json::Value {
    let client = match AutomationState::client(&state) {
        Ok(client) => client,
        Err(error) => return AutomationState::respond(Err::<(), _>(error)),
    };
    let message_boxes = state
        .borrow()
        .borrow::<AutomationState>()
        .message_boxes
        .clone();
    let mut message_boxes = message_boxes.lock().await;
    if message_boxes.is_none() {
        match client
            .send(WatchMessageBoxes)
            .await
            .map_err(AutomationError::from)
            .and_then(|result| result)
        {
            Ok(watch) => *message_boxes = Some(watch),
            Err(error) => return AutomationState::respond(Err::<(), _>(error)),
        }
    }
    let next = message_boxes
        .as_mut()
        .expect("message box watch was attached")
        .next()
        .await;
    if next.is_none() {
        message_boxes.take();
    }
    AutomationState::respond(Ok(next))
}

pub(crate) fn attach(runtime: &mut JsRuntime, session: AutomationClient) {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<AutomationState>()
        .client = Some(session);
}

pub(crate) fn close(runtime: &mut JsRuntime) {
    *runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<AutomationState>() = AutomationState::default();
}

deno_core::extension!(
    polymer_automation,
    ops = [op_ui_automation_request, op_ui_automation_next_message_box],
    esm_entry_point = "ext:polymer_automation/00_automation.js",
    esm = [dir "automation/js", "00_automation.js"],
    state = |state| state.put(AutomationState::default()),
);

pub(crate) fn extension() -> deno_core::Extension {
    polymer_automation::init()
}
