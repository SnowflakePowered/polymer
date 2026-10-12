//! Native Actix recipients for the application-owned GUI services.

use super::{emulator, input};
use actix::{Message, Recipient};
use polymer_deno::polymer::ApiFuture;
use std::sync::Arc;

/// A complete set of recipients supplied by the GUI.
#[derive(Clone)]
pub struct FrontendRecipients {
    pub emulator: EmulatorRecipients,
    pub input: InputRecipients,
}

#[derive(Clone)]
pub struct EmulatorRecipients {
    pub launch: Recipient<emulator::Launch>,
    pub close: Recipient<emulator::Close>,
    pub set_volume: Recipient<emulator::SetVolume>,
    pub set_muted: Recipient<emulator::SetMuted>,
    pub message: Recipient<emulator::Message>,
    pub input: Recipient<emulator::Input>,
    pub pause_state: Recipient<emulator::PauseState>,
    pub change_disc: Recipient<emulator::ChangeDisc>,
    pub save_state: Recipient<emulator::SaveState>,
    pub resume_state: Recipient<emulator::ResumeState>,
    pub state_screenshot: Recipient<emulator::StateScreenshot>,
}

#[derive(Clone)]
pub struct InputRecipients {
    pub configure: Recipient<input::Configure>,
    pub controller_api: Recipient<input::ControllerApi>,
}

#[derive(Clone)]
pub struct FrontendCommands {
    connection: Option<Arc<FrontendConnection>>,
}

struct FrontendConnection {
    recipients: FrontendRecipients,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl FrontendCommands {
    pub fn new(recipients: FrontendRecipients, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            connection: Some(Arc::new(FrontendConnection {
                recipients,
                wake: Box::new(wake),
            })),
        }
    }

    /// Build services without a GUI host, for headless use and tests.
    pub fn disconnected() -> Self {
        Self { connection: None }
    }

    pub(super) fn request<M, R>(
        &self,
        recipient: impl FnOnce(&FrontendRecipients) -> &Recipient<M>,
        message: M,
        unavailable: &'static str,
    ) -> ApiFuture<Result<R, String>>
    where
        M: Message<Result = Result<R, String>> + Send + 'static,
        R: Send + 'static,
    {
        let response = self.connection.as_ref().map(|connection| {
            let response = recipient(&connection.recipients).send(message);
            (connection.wake)();
            response
        });
        Box::pin(async move {
            response
                .ok_or_else(|| unavailable.to_owned())?
                .await
                .map_err(|_| unavailable.to_owned())?
        })
    }

    pub(super) fn notify<M>(
        &self,
        recipient: impl FnOnce(&FrontendRecipients) -> &Recipient<M>,
        message: M,
    ) where
        M: Message<Result = ()> + Send + 'static,
    {
        if let Some(connection) = &self.connection {
            recipient(&connection.recipients).do_send(message);
            (connection.wake)();
        }
    }
}
