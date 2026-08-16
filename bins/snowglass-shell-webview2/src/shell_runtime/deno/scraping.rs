use std::{cell::RefCell, path::PathBuf, rc::Rc};

use deno_core::{OpState, op2};
use deno_error::JsErrorBox;

#[op2]
#[serde]
pub(super) async fn op_scraping_package_from_path(
    _state: Rc<RefCell<OpState>>,
    #[string] path: String,
) -> Result<snowglass_runtime::ScrapedPackage, JsErrorBox> {
    tokio::task::spawn_blocking(move || {
        snowglass_runtime::ScrapedPackage::from_path(PathBuf::from(path))
    })
    .await
    .map_err(|error| JsErrorBox::generic(format!("package detection task failed: {error}")))?
    .map_err(|error| JsErrorBox::generic(error.to_string()))
}
