use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use std::{cell::RefCell, future::Future, pin::Pin, rc::Rc};

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DigestSource {
    Path {
        path: String,
        binary: bool,
    },
    Handle {
        token: String,
        binary: bool,
    },
    Cache {
        key: String,
        path: String,
    },
    Resource {
        kind: String,
        token: String,
        components: Vec<String>,
    },
}

pub type DigestFuture = Pin<Box<dyn Future<Output = Result<String, JsErrorBox>>>>;

/// Each realm resolves sources using its own host capabilities and permissions.
pub struct DigestHandler(
    pub fn(&mut OpState, DigestSource, String) -> Result<DigestFuture, JsErrorBox>,
);

#[op2]
#[string]
async fn op_digest(
    state: Rc<RefCell<OpState>>,
    #[serde] source: DigestSource,
    #[string] algorithm: String,
) -> Result<String, JsErrorBox> {
    let pending = {
        let mut state = state.borrow_mut();
        let handler = state
            .try_borrow::<DigestHandler>()
            .ok_or_else(|| JsErrorBox::type_error("digest sources are unavailable in this realm"))?
            .0;
        handler(&mut state, source, algorithm)?
    };
    pending.await
}

deno_core::extension!(polymer_digest_op, ops = [op_digest]);

impl DigestHandler {
    pub(crate) fn extension() -> deno_core::Extension {
        polymer_digest_op::init()
    }
}
