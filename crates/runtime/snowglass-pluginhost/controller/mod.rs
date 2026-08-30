use parking_lot::Mutex;
use snowglass_input::gamepad::VirtualGamepadConnection;
use std::sync::Arc;

pub(crate) type ConnectionSink =
    Arc<dyn Fn(VirtualGamepadConnection) -> Result<(), String> + Send + Sync>;

#[derive(Clone, Debug)]
pub struct ControllerSession {
    pub(crate) commands: std::sync::mpsc::Sender<ControllerSessionCommand>,
    errors: Arc<Mutex<Vec<String>>>,
}

pub(crate) enum ControllerSessionCommand {
    Attach(
        ConnectionSink,
        std::sync::mpsc::SyncSender<Result<(), String>>,
    ),
    AttachAutomation(
        snowglass_automation::AutomationSession,
        std::sync::mpsc::SyncSender<Result<(), String>>,
    ),
    Changed(
        snowglass_input::topology::ControllerTopology,
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

    pub fn attach(
        &self,
        sink: impl Fn(VirtualGamepadConnection) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<(), String> {
        let (result, response) = std::sync::mpsc::sync_channel(1);
        self.commands
            .send(ControllerSessionCommand::Attach(Arc::new(sink), result))
            .map_err(|_| "controller plugin session is closed".to_owned())?;
        response
            .recv()
            .map_err(|_| "controller plugin session closed while attaching input".to_owned())?
    }

    pub fn change(
        &self,
        topology: snowglass_input::topology::ControllerTopology,
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
