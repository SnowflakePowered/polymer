use deno_core::JsRuntime;
use parking_lot::Mutex;
use snowglass_input::gamepad::{GamepadSlot, VirtualGamepadConnection};
use std::{collections::BTreeMap, sync::Arc};

pub(crate) type ConnectionSink =
    Arc<dyn Fn(VirtualGamepadConnection) -> Result<(), String> + Send + Sync>;

#[derive(Default)]
pub(crate) struct ControllerEffects {
    connected: Vec<bool>,
    pending: BTreeMap<usize, bool>,
    sink: Option<ConnectionSink>,
}

impl ControllerEffects {
    pub(crate) fn configure(&mut self, connected: Vec<bool>) {
        self.connected = connected;
        self.pending.clear();
    }

    pub(crate) fn attach(&mut self, sink: ConnectionSink) -> Result<(), String> {
        self.sink = Some(sink);
        for (index, connected) in self.connected.iter().copied().enumerate() {
            self.pending.insert(index, connected);
        }
        self.flush()
    }

    pub(crate) fn stage(&mut self, connected: Vec<bool>) -> Result<(), String> {
        if connected.len() != self.connected.len() {
            return Err(format!(
                "controller topology changed virtual gamepad count from {} to {}",
                self.connected.len(),
                connected.len()
            ));
        }
        self.pending.extend(connected.into_iter().enumerate());
        Ok(())
    }

    pub(crate) fn flush(&mut self) -> Result<(), String> {
        let Some(sink) = &self.sink else {
            return Ok(());
        };
        while let Some((&index, &connected)) = self.pending.first_key_value() {
            let slot = u8::try_from(index)
                .ok()
                .and_then(GamepadSlot::new)
                .ok_or_else(|| format!("virtual gamepad {index} is outside the wire range"))?;
            sink(VirtualGamepadConnection { slot, connected })?;
            self.connected[index] = connected;
            self.pending.remove(&index);
        }
        Ok(())
    }

    pub(crate) fn discard(&mut self) {
        self.pending.clear();
    }
}

pub(crate) fn configure(runtime: &mut JsRuntime, connected: Vec<bool>) {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<ControllerEffects>()
        .configure(connected);
}

pub(crate) fn attach(runtime: &mut JsRuntime, sink: ConnectionSink) -> Result<(), String> {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<ControllerEffects>()
        .attach(sink)
}

pub(crate) fn stage(runtime: &mut JsRuntime, connected: Vec<bool>) -> Result<(), String> {
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<ControllerEffects>()
        .stage(connected)
}

pub(crate) fn flush(runtime: &mut JsRuntime) -> Result<(), String> {
    crate::filesystem::flush_staged(runtime).map_err(|error| error.to_string())?;
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<ControllerEffects>()
        .flush()
}

pub(crate) fn discard(runtime: &mut JsRuntime) {
    crate::filesystem::discard_staged(runtime);
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<ControllerEffects>()
        .discard();
}

#[derive(Debug)]
pub struct ControllerSession {
    pub(crate) commands: std::sync::mpsc::Sender<ControllerSessionCommand>,
    errors: Arc<Mutex<Vec<String>>>,
}

impl Clone for ControllerSession {
    fn clone(&self) -> Self {
        Self {
            commands: self.commands.clone(),
            errors: self.errors.clone(),
        }
    }
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
        Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
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
            .send(ControllerSessionCommand::Changed(topology, Some(response)))
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
