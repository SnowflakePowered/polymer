//! Typed commands crossing the Deno shell and native event-loop boundary.

use crate::{
    cli::GameLaunchRequest, game_host::GameHostCommand, launcher, window_manager::WindowCommand,
};

#[derive(Debug)]
pub(crate) enum UserEvent {
    Window(WindowCommand),
    Runtime(RuntimeCommand),
    GameHost(GameHostCommand),
}

#[derive(Debug)]
pub(crate) enum RuntimeCommand {
    LauncherOpen(GameLaunchRequest),
    LauncherStart(launcher::LauncherStartRequest),
    LauncherCancel,
    LauncherSettle {
        id: u64,
        error: Option<String>,
    },
    GameDetected {
        generation: u64,
        result: Result<launcher::DetectedGame, String>,
    },
}

#[derive(Debug)]
pub(crate) struct ShellDocumentSource {
    pub(crate) html: String,
    pub(crate) base_url: String,
}
