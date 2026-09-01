use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct ControllerSession {
    pub(crate) commands: std::sync::mpsc::Sender<ControllerSessionCommand>,
    errors: Arc<Mutex<Vec<SessionError>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionError {
    ControllerChanged(String),
    PauseResume(String),
}

impl SessionError {
    pub fn message(&self) -> &str {
        match self {
            Self::ControllerChanged(message) | Self::PauseResume(message) => message,
        }
    }
}

pub(crate) enum ControllerSessionCommand {
    AttachAutomation(
        snowglass_automation::AutomationSession,
        std::sync::mpsc::SyncSender<Result<(), String>>,
    ),
    AttachActions(
        crate::actions::ActionSink,
        std::sync::mpsc::SyncSender<Result<(), String>>,
    ),
    Changed(
        snowglass_controllers::ControllerTopology,
        tokio::sync::oneshot::Sender<Result<snowglass_controllers::ControllerTopology, String>>,
    ),
    PauseResume(PauseState),
    Stop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PauseState {
    Pause,
    Resume,
}

impl PauseState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
        }
    }
}

impl ControllerSession {
    pub fn pause_resume(&self, state: PauseState) -> Result<(), String> {
        self.commands
            .send(ControllerSessionCommand::PauseResume(state))
            .map_err(|_| "emulator plugin session is closed".to_owned())
    }

    pub fn attach_actions(
        &self,
        sink: impl Fn(&crate::ParsedHotkey) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<(), String> {
        let (result, response) = std::sync::mpsc::sync_channel(1);
        self.commands
            .send(ControllerSessionCommand::AttachActions(
                Arc::new(sink),
                result,
            ))
            .map_err(|_| "emulator plugin session is closed while attaching actions".to_owned())?;
        response
            .recv()
            .map_err(|_| "emulator plugin session closed while attaching actions".to_owned())?
    }

    pub(crate) fn new(
        commands: std::sync::mpsc::Sender<ControllerSessionCommand>,
        errors: Arc<Mutex<Vec<SessionError>>>,
    ) -> Self {
        Self { commands, errors }
    }

    pub fn change(
        &self,
        topology: snowglass_controllers::ControllerTopology,
    ) -> Result<
        tokio::sync::oneshot::Receiver<Result<snowglass_controllers::ControllerTopology, String>>,
        String,
    > {
        let (response, receiver) = tokio::sync::oneshot::channel();
        self.commands
            .send(ControllerSessionCommand::Changed(topology, response))
            .map_err(|_| "controller plugin session is closed".to_owned())?;
        Ok(receiver)
    }

    pub fn attach_automation(
        &self,
        session: snowglass_automation::AutomationSession,
    ) -> Result<(), String> {
        let (result, response) = std::sync::mpsc::sync_channel(1);
        self.commands
            .send(ControllerSessionCommand::AttachAutomation(session, result))
            .map_err(|_| "controller plugin session is closed".to_owned())?;
        response
            .recv()
            .map_err(|_| "controller plugin session closed while attaching automation".to_owned())?
    }

    pub fn take_errors(&self) -> Vec<SessionError> {
        std::mem::take(&mut *self.errors.lock())
    }
}

impl Drop for ControllerSession {
    fn drop(&mut self) {
        if Arc::strong_count(&self.errors) == 1 {
            let _ = self.commands.send(ControllerSessionCommand::Stop);
        }
    }
}
