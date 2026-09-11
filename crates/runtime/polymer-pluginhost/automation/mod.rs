use std::{cell::RefCell, rc::Rc};

use ::polymer_automation::{AutomationClient, AutomationRequest};
use deno_core::{JsRuntime, OpState, op2};

#[derive(Default)]
pub(crate) struct AutomationState(Option<AutomationClient>);

#[op2]
#[serde]
pub(crate) async fn op_ui_automation_request(
    state: Rc<RefCell<OpState>>,
    #[serde] request: AutomationRequest,
) -> serde_json::Value {
    let session = state.borrow().borrow::<AutomationState>().0.clone();
    let Some(session) = session else {
        return serde_json::json!({
            "ok": false,
            "error": {
                "kind": "unavailable",
                "message": "UI automation is not attached to an emulator process",
            },
        });
    };
    match session.execute(request).await {
        Ok(value) => serde_json::json!({ "ok": true, "value": value }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    }
}

pub(crate) fn attach(runtime: &mut JsRuntime, session: AutomationClient) {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<AutomationState>()
        .0 = Some(session);
}

pub(crate) fn close(runtime: &mut JsRuntime) {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<AutomationState>()
        .0
        .take();
}

deno_core::extension!(
    polymer_automation,
    ops = [op_ui_automation_request],
    esm_entry_point = "ext:polymer_automation/00_automation.js",
    esm = [dir "automation/js", "00_automation.js"],
    state = |state| state.put(AutomationState::default()),
);

pub(crate) fn extension() -> deno_core::Extension {
    polymer_automation::init()
}
