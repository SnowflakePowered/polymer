//! Typed commands crossing the Deno shell and native event-loop boundary.

use crate::{game_host::GameHostCommand, window_manager::WindowCommand};

#[derive(Debug)]
pub(crate) enum UserEvent {
    Window(WindowCommand),
    GameHost(GameHostCommand),
    InspectorWindowRequested { address: std::net::SocketAddr },
    Inspector(crate::inspector::elements::InspectorProtocolCommand),
}

#[derive(Debug)]
pub(crate) struct ShellDocumentSource {
    pub(crate) html: String,
    pub(crate) base_url: String,
}
