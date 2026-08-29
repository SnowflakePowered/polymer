use std::{cell::RefCell, rc::Rc};

use deno_core::{JsRuntime, OpState, op2};
use snowglass_automation::{AutomationRequest, AutomationSession};

#[derive(Default)]
pub(crate) struct AutomationState(Option<AutomationSession>);

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

pub(crate) fn attach(runtime: &mut JsRuntime, session: AutomationSession) {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<AutomationState>()
        .0 = Some(session);
}

pub(crate) fn close(runtime: &mut JsRuntime) {
    if let Some(session) = runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<AutomationState>()
        .0
        .take()
    {
        session.close();
    }
}
