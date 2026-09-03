use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct ControllerSession {
    commands: Arc<std::sync::mpsc::Sender<ControllerSessionCommand>>,
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
        snowglass_controllers::ControllerTopologyChange,
        tokio::sync::oneshot::Sender<Result<snowglass_controllers::ControllerTopology, String>>,
    ),
    PauseResume(
        PauseState,
        std::time::Duration,
        tokio::sync::oneshot::Sender<Result<(), String>>,
    ),
    StateAction(
        u32,
        bool,
        std::time::Duration,
        tokio::sync::oneshot::Sender<Result<(), String>>,
    ),
    Quit(
        std::time::Duration,
        tokio::sync::oneshot::Sender<Result<Option<Vec<u8>>, String>>,
    ),
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
    pub fn pause_resume(
        &self,
        state: PauseState,
        timeout: Option<std::time::Duration>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), String>>, String> {
        const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        let (result, response) = tokio::sync::oneshot::channel();
        self.commands
            .send(ControllerSessionCommand::PauseResume(
                state,
                timeout.unwrap_or(DEFAULT_TIMEOUT),
                result,
            ))
            .map_err(|_| "emulator plugin session is closed".to_owned())?;
        Ok(response)
    }

    pub fn save_state(
        &self,
        slot: u32,
        timeout: Option<std::time::Duration>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), String>>, String> {
        self.state_action(slot, true, timeout)
    }

    pub fn resume_state(
        &self,
        slot: u32,
        timeout: Option<std::time::Duration>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), String>>, String> {
        self.state_action(slot, false, timeout)
    }

    fn state_action(
        &self,
        slot: u32,
        save: bool,
        timeout: Option<std::time::Duration>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), String>>, String> {
        const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        let (result, response) = tokio::sync::oneshot::channel();
        self.commands
            .send(ControllerSessionCommand::StateAction(
                slot,
                save,
                timeout.unwrap_or(DEFAULT_TIMEOUT),
                result,
            ))
            .map_err(|_| "emulator plugin session is closed".to_owned())?;
        Ok(response)
    }

    pub fn quit(
        &self,
        timeout: Option<std::time::Duration>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<Option<Vec<u8>>, String>>, String> {
        const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        let (result, response) = tokio::sync::oneshot::channel();
        self.commands
            .send(ControllerSessionCommand::Quit(
                timeout.unwrap_or(DEFAULT_TIMEOUT),
                result,
            ))
            .map_err(|_| "emulator plugin session is closed".to_owned())?;
        Ok(response)
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

    pub(crate) fn new(commands: std::sync::mpsc::Sender<ControllerSessionCommand>) -> Self {
        Self {
            commands: Arc::new(commands),
        }
    }

    pub fn change(
        &self,
        change: snowglass_controllers::ControllerTopologyChange,
    ) -> Result<
        tokio::sync::oneshot::Receiver<Result<snowglass_controllers::ControllerTopology, String>>,
        String,
    > {
        let (response, receiver) = tokio::sync::oneshot::channel();
        self.commands
            .send(ControllerSessionCommand::Changed(change, response))
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
}

impl Drop for ControllerSession {
    fn drop(&mut self) {
        if Arc::strong_count(&self.commands) == 1 {
            let _ = self.commands.send(ControllerSessionCommand::Stop);
        }
    }
}
