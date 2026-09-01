use snowglass_config::input::{
    InputConfiguration, InputOperation, InputTransaction, SourceAssignment,
};
use snowglass_controllers::{
    ControllerConfiguration, ControllerManager, ControllerManagerEffect, ControllerTopology,
};
use snowglass_egui::{InputAxisDiagnostics, InputDiagnostics, InputSlotDiagnostics};
use snowglass_input::{
    HostInputEvent, InputHub, KeyCode, PlayerIndex, PointerFlags, PointerReport,
};
use snowglass_input_ipc::InputIpcListener;
use snowglass_input_ipc::{RawHidChannel, RawHidSpec};
use snowglass_protocol::input::PointerPhase;
use snowglass_renderer::{
    input::{PointerEventSink, PointerRoutingState},
    layout::Rect,
    pointer::PointerProjector,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

mod bluetooth;
use bluetooth::BluetoothUi;

const DOM_MK_SHIFT: u32 = 0x0004;
const DOM_MK_CONTROL: u32 = 0x0008;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputPriority {
    Game,
    Ui,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeyboardTarget {
    Overlay,
    Ui,
    Game,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PointerPosition {
    pub x: i32,
    pub y: i32,
}

pub(crate) struct GameInputEndpoint {
    listener: InputIpcListener,
}

impl GameInputEndpoint {
    pub fn new(listener: InputIpcListener) -> Self {
        Self { listener }
    }
}

pub(crate) struct InputPublication {
    pub diagnostics: InputDiagnostics,
    pub api_state: Option<serde_json::Value>,
    pub configuration_changed: bool,
}

pub(crate) struct InputConfigurationPublication {
    pub configuration: serde_json::Value,
    pub layouts: serde_json::Value,
}

pub(crate) struct PreparedInputTransaction {
    transaction: InputTransaction,
    next: InputConfiguration,
}

impl PreparedInputTransaction {
    pub(crate) fn candidate(&self) -> &InputConfiguration {
        &self.next
    }
}

pub(crate) struct InputRouter {
    hub: InputHub,
    configuration: ControllerConfiguration,
    manager: ControllerManager,
    controller_api: snowglass_runtime::ControllerApiState,
    listener: Option<InputIpcListener>,
    sdl: Arc<snowglass_input_sdl3::Sdl3Backend>,
    bluetooth: Arc<snowglass_input_bluetooth::BluetoothBackend>,
    bluetooth_ui: BluetoothUi,
    topology: Option<snowglass_input_layout::topology::InputTopology>,
    mouse_buttons: u8,
    pointer_routing: PointerRoutingState,
    pointer_projectors: Vec<PointerProjector>,
    pointer_revisions: Vec<Option<u64>>,
    priority: InputPriority,
    cursor: PointerPosition,
    dom_mouse_keys: u32,
    dom_alt_key: bool,
    next_publish: Instant,
    last_controller_api_revision: u64,
    controller_topology: Option<ControllerTopology>,
    input_hooks: Vec<snowglass_runtime::InputHook>,
    input_namespace: String,
    raw_hid: Vec<RawHidChannel>,
}

impl InputRouter {
    pub async fn runtime(
        hub: InputHub,
        controller_api: snowglass_runtime::ControllerApiState,
    ) -> Result<Self, String> {
        let directories =
            snowglass_library::Directories::discover().map_err(|error| error.to_string())?;
        let sdl = Arc::new(
            snowglass_input_sdl3::Sdl3Backend::new(Default::default())
                .map_err(|error| error.to_string())?,
        );
        hub.register_backend(sdl.clone())
            .map_err(|error| error.to_string())?;
        let bluetooth_store = snowglass_bluetooth::store::SqliteControllerStore::open(
            directories.storage().join("input.db"),
        )
        .map_err(|error| error.to_string())?;
        let bluetooth_config = snowglass_bluetooth::ManagerConfig {
            store: Arc::new(bluetooth_store),
            ..Default::default()
        };
        let bluetooth = Arc::new(
            snowglass_input_bluetooth::BluetoothBackend::start(bluetooth_config)
                .await
                .map_err(|error| error.to_string())?,
        );
        let mut bluetooth_ui = BluetoothUi::new(&bluetooth);
        hub.register_backend(bluetooth.clone())
            .map_err(|error| error.to_string())?;
        let manager = ControllerManager::open(
            directories.storage().join("input.db"),
            controller_api.controller_profiles(),
            hub.clone(),
        )
        .await?;
        let configuration =
            ControllerConfiguration::new(None, []).map_err(|error| error.to_string())?;
        let mut controllers = manager.snapshot();
        bluetooth_ui.merge(&mut controllers);
        controller_api.publish(controllers, &manager);
        Ok(Self {
            hub,
            configuration,
            manager,
            controller_api,
            listener: None,
            sdl,
            bluetooth,
            bluetooth_ui,
            topology: None,
            mouse_buttons: 0,
            pointer_routing: PointerRoutingState::new(Instant::now()),
            pointer_projectors: (0..8)
                .map(|_| PointerProjector::new(Instant::now()))
                .collect(),
            pointer_revisions: vec![None; 8],
            priority: InputPriority::Ui,
            cursor: PointerPosition::default(),
            dom_mouse_keys: 0,
            dom_alt_key: false,
            next_publish: Instant::now(),
            last_controller_api_revision: 0,
            controller_topology: None,
            input_hooks: Vec::new(),
            input_namespace: String::new(),
            raw_hid: Vec::new(),
        })
    }

    pub fn configuration_snapshot(&self) -> InputConfiguration {
        self.configuration.snapshot()
    }

    pub fn controller_topology(&self) -> Result<ControllerTopology, String> {
        self.manager.topology_for(&self.configuration.snapshot())
    }

    pub fn controller_topology_for(
        &self,
        configuration: &InputConfiguration,
    ) -> Result<ControllerTopology, String> {
        self.manager.topology_for(configuration)
    }

    pub fn layouts(&self) -> &'static [snowglass_input_layout::layout::InputLayout] {
        self.configuration.layouts()
    }

    pub fn configuration_publication(&self) -> InputConfigurationPublication {
        InputConfigurationPublication {
            configuration: serde_json::to_value(self.configuration_snapshot()).unwrap_or_default(),
            layouts: serde_json::to_value(self.layouts()).unwrap_or_default(),
        }
    }

    pub fn configuration_result(
        request_id: u64,
        result: Result<InputConfiguration, String>,
    ) -> (serde_json::Value, serde_json::Value) {
        let result = match result {
            Ok(configuration) => serde_json::json!({"ok": true, "configuration": configuration}),
            Err(error) => serde_json::json!({"ok": false, "error": error}),
        };
        let command = serde_json::json!({"type": "inputConfigurationResult", "id": request_id, "result": result});
        (result, command)
    }

    pub fn attach(&mut self, endpoint: GameInputEndpoint) {
        endpoint.listener.attach(&self.hub);
        if let Some(topology) = &self.topology {
            let _ = endpoint.listener.publish_topology(topology);
        }
        self.listener = Some(endpoint.listener);
        let _ = self.rebuild_input_hooks();
    }

    pub fn detach(&mut self) {
        self.listener.take();
    }

    pub fn add_mouse_delta(&self, delta_x: f64, delta_y: f64) {
        if self.priority == InputPriority::Game {
            self.hub.publish_host(HostInputEvent::MouseMotion {
                dx: delta_x,
                dy: delta_y,
            });
        }
    }

    pub fn route_game_key(&self, key: &str, pressed: bool) -> bool {
        let code = KeyCode::new(key);
        self.hub.publish_host(HostInputEvent::Key {
            code,
            scan_code: 0,
            virtual_key: 0,
            pressed,
            repeat: false,
            extended: false,
            target_window: 0,
            timestamp_ms: 0,
            sequence: 0,
        });
        code.as_str() != "Unidentified"
    }

    pub const fn priority(&self) -> InputPriority {
        self.priority
    }

    pub fn replace_priority(&mut self, priority: InputPriority) -> InputPriority {
        std::mem::replace(&mut self.priority, priority)
    }

    pub const fn cursor(&self) -> PointerPosition {
        self.cursor
    }

    pub fn set_cursor(&mut self, x: f64, y: f64) {
        self.cursor = PointerPosition {
            x: x.round() as i32,
            y: y.round() as i32,
        };
        self.pointer_routing.set_cursor(x, y);
    }

    pub const fn mouse_buttons(&self) -> u8 {
        self.mouse_buttons
    }
    pub fn restore_mouse_buttons(&mut self, buttons: u8) {
        self.mouse_buttons = buttons;
    }
    pub const fn dom_shift_key(&self) -> bool {
        self.dom_mouse_keys & DOM_MK_SHIFT != 0
    }
    pub const fn dom_control_key(&self) -> bool {
        self.dom_mouse_keys & DOM_MK_CONTROL != 0
    }
    pub const fn dom_alt_key(&self) -> bool {
        self.dom_alt_key
    }

    pub fn set_modifiers(&mut self, alt: bool, shift: bool, control: bool) {
        self.dom_alt_key = alt;
        Self::set_flag(&mut self.dom_mouse_keys, DOM_MK_SHIFT, shift);
        Self::set_flag(&mut self.dom_mouse_keys, DOM_MK_CONTROL, control);
    }

    pub fn ui_buttons_active(&self) -> bool {
        self.mouse_buttons != 0
    }
    pub fn captured_connection(&self) -> Option<u64> {
        self.pointer_routing.captured_connection()
    }
    pub fn has_active_pointer_sequence(&self) -> bool {
        self.captured_connection().is_some() || self.ui_buttons_active()
    }

    pub fn route_game_pointer(
        &mut self,
        phase: PointerPhase,
        button: u32,
        horizontal: i32,
        vertical: i32,
        presentation: &snowglass_renderer::Presentation,
        plan: &snowglass_renderer::CompositingPlan,
        pointer: &snowglass_renderer::SurfacePointerSender,
    ) {
        let _ = self.pointer_routing.send(
            phase,
            button,
            horizontal,
            vertical,
            presentation,
            plan,
            pointer,
        );
    }

    pub fn route_device_pointers(
        &mut self,
        presentation: &snowglass_renderer::Presentation,
        plan: &snowglass_renderer::CompositingPlan,
        pointer: &snowglass_renderer::SurfacePointerSender,
    ) {
        use snowglass_input_layout::topology::{PointerMode, PointerSource};

        let routes = self.topology.as_ref().map_or_else(Vec::new, |topology| {
            topology
                .ports
                .iter()
                .map(|port| (port.player, port.pointer))
                .collect()
        });
        let extent = presentation.viewer_extent();
        let bounds = Rect::new(0, 0, extent.width, extent.height);
        for (player, binding) in routes {
            let index = PlayerIndex::new(player);
            if self.priority != InputPriority::Game
                || binding.mode != PointerMode::AbsoluteScreen
                || matches!(
                    binding.source,
                    PointerSource::None | PointerSource::HostMouse
                )
            {
                self.hub.set_pointer_override(index, None);
                self.pointer_revisions[usize::from(player)] = None;
                continue;
            }
            let Some((revision, sample)) = self.hub.pointer_snapshot(index) else {
                self.hub.set_pointer_override(index, None);
                self.pointer_revisions[usize::from(player)] = None;
                continue;
            };
            let Some(projector) = self.pointer_projectors.get_mut(usize::from(player)) else {
                continue;
            };
            if self.pointer_revisions[usize::from(player)] != Some(revision) {
                projector.accept(&sample, bounds, Instant::now());
                self.pointer_revisions[usize::from(player)] = Some(revision);
            }
            let Some(projected) = projector.project(presentation, plan) else {
                self.hub.set_pointer_override(index, None);
                continue;
            };
            let _ = pointer.send_pointer(projected.connection, projected.pointer);
            let current = self
                .hub
                .state(index)
                .map(|state| state.pointer)
                .unwrap_or_default();
            let flags = PointerFlags::PROJECTED
                | PointerFlags::DEVICE
                | if current.flags.contains(PointerFlags::CONTACT) {
                    PointerFlags::CONTACT
                } else {
                    PointerFlags::default()
                };
            self.hub.set_pointer_override(
                index,
                Some(PointerReport {
                    position: projected.normalized,
                    delta: [0; 2],
                    flags,
                    kind: current.kind,
                }),
            );
        }
    }

    pub fn cancel_pointer_connection(
        &mut self,
        connection: u64,
        presentation: &snowglass_renderer::Presentation,
        plan: &snowglass_renderer::CompositingPlan,
        pointer: &snowglass_renderer::SurfacePointerSender,
    ) {
        self.pointer_routing
            .cancel(connection, presentation, plan, pointer);
    }

    pub fn prepare_priority_change(&mut self) {
        self.hub.publish_host(HostInputEvent::FocusLost);
    }
    pub fn set_game_input_enabled(&self, enabled: bool) {
        if let Some(listener) = &self.listener {
            listener.set_forwarding(enabled, &self.hub);
        }
    }
    pub fn clear_guest_input_state(&self) {
        self.hub.publish_host(HostInputEvent::FocusLost);
    }

    pub fn focus_lost_buttons(&self) -> Vec<(u16, u8)> {
        (0..5)
            .filter_map(|button| {
                let flag = 1 << button;
                (self.mouse_buttons & flag != 0).then_some((button, flag))
            })
            .collect()
    }

    pub fn finish_focus_lost(&mut self) {
        self.mouse_buttons = 0;
        self.dom_mouse_keys = 0;
        self.dom_alt_key = false;
        self.hub.publish_host(HostInputEvent::FocusLost);
    }

    pub fn publication_due(&mut self, now: Instant) -> bool {
        if now < self.next_publish {
            false
        } else {
            self.next_publish = now + Duration::from_millis(16);
            true
        }
    }

    pub fn set_mouse_button(&mut self, flag: u8, pressed: bool) -> u8 {
        let previous = self.mouse_buttons;
        if pressed {
            self.mouse_buttons |= flag;
        } else {
            self.mouse_buttons &= !flag;
        }
        previous
    }

    pub fn keyboard_target(&self, overlay_consumed: bool, ui_prevented: bool) -> KeyboardTarget {
        if ui_prevented {
            KeyboardTarget::Ui
        } else if overlay_consumed && self.priority == InputPriority::Ui {
            KeyboardTarget::Overlay
        } else if self.priority == InputPriority::Game {
            KeyboardTarget::Game
        } else {
            KeyboardTarget::Ui
        }
    }

    pub fn configure_launch(
        &mut self,
        machine: &str,
        topology: &ControllerTopology,
    ) -> Result<(), String> {
        let transaction =
            topology.input_transaction(machine, self.configuration.snapshot().revision)?;
        self.configuration
            .apply(transaction)
            .map_err(|error| error.to_string())?;
        self.manager
            .sync_configuration(&self.configuration.snapshot())?;
        let wire = topology.input_topology(
            self.configuration.snapshot().revision as u32,
            Some(machine.to_owned()),
        )?;
        if let Some(listener) = &self.listener {
            listener
                .publish_topology(&wire)
                .map_err(|error| error.to_string())?;
        }
        self.topology = Some(wire);
        self.controller_topology = Some(topology.clone());
        self.rebuild_input_hooks()
    }

    pub fn configure_input_hooks(
        &mut self,
        hooks: &[snowglass_runtime::InputHook],
        namespace: &str,
    ) -> Result<(), String> {
        self.input_hooks = hooks.to_vec();
        self.input_namespace = namespace.to_owned();
        self.rebuild_input_hooks()
    }

    pub fn clear_input_hooks(&mut self) {
        self.raw_hid.clear();
        if let Some(listener) = &self.listener {
            listener.clear_raw_hid();
        }
        self.input_hooks.clear();
        self.controller_topology = None;
        if let Some(topology) = &mut self.topology {
            topology.raw_hid.clear();
        }
    }

    pub fn apply_controller_topology(
        &mut self,
        topology: ControllerTopology,
    ) -> Result<(), String> {
        let configuration = self.configuration.snapshot();
        self.topology =
            Some(topology.input_topology(configuration.revision as u32, configuration.machine)?);
        self.controller_topology = Some(topology);
        self.rebuild_input_hooks()
    }

    pub fn prepare_configuration_transaction(
        &mut self,
        transaction: InputTransaction,
        _topology_locked: bool,
    ) -> Result<PreparedInputTransaction, String> {
        let next = self
            .configuration
            .preview(&transaction)
            .map_err(|error| error.to_string())?;
        self.manager.topology_for(&next)?;
        Ok(PreparedInputTransaction { transaction, next })
    }

    pub fn commit_configuration_transaction(
        &mut self,
        prepared: PreparedInputTransaction,
    ) -> Result<InputConfiguration, String> {
        let configuration = self
            .configuration
            .apply(prepared.transaction)
            .map_err(|error| error.to_string())?;
        self.manager.sync_configuration(&configuration)?;
        Ok(configuration)
    }

    pub async fn controller_command(
        &mut self,
        command: serde_json::Value,
        _topology_locked: bool,
    ) -> Result<(serde_json::Value, bool), String> {
        let outcome = self.manager.command(command).await?;
        let state_response = outcome.response.get("devices").is_some();
        let mut configuration_changed = outcome
            .assignments
            .map(|assignments| self.replace_assignments(assignments))
            .transpose()?
            .unwrap_or(false);
        for effect in outcome.effects {
            match effect {
                ControllerManagerEffect::SessionDiscovery(active) => {
                    self.bluetooth_ui.set_session_active(active);
                }
                ControllerManagerEffect::DisconnectSources(sources) => {
                    for source in sources {
                        self.bluetooth_ui.disconnect(&source);
                    }
                }
                ControllerManagerEffect::Connect(id) => self.bluetooth_ui.connect(&id)?,
                ControllerManagerEffect::Forget(id) => self.bluetooth_ui.forget(&id)?,
                ControllerManagerEffect::ReapplyMappings => configuration_changed = true,
            }
        }
        let controllers = self.publish_controller_api();
        Ok((
            if state_response {
                controllers
            } else {
                outcome.response
            },
            configuration_changed,
        ))
    }

    pub fn build_publication(
        &mut self,
        mappings: &[snowglass_controllers::ControllerMapping],
        game_active: bool,
        _topology_locked: bool,
    ) -> InputPublication {
        let configuration_changed = self.manager.observe_session().is_some_and(|assignments| {
            self.replace_assignments(assignments).unwrap_or_else(|error| {
                tracing::error!(%error, "snowglass: updating controller session assignments failed");
                false
            })
        });
        self.publish_controller_api();
        let snapshots = self.hub.snapshot();
        let slots = mappings
            .iter()
            .map(|mapping| {
                let state = snapshots.iter().find(|(index, _, _)| {
                    index.is_some_and(|index| index.get() == mapping.gamepad)
                });
                state.map_or_else(
                    || InputSlotDiagnostics {
                        slot: usize::from(mapping.gamepad),
                        layout: mapping.layout.clone(),
                        ..Default::default()
                    },
                    |(_, device, state)| InputSlotDiagnostics {
                        slot: usize::from(mapping.gamepad),
                        layout: mapping.layout.clone(),
                        connected: true,
                        name: device.name.to_string(),
                        pressed: snowglass_input::GamepadButton::ALL
                            .into_iter()
                            .filter(|button| state.gamepad.buttons & button.mask() != 0)
                            .map(|button| format!("{button:?}"))
                            .collect(),
                        axes: state
                            .gamepad
                            .left_stick
                            .into_iter()
                            .enumerate()
                            .map(|(index, value)| InputAxisDiagnostics {
                                name: format!("axis{index}"),
                                value,
                            })
                            .collect(),
                        mapped_buttons: state.gamepad.buttons,
                        left_stick: state.gamepad.left_stick,
                        right_stick: state.gamepad.right_stick,
                        left_trigger: state.gamepad.left_trigger,
                        right_trigger: state.gamepad.right_trigger,
                        pointer: state.pointer.position,
                        accelerometer: state.sensor.accelerometer,
                        gyroscope: state.sensor.gyroscope,
                        ..Default::default()
                    },
                )
            })
            .collect();
        let api = self.controller_api.snapshot();
        let revision = api["revision"].as_u64().unwrap_or_default();
        let api_state = (revision != self.last_controller_api_revision).then(|| {
            self.last_controller_api_revision = revision;
            api
        });
        InputPublication {
            diagnostics: InputDiagnostics {
                active: self.priority == InputPriority::Game && game_active,
                slots,
            },
            api_state,
            configuration_changed,
        }
    }

    fn replace_assignments(&mut self, assignments: Vec<SourceAssignment>) -> Result<bool, String> {
        let current = self.configuration.snapshot();
        if current.assignments == assignments {
            return Ok(false);
        }
        let transaction = InputTransaction {
            expected_revision: current.revision,
            operations: vec![InputOperation::ReplaceAssignments { assignments }],
        };
        let configuration = self
            .configuration
            .apply(transaction)
            .map_err(|error| error.to_string())?;
        self.manager.sync_configuration(&configuration)?;
        Ok(true)
    }

    fn set_flag(flags: &mut u32, flag: u32, enabled: bool) {
        if enabled {
            *flags |= flag;
        } else {
            *flags &= !flag;
        }
    }

    fn publish_controller_api(&mut self) -> serde_json::Value {
        let mut controllers = self.manager.snapshot();
        self.bluetooth_ui.merge(&mut controllers);
        self.controller_api
            .publish(controllers.clone(), &self.manager);
        controllers
    }

    fn rebuild_input_hooks(&mut self) -> Result<(), String> {
        let enabled = self
            .input_hooks
            .contains(&snowglass_runtime::InputHook::BluetoothApiWin32);
        let Some(controller_topology) = self.controller_topology.as_ref() else {
            return Ok(());
        };
        let devices = self.hub.devices();
        let bindings = if enabled {
            controller_topology
                .mappings
                .iter()
                .filter(|mapping| {
                    mapping.connected
                        && mapping.device.as_ref().is_some_and(|device| {
                            device.capabilities.iter().any(|value| value == "rawHid")
                        })
                })
                .map(|mapping| {
                    let source_id = &mapping
                        .device
                        .as_ref()
                        .expect("filtered mappings have a device")
                        .id;
                    let (source_player, source_device) = devices
                        .iter()
                        .find(|(_, device)| device.id.as_str() == source_id)
                        .ok_or_else(|| format!("raw HID source '{source_id}' is not connected"))?;
                    let source_player = (*source_player).ok_or_else(|| {
                        format!("raw HID source '{source_id}' has no input assignment")
                    })?;
                    Ok(snowglass_input_layout::topology::RawHidBinding {
                        player: source_player.get(),
                        port: u8::try_from(mapping.target.port).map_err(|_| {
                            format!("raw HID target port {} exceeds 255", mapping.target.port)
                        })?,
                        vendor_id: source_device.vendor_id.ok_or_else(|| {
                            format!("raw HID source '{source_id}' does not report a vendor ID")
                        })?,
                        product_id: source_device.product_id.ok_or_else(|| {
                            format!("raw HID source '{source_id}' does not report a product ID")
                        })?,
                        pipe: format!(
                            r"\\.\pipe\snowglass-{}-hid-{}",
                            self.input_namespace, mapping.target.port
                        ),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?
        } else {
            Vec::new()
        };
        let Some(topology) = &mut self.topology else {
            return Ok(());
        };
        if topology.raw_hid == bindings
            && (self.listener.is_none() || self.raw_hid.len() == bindings.len())
        {
            return Ok(());
        }
        topology.raw_hid = bindings;
        self.raw_hid.clear();
        if let Some(listener) = &self.listener {
            listener.clear_raw_hid();
            for binding in &topology.raw_hid {
                let spec = RawHidSpec {
                    pipe_name: binding.pipe.clone(),
                    vendor_id: binding.vendor_id,
                    product_id: binding.product_id,
                    report_size: 22,
                };
                self.raw_hid.push(
                    listener
                        .open_raw_hid(snowglass_input::PlayerIndex::new(binding.player), spec)
                        .map_err(|error| error.to_string())?,
                );
            }
            listener
                .publish_topology(topology)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

impl Drop for InputRouter {
    fn drop(&mut self) {
        let _ = Arc::strong_count(&self.sdl);
        let _ = Arc::strong_count(&self.bluetooth);
    }
}
