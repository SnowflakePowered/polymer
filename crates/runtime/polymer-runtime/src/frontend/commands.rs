//! Native Actix recipients for the application-owned GUI services.

use super::{emulator, input, webrtc};
use actix::{Message, Recipient};
use polymer_deno::polymer::ApiFuture;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct FrontendCommands {
    pub emulator_launch: Option<Recipient<emulator::Launch>>,
    pub emulator_close: Option<Recipient<emulator::Close>>,
    pub emulator_set_volume: Option<Recipient<emulator::SetVolume>>,
    pub emulator_set_muted: Option<Recipient<emulator::SetMuted>>,
    pub emulator_message: Option<Recipient<emulator::Message>>,
    pub emulator_input: Option<Recipient<emulator::Input>>,
    pub emulator_pause_state: Option<Recipient<emulator::PauseState>>,
    pub emulator_save_state: Option<Recipient<emulator::SaveState>>,
    pub emulator_resume_state: Option<Recipient<emulator::ResumeState>>,
    pub emulator_state_screenshot: Option<Recipient<emulator::StateScreenshot>>,
    pub input_configure: Option<Recipient<input::Configure>>,
    pub input_controller_api: Option<Recipient<input::ControllerApi>>,
    pub webrtc_start: Option<Recipient<webrtc::Start>>,
    pub webrtc_stop: Option<Recipient<webrtc::Stop>>,
    /// Interrupts the platform's native event wait after a mailbox send.
    pub wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl FrontendCommands {
    pub(super) fn request<M, R>(
        &self,
        recipient: &Option<Recipient<M>>,
        message: M,
        unavailable: &'static str,
    ) -> ApiFuture<Result<R, String>>
    where
        M: Message<Result = Result<R, String>> + Send + 'static,
        R: Send + 'static,
    {
        let response = recipient.as_ref().map(|recipient| recipient.send(message));
        if let Some(wake) = &self.wake {
            wake();
        }
        Box::pin(async move {
            response
                .ok_or_else(|| unavailable.to_owned())?
                .await
                .map_err(|_| unavailable.to_owned())?
        })
    }

    pub(super) fn notify<M>(&self, recipient: &Option<Recipient<M>>, message: M)
    where
        M: Message<Result = ()> + Send + 'static,
    {
        if let Some(recipient) = recipient {
            recipient.do_send(message);
        }
        if let Some(wake) = &self.wake {
            wake();
        }
    }
}
