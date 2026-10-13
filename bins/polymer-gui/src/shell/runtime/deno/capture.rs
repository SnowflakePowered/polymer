use blitz_dom::NodeId;
use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use image::ImageFormat;
use std::{cell::RefCell, rc::Rc};

use super::dom::ScriptNodeId;
use crate::shell::document::SharedShellDocument;

#[op2]
#[buffer]
pub(super) async fn op_surface_capture(
    state: Rc<RefCell<OpState>>,
    node: f64,
    #[string] mime: String,
) -> Result<Option<Vec<u8>>, JsErrorBox> {
    let format = ImageFormat::from_mime_type(&mime)
        .filter(|format| {
            matches!(
                format,
                ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
            )
        })
        .ok_or_else(|| JsErrorBox::type_error(format!("{mime} is not a supported capture type")))?;
    let response = state
        .borrow()
        .borrow::<SharedShellDocument>()
        .0
        .borrow()
        .capture_surface(NodeId::from_script(node))
        .map_err(JsErrorBox::type_error)?;
    let Some(screenshot) = response
        .await
        .map_err(|_| JsErrorBox::generic("the window closed during surface capture"))?
        .map_err(JsErrorBox::generic)?
    else {
        return Ok(None);
    };
    tokio::task::spawn_blocking(move || screenshot.encode(format))
        .await
        .map_err(JsErrorBox::from_err)?
        .map(Some)
        .map_err(|error| JsErrorBox::generic(error.to_string()))
}
