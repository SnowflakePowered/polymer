use deno_core::{JsRuntime, OpState, op2};
use deno_error::JsErrorBox;
use parking_lot::Mutex;
use snowglass_input::gamepad::{GamepadSlot, VirtualGamepadConnection};
use std::{collections::BTreeMap, sync::Arc};

pub(crate) struct ControllerSetup {
    pub arguments: serde_json::Value,
    pub assignments: snowglass_config::ast::PortAssignments,
    pub gamepad_mapping: serde_json::Value,
    pub connected: Vec<bool>,
}

impl ControllerSetup {
    pub(crate) fn expand(
        plugin: &dyn crate::EmulatorPluginInstance,
        console: &str,
        arguments: &serde_json::Value,
    ) -> Result<Self, crate::PluginInvocationError> {
        let ports = plugin
            .ports(console)
            .map_err(crate::PluginInvocationError::worker)?;
        let original_assignments = arguments
            .get("portAssignments")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| {
                crate::PluginInvocationError::new(
                    "TypeError",
                    "INVALID_ARGUMENTS",
                    "setupLaunch arguments require portAssignments",
                )
            })?;
        let existing = arguments
            .get("controllerMappings")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        if ports.len() > u8::MAX as usize + 1 {
            return Err(crate::PluginInvocationError::worker(
                "an emulator cannot expose more than 256 virtual gamepads",
            ));
        }
        let mut used_slots = vec![false; ports.len()];
        let mut port_slots = vec![None; ports.len()];
        for (port_index, port) in ports.iter().enumerate() {
            let Some(slot) = existing
                .iter()
                .find(|mapping| {
                    mapping.get("group").and_then(serde_json::Value::as_str)
                        == Some(port.group.as_str())
                        && mapping.get("port").and_then(serde_json::Value::as_u64)
                            == Some(port.index as u64)
                })
                .and_then(|mapping| mapping.get("slot"))
                .and_then(serde_json::Value::as_u64)
                .and_then(|slot| usize::try_from(slot).ok())
            else {
                continue;
            };
            if slot >= ports.len() {
                return Err(crate::PluginInvocationError::worker(format!(
                    "virtual gamepad {slot} is outside the emulator's {}-pad topology",
                    ports.len()
                )));
            }
            if std::mem::replace(&mut used_slots[slot], true) {
                return Err(crate::PluginInvocationError::worker(format!(
                    "virtual gamepad {slot} is assigned to more than one emulator port"
                )));
            }
            port_slots[port_index] = Some(slot);
        }
        let mut remaining_slots = used_slots
            .iter()
            .enumerate()
            .filter_map(|(slot, used)| (!used).then_some(slot));
        for slot in &mut port_slots {
            if slot.is_none() {
                *slot = remaining_slots.next();
            }
        }
        let mut expanded_assignments = serde_json::Map::new();
        let mut controller_mappings = Vec::with_capacity(ports.len());
        let mut gamepad_mapping = Vec::with_capacity(ports.len());
        let mut gamepad_bindings = Vec::with_capacity(ports.len());
        let mut connected = vec![false; ports.len()];

        for (port_index, port) in ports.iter().enumerate() {
            let slot = port_slots[port_index].expect("every emulator port has a virtual gamepad");
            let selected = existing.iter().find(|mapping| {
                mapping.get("group").and_then(serde_json::Value::as_str)
                    == Some(port.group.as_str())
                    && mapping.get("port").and_then(serde_json::Value::as_u64)
                        == Some(port.index as u64)
            });
            let requested_layout = original_assignments
                .get(&port.group)
                .and_then(serde_json::Value::as_array)
                .and_then(|layouts| layouts.get(port.index))
                .and_then(serde_json::Value::as_str);
            let layout = selected
                .and_then(|mapping| mapping.get("layout"))
                .and_then(serde_json::Value::as_str)
                .or(requested_layout)
                .or_else(|| port.layouts.first().map(String::as_str))
                .ok_or_else(|| {
                    crate::PluginInvocationError::worker(format!(
                        "emulator port {}:{} declares no layouts",
                        port.group, port.index
                    ))
                })?
                .to_owned();
            let layouts = expanded_assignments
                .entry(port.group.clone())
                .or_insert_with(|| serde_json::Value::Array(Vec::new()))
                .as_array_mut()
                .expect("expanded port assignments are arrays");
            layouts.resize(port.index + 1, serde_json::Value::String(layout.clone()));
            layouts[port.index] = serde_json::Value::String(layout.clone());

            let mut mapping = selected.cloned().unwrap_or_else(|| {
                serde_json::json!({
                    "source": null,
                    "device": null,
                    "mouse": false,
                    "projectionMode": "position",
                    "mapping": {},
                })
            });
            let object = mapping
                .as_object_mut()
                .expect("controller mappings are objects");
            object.insert("slot".to_owned(), slot.into());
            object.insert("group".to_owned(), port.group.clone().into());
            object.insert("port".to_owned(), port.index.into());
            object.insert("layout".to_owned(), layout.clone().into());
            let is_connected = object
                .get("device")
                .and_then(|device| device.get("connected"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or_else(|| object.get("source").is_some_and(|source| !source.is_null()));
            let source = object
                .get("source")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let mouse = object
                .get("mouse")
                .cloned()
                .unwrap_or(serde_json::Value::Bool(false));
            let projection_mode = object
                .get("projectionMode")
                .cloned()
                .unwrap_or_else(|| "position".into());
            let input_mapping = object
                .get("mapping")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            connected[slot] = is_connected;
            gamepad_mapping.push(serde_json::json!({
                "source": source,
                "mouse": mouse,
                "slot": slot,
                "port": { "group": port.group, "index": port.index },
                "layout": layout,
                "projectionMode": projection_mode,
                "mapping": input_mapping,
            }));
            controller_mappings.push(mapping);
            gamepad_bindings.push(serde_json::json!({
                "target": { "group": port.group, "port": port.index },
                "gamepad": slot,
            }));
        }

        let assignments =
            serde_json::from_value(serde_json::Value::Object(expanded_assignments.clone()))
                .map_err(crate::PluginInvocationError::worker)?;
        let mut arguments = arguments.clone();
        let object = arguments
            .as_object_mut()
            .expect("validated setupLaunch arguments are an object");
        object.insert(
            "portAssignments".to_owned(),
            serde_json::Value::Object(expanded_assignments),
        );
        object.insert(
            "controllerMappings".to_owned(),
            serde_json::Value::Array(controller_mappings),
        );
        object.insert(
            "gamepadBindings".to_owned(),
            serde_json::Value::Array(gamepad_bindings),
        );
        Ok(Self {
            arguments,
            assignments,
            gamepad_mapping: serde_json::Value::Array(gamepad_mapping),
            connected,
        })
    }
}

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

    fn stage(&mut self, index: usize, connected: bool) -> Result<(), String> {
        if index >= self.connected.len() {
            return Err(format!("virtual gamepad {index} does not exist"));
        }
        self.pending.insert(index, connected);
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

    pub(crate) fn states(&self) -> Vec<bool> {
        self.connected.clone()
    }
}

#[op2(fast)]
pub(crate) fn op_virtual_gamepad_set_connected(
    state: &mut OpState,
    index: u32,
    connected: bool,
) -> Result<(), JsErrorBox> {
    state
        .borrow_mut::<ControllerEffects>()
        .stage(index as usize, connected)
        .map_err(JsErrorBox::generic)
}

#[op2(fast)]
pub(crate) fn op_controller_change_flush(state: &mut OpState) -> Result<(), JsErrorBox> {
    state
        .borrow_mut::<ControllerEffects>()
        .flush()
        .map_err(JsErrorBox::generic)
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

pub(crate) fn states(runtime: &mut JsRuntime) -> Vec<bool> {
    runtime
        .op_state()
        .borrow_mut()
        .borrow::<ControllerEffects>()
        .states()
}

pub(crate) fn port_assignments(
    topology: &serde_json::Value,
) -> Result<snowglass_config::ast::PortAssignments, String> {
    let mappings = topology
        .get("controllerMappings")
        .and_then(serde_json::Value::as_array)
        .or_else(|| topology.as_array())
        .ok_or_else(|| "controller topology requires controllerMappings".to_owned())?;
    let mut groups = serde_json::Map::new();
    for mapping in mappings {
        let target = mapping.get("target").unwrap_or(mapping);
        let group = target
            .get("group")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "controller mapping target requires group".to_owned())?;
        let port = target
            .get("port")
            .and_then(serde_json::Value::as_u64)
            .and_then(|port| usize::try_from(port).ok())
            .ok_or_else(|| "controller mapping target requires port".to_owned())?;
        let layout = mapping
            .get("layout")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "controller mapping requires layout".to_owned())?;
        let layouts = groups
            .entry(group.to_owned())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()))
            .as_array_mut()
            .expect("controller assignment groups are arrays");
        layouts.resize(port + 1, serde_json::Value::String(layout.to_owned()));
        layouts[port] = serde_json::Value::String(layout.to_owned());
    }
    serde_json::from_value(serde_json::Value::Object(groups)).map_err(|error| error.to_string())
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
    Changed(serde_json::Value),
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

    pub fn changed(&self, topology: serde_json::Value) -> Result<(), String> {
        self.commands
            .send(ControllerSessionCommand::Changed(topology))
            .map_err(|_| "controller plugin session is closed".to_owned())
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
