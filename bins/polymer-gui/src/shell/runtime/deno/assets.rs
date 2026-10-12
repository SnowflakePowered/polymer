use blitz_traits::net::Url;
use deno_core::{OpState, ToJsBuffer, op2};
use deno_error::JsErrorBox;
use serde::Serialize;
use std::{cell::RefCell, rc::Rc};

use crate::shell::document::SharedShellDocument;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ShellAsset {
    body: ToJsBuffer,
    content_type: String,
}

#[op2]
#[string]
pub(super) fn op_shell_resolve_asset(state: &OpState, #[string] url: String) -> Option<String> {
    let url = Url::parse(&url).ok()?;
    let urls = state.borrow::<SharedShellDocument>().0.borrow().urls();
    urls.resolve(&url).map(String::from)
}

#[op2]
#[serde]
pub(super) async fn op_shell_read_asset(
    state: Rc<RefCell<OpState>>,
    #[string] url: String,
) -> Result<ShellAsset, JsErrorBox> {
    let urls = state
        .borrow()
        .borrow::<SharedShellDocument>()
        .0
        .borrow()
        .urls();
    let path = Url::parse(&url)
        .ok()
        .and_then(|parsed| urls.local_file(&parsed))
        .ok_or_else(|| JsErrorBox::type_error(format!("{url} is not a local shell asset")))?;
    let content_type = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string();
    let body = tokio::fs::read(&path).await.map_err(JsErrorBox::from_err)?;
    Ok(ShellAsset {
        body: body.into(),
        content_type,
    })
}
