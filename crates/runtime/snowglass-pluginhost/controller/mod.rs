use parking_lot::Mutex;
use std::sync::Arc;

pub(crate) type InputProviderSink = Arc<
    dyn Fn(
            Vec<crate::InputProvider>,
            snowglass_controllers::ControllerTopology,
        ) -> Result<(), String>
        + Send
        + Sync,
>;

#[derive(Clone, Debug)]
pub struct ControllerSession {
    pub(crate) commands: std::sync::mpsc::Sender<ControllerSessionCommand>,
    errors: Arc<Mutex<Vec<String>>>,
}

pub(crate) enum ControllerSessionCommand {
    AttachAutomation(
        snowglass_automation::AutomationSession,
        std::sync::mpsc::SyncSender<Result<(), String>>,
    ),
    AttachInputProviders(
        InputProviderSink,
        std::sync::mpsc::SyncSender<Result<(), String>>,
    ),
    Changed(
        snowglass_controllers::ControllerTopology,
        tokio::sync::oneshot::Sender<Result<(), String>>,
    ),
    Stop,
}

impl ControllerSession {
    pub(crate) fn new(
        commands: std::sync::mpsc::Sender<ControllerSessionCommand>,
        errors: Arc<Mutex<Vec<String>>>,
    ) -> Self {
        Self { commands, errors }
    }

    pub fn change(
        &self,
        topology: snowglass_controllers::ControllerTopology,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<(), String>>, String> {
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

    pub fn attach_input_providers(
        &self,
        sink: impl Fn(
            Vec<crate::InputProvider>,
            snowglass_controllers::ControllerTopology,
        ) -> Result<(), String>
        + Send
        + Sync
        + 'static,
    ) -> Result<(), String> {
        let (result, response) = std::sync::mpsc::sync_channel(1);
        self.commands
            .send(ControllerSessionCommand::AttachInputProviders(
                Arc::new(sink),
                result,
            ))
            .map_err(|_| "controller plugin session is closed".to_owned())?;
        response.recv().map_err(|_| {
            "controller plugin session closed while attaching native input".to_owned()
        })?
    }

    pub fn take_errors(&self) -> Vec<String> {
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
