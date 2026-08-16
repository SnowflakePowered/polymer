use std::{cell::RefCell, collections::HashSet, net::SocketAddr, rc::Rc, sync::Arc};

use deno_core::{OpState, op2};
use deno_error::JsErrorBox;

use super::InstanceToken;

#[derive(Debug)]
pub enum WebRtcCommand {
    Start {
        token: InstanceToken,
        bind: SocketAddr,
        targets: Vec<u32>,
        result: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    Stop {
        token: InstanceToken,
        result: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Clone)]
pub struct WebRtcCommandSink(Arc<dyn Fn(WebRtcCommand) + Send + Sync>);

impl WebRtcCommandSink {
    pub fn new(send: impl Fn(WebRtcCommand) + Send + Sync + 'static) -> Self {
        Self(Arc::new(send))
    }

    fn send(&self, command: WebRtcCommand) {
        (self.0)(command);
    }
}

impl Default for WebRtcCommandSink {
    fn default() -> Self {
        Self::new(|_| {})
    }
}

pub(super) struct WebRtcBridge {
    pub command_sink: WebRtcCommandSink,
}

impl Default for WebRtcBridge {
    fn default() -> Self {
        Self {
            command_sink: WebRtcCommandSink::default(),
        }
    }
}

#[op2]
pub(super) async fn op_webrtc_start(
    state: Rc<RefCell<OpState>>,
    token: u32,
    #[string] bind: String,
    #[serde] targets: Vec<u32>,
) -> Result<(), JsErrorBox> {
    let bind = bind
        .parse::<SocketAddr>()
        .map_err(|error| JsErrorBox::type_error(format!("invalid WebRTC bind address: {error}")))?;
    if targets.is_empty() {
        return Err(JsErrorBox::type_error(
            "WebRTC publication requires at least one render target",
        ));
    }
    if targets.iter().copied().collect::<HashSet<_>>().len() != targets.len() {
        return Err(JsErrorBox::type_error(
            "WebRTC render-target indices must be unique",
        ));
    }
    let command_sink = state.borrow().borrow::<WebRtcBridge>().command_sink.clone();
    let (result, response) = tokio::sync::oneshot::channel();
    command_sink.send(WebRtcCommand::Start {
        token: InstanceToken(token),
        bind,
        targets,
        result,
    });
    response
        .await
        .map_err(|_| JsErrorBox::generic("WebRTC coordinator is unavailable"))?
        .map_err(JsErrorBox::generic)
}

#[op2]
pub(super) async fn op_webrtc_stop(
    state: Rc<RefCell<OpState>>,
    token: u32,
) -> Result<(), JsErrorBox> {
    let command_sink = state.borrow().borrow::<WebRtcBridge>().command_sink.clone();
    let (result, response) = tokio::sync::oneshot::channel();
    command_sink.send(WebRtcCommand::Stop {
        token: InstanceToken(token),
        result,
    });
    response
        .await
        .map_err(|_| JsErrorBox::generic("WebRTC coordinator is unavailable"))?
        .map_err(JsErrorBox::generic)
}
