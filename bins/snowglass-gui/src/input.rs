//! Shell policy for physical input capture.
//!
//! The emulator instance owns IPC. This adapter only gives long-lived shell
//! input objects a stable endpoint while game instances are replaced.

use parking_lot::RwLock;
use snowglass_egui::InputDiagnostics;
use snowglass_input::provider::sdl3::{ProviderConfig, Sdl3Provider};
use snowglass_input::{
    gamepad::{GamepadState, GamepadUpdate, GamepadUpdateSink},
    hub::{InputConfiguration, InputHub, InputOperation, InputTransaction},
    source::{InputSourceCatalog, InputSourceDescriptor, InputSourceKind},
    topology::ControllerTopology,
};
use snowglass_input_dinput::DirectInputForwarder;
use snowglass_ipc::{Message, MessageSink};
use snowglass_renderer::input::PointerRoutingState;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Instant,
};

const DOM_MK_SHIFT: u32 = 0x0004;
const DOM_MK_CONTROL: u32 = 0x0008;

mod bluetooth;
mod publication;
mod reconcile;
use bluetooth::BluetoothHandle;
use reconcile::InputReconciler;
use snowglass_runtime::{ControllerCommandOutcome, ControllerManager, ControllerManagerEffect};

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

/// Opaque input capabilities exported by an active game host. The input
/// subsystem can attach without borrowing the host's emulator runtime.
pub(crate) struct GameInputEndpoint {
    control: snowglass_runtime::ProtocolInputSender,
    gamepad: Arc<dyn GamepadUpdateSink>,
}

impl GameInputEndpoint {
    pub fn new(
        control: snowglass_runtime::ProtocolInputSender,
        gamepad: Arc<dyn GamepadUpdateSink>,
    ) -> Self {
        Self { control, gamepad }
    }
}

pub(crate) struct InputRouter {
    transport: InputTransport,
    mouse_buttons: u8,
    pointer_routing: PointerRoutingState,
    direct_input: DirectInputForwarder,
    priority: InputPriority,
    cursor: PointerPosition,
    dom_mouse_keys: u32,
    dom_alt_key: bool,
    last_published_state: String,
    ui_controls: HashMap<String, i32>,
    next_publish: Instant,
    last_input_api_revision: u64,
}

pub(crate) struct InputPublication {
    pub diagnostics: InputDiagnostics,
    pub sources: serde_json::Value,
    pub routed: Vec<serde_json::Value>,
    pub state_changed: Option<bool>,
    pub priority: &'static str,
    pub api_state: Option<serde_json::Value>,
}

pub(crate) struct InputConfigurationPublication {
    pub configuration: serde_json::Value,
    pub layouts: serde_json::Value,
}

pub(crate) struct PreparedInputTransaction {
    transaction: InputTransaction,
    previous: snowglass_runtime::ResolvedInputConfiguration,
    next: snowglass_runtime::ResolvedInputConfiguration,
}

impl PreparedInputTransaction {
    pub(crate) fn candidate(&self) -> &InputConfiguration {
        &self.next.configuration
    }
}

impl InputRouter {
    pub fn new(transport: InputTransport, direct_input: DirectInputForwarder) -> Self {
        Self {
            transport,
            mouse_buttons: 0,
            pointer_routing: PointerRoutingState::new(Instant::now()),
            direct_input,
            priority: InputPriority::Ui,
            cursor: PointerPosition::default(),
            dom_mouse_keys: 0,
            dom_alt_key: false,
            last_published_state: String::new(),
            ui_controls: HashMap::new(),
            next_publish: Instant::now(),
            last_input_api_revision: 0,
        }
    }

    pub fn configuration_snapshot(&self) -> InputConfiguration {
        self.transport.hub().snapshot()
    }

    pub fn controller_topology(&self) -> Result<ControllerTopology, String> {
        self.transport.controller_topology()
    }

    pub fn controller_topology_for(
        &self,
        configuration: &InputConfiguration,
    ) -> Result<ControllerTopology, String> {
        self.transport.controller_topology_for(configuration)
    }

    pub fn layouts(&self) -> &'static [snowglass_input::layout::InputLayout] {
        self.transport.hub().layouts()
    }

    pub fn configuration_publication(&self) -> InputConfigurationPublication {
        let configuration =
            serde_json::to_value(self.configuration_snapshot()).unwrap_or(serde_json::Value::Null);
        let layouts = serde_json::to_value(self.layouts()).unwrap_or(serde_json::Value::Null);
        InputConfigurationPublication {
            configuration,
            layouts,
        }
    }

    pub fn configuration_result(
        request_id: u64,
        result: Result<InputConfiguration, String>,
    ) -> (serde_json::Value, serde_json::Value) {
        let result = match result {
            Ok(configuration) => serde_json::json!({
                "ok": true,
                "configuration": configuration,
            }),
            Err(error) => serde_json::json!({
                "ok": false,
                "error": error,
            }),
        };
        let command = serde_json::json!({
            "type": "inputConfigurationResult",
            "id": request_id,
            "result": result,
        });
        (result, command)
    }

    pub fn attach(&self, endpoint: GameInputEndpoint) {
        self.transport.attach(endpoint);
    }

    pub fn detach(&self) {
        self.transport.detach();
    }

    pub fn add_mouse_delta(&self, delta_x: f64, delta_y: f64) {
        if self.priority == InputPriority::Game {
            self.transport.provider.add_mouse_delta([delta_x, delta_y]);
        }
    }

    /// Returns true when the key was projected into the active gamepad data
    /// plane. Unmapped keys remain available for the coordinator to publish as
    /// neutral game input events.
    pub fn route_game_key(&self, key: &str, pressed: bool) -> bool {
        if !self.transport.provider.is_key_mapped(key) {
            return false;
        }
        self.transport.provider.set_key(key.to_owned(), pressed);
        true
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
        set_flag(&mut self.dom_mouse_keys, DOM_MK_SHIFT, shift);
        set_flag(&mut self.dom_mouse_keys, DOM_MK_CONTROL, control);
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
        phase: snowglass_input::mouse::PointerPhase,
        button: u32,
        horizontal: i32,
        vertical: i32,
        presentation: &snowglass_renderer::Presentation,
        plan: &snowglass_renderer::CompositingPlan,
        pointer: &snowglass_renderer::SurfacePointerSender,
    ) {
        if !self.pointer_routing.send(
            phase,
            button,
            horizontal,
            vertical,
            presentation,
            plan,
            pointer,
        ) {
            if phase == snowglass_input::mouse::PointerPhase::Move {
                self.direct_input.reset_mouse_position();
                self.transport.provider.set_pointer(None);
            }
            return;
        }
        let Some(sent) = self.pointer_routing.last_sent() else {
            return;
        };
        match phase {
            snowglass_input::mouse::PointerPhase::Move => {
                self.direct_input.mouse_position(
                    sent.surface_id,
                    sent.x,
                    sent.y,
                    sent.coordinate_width,
                    sent.coordinate_height,
                );
                self.transport
                    .provider
                    .set_pointer(GamepadState::normalized_pointer_axes(
                        sent.normalized_x,
                        sent.normalized_y,
                    ));
            }
            snowglass_input::mouse::PointerPhase::ButtonDown
            | snowglass_input::mouse::PointerPhase::ButtonUp => self.direct_input.mouse_button(
                button as u16,
                phase == snowglass_input::mouse::PointerPhase::ButtonDown,
            ),
            snowglass_input::mouse::PointerPhase::Wheel => self.direct_input.mouse_wheel(vertical),
            snowglass_input::mouse::PointerPhase::Cancel => {}
        }
    }

    pub fn cancel_pointer_connection(
        &mut self,
        connection: u64,
        presentation: &snowglass_renderer::Presentation,
        plan: &snowglass_renderer::CompositingPlan,
        pointer: &snowglass_renderer::SurfacePointerSender,
    ) {
        if let Some(buttons) = self
            .pointer_routing
            .cancel(connection, presentation, plan, pointer)
        {
            self.direct_input.release_mouse_buttons(buttons);
        }
    }

    pub fn prepare_priority_change(&mut self) {
        self.direct_input.reset_mouse_position();
        self.transport.provider.clear_keys();
        self.transport.provider.set_pointer(None);
    }

    pub fn set_provider_enabled(&self, enabled: bool) {
        self.transport.provider.set_enabled(enabled);
    }

    pub fn clear_provider_state(&self) {
        self.transport.provider.clear_keys();
        self.transport.provider.set_pointer(None);
        self.transport.provider.set_enabled(false);
    }

    pub fn focus_lost_buttons(&self) -> Vec<(u16, u8)> {
        [(0, 1), (1, 4), (2, 2), (3, 8), (4, 16)]
            .into_iter()
            .filter(|(_, flag)| self.mouse_buttons & flag != 0)
            .collect()
    }

    pub fn finish_focus_lost(&mut self) {
        reset_focus_state(
            &mut self.mouse_buttons,
            &mut self.dom_mouse_keys,
            &mut self.dom_alt_key,
        );
        self.transport.provider.clear_keys();
        self.transport.provider.set_pointer(None);
        self.direct_input.reset_mouse_position();
    }

    pub fn publication_due(&mut self, now: Instant) -> bool {
        if now < self.next_publish {
            return false;
        }
        self.next_publish = now + std::time::Duration::from_millis(16);
        true
    }

    pub fn route_ui_controls(&mut self, next: HashMap<String, i32>) -> Vec<(String, i32)> {
        let mut changed = next
            .iter()
            .filter_map(|(key, &value)| {
                (self.ui_controls.get(key).copied().unwrap_or_default() != value)
                    .then(|| (key.clone(), value))
            })
            .collect::<Vec<_>>();
        changed.extend(self.ui_controls.iter().filter_map(|(key, &value)| {
            (value != 0 && !next.contains_key(key)).then(|| (key.clone(), 0))
        }));
        self.ui_controls = next;
        changed
    }

    pub fn observe_published_state(&mut self, json: &str) -> bool {
        if self.last_published_state == json {
            return false;
        }
        self.last_published_state.clear();
        self.last_published_state.push_str(json);
        true
    }

    fn observe_input_api_state(&mut self) -> Option<serde_json::Value> {
        let snapshot = self.transport.input_api.snapshot();
        let revision = snapshot["revision"].as_u64().unwrap_or_default();
        if revision == self.last_input_api_revision {
            return None;
        }
        self.last_input_api_revision = revision;
        Some(snapshot)
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

    /// Keyboard events are offered to the shell document for shortcuts
    /// before falling through to the priority owner.
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
        let previous = self.transport.hub().snapshot();
        let transaction = topology.input_transaction(machine, previous.revision)?;
        let next = self
            .transport
            .hub()
            .preview(&transaction)
            .map_err(|error| error.to_string())?;
        self.transport.reconcile(&previous, &next, false)?;
        self.transport
            .hub()
            .apply(transaction)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn prepare_configuration_transaction(
        &mut self,
        transaction: InputTransaction,
        topology_locked: bool,
    ) -> Result<PreparedInputTransaction, String> {
        let previous = self.transport.hub().snapshot();
        let next = self
            .transport
            .hub()
            .preview(&transaction)
            .map_err(|error| error.to_string())?;
        let previous = self
            .transport
            .controller_manager
            .resolve_configuration(&previous);
        let next = self
            .transport
            .controller_manager
            .resolve_configuration(&next);
        InputReconciler::validate(&previous, &next, topology_locked)?;
        Ok(PreparedInputTransaction {
            transaction,
            previous,
            next,
        })
    }

    pub fn commit_configuration_transaction(
        &mut self,
        prepared: PreparedInputTransaction,
    ) -> Result<InputConfiguration, String> {
        InputReconciler::apply_reconciliation(
            &self.transport.provider,
            &prepared.previous,
            &prepared.next,
        )?;
        self.transport
            .sync_physical_routes(&prepared.next.configuration);
        self.transport
            .hub()
            .apply(prepared.transaction)
            .map_err(|error| error.to_string())
    }

    pub async fn controller_command(
        &mut self,
        command: serde_json::Value,
        topology_locked: bool,
    ) -> Result<(serde_json::Value, bool), String> {
        self.transport
            .apply_controller_command(command, topology_locked)
            .await
    }
}

fn reset_focus_state(mouse_buttons: &mut u8, dom_mouse_keys: &mut u32, dom_alt_key: &mut bool) {
    *mouse_buttons = 0;
    *dom_mouse_keys = 0;
    *dom_alt_key = false;
}

fn set_flag(flags: &mut u32, flag: u32, enabled: bool) {
    if enabled {
        *flags |= flag;
    } else {
        *flags &= !flag;
    }
}

#[derive(Clone, Default)]
pub struct RoutedInputSender(Arc<RwLock<Option<snowglass_runtime::ProtocolInputSender>>>);

impl RoutedInputSender {
    fn attach(&self, sender: snowglass_runtime::ProtocolInputSender) {
        *self.0.write() = Some(sender);
    }

    fn detach(&self) {
        *self.0.write() = None;
    }
}

impl MessageSink for RoutedInputSender {
    fn send_message(&self, message: Message) -> bool {
        self.0
            .read()
            .as_ref()
            .is_some_and(|sender| sender.send(message))
    }
}

#[derive(Clone, Default)]
struct RoutedGamepadSink(Arc<RwLock<RoutedGamepadState>>);

#[derive(Default)]
struct RoutedGamepadState {
    target: Option<Arc<dyn GamepadUpdateSink>>,
    latest: BTreeMap<u8, GamepadUpdate>,
}

impl RoutedGamepadSink {
    fn attach(&self, target: Arc<dyn GamepadUpdateSink>) {
        let updates = {
            let mut state = self.0.write();
            state.target = Some(target.clone());
            state.latest.values().copied().collect::<Vec<_>>()
        };
        for update in updates {
            target.publish(update);
        }
    }

    fn detach(&self) {
        let detached = {
            let mut state = self.0.write();
            state
                .target
                .take()
                .map(|target| (target, state.latest.values().copied().collect::<Vec<_>>()))
        };
        if let Some((target, updates)) = detached {
            for update in updates {
                target.publish(GamepadUpdate {
                    sequence: update.sequence.wrapping_add(1),
                    connected: false,
                    state: GamepadState::default(),
                    ..update
                });
            }
        }
    }
}

impl GamepadUpdateSink for RoutedGamepadSink {
    fn publish(&self, update: GamepadUpdate) {
        let target = {
            let mut state = self.0.write();
            if update.connected {
                state.latest.insert(update.slot.get(), update);
            } else {
                state.latest.remove(&update.slot.get());
            }
            state.target.clone()
        };
        if let Some(target) = target {
            target.publish(update);
        }
    }
}

pub struct InputTransport {
    control: RoutedInputSender,
    gamepad: RoutedGamepadSink,
    provider: Sdl3Provider,
    hub: InputHub,
    sources: InputSourceCatalog,
    input_api: snowglass_runtime::InputApiState,
    bluetooth: BluetoothHandle,
    physical_router: snowglass_input::router::PhysicalInputRouter,
    controller_manager: ControllerManager,
}

impl InputTransport {
    pub async fn runtime(
        sources: InputSourceCatalog,
        input_api: snowglass_runtime::InputApiState,
    ) -> Result<Self, String> {
        let control = RoutedInputSender::default();
        let gamepad = RoutedGamepadSink::default();
        let hub = InputHub::new(None, []).expect("an empty topology is valid");
        let provider = Sdl3Provider::start_disabled(
            ProviderConfig {
                slots: Vec::new(),
                ..ProviderConfig::default()
            },
            Arc::new(gamepad.clone()),
        )?;
        let physical_router =
            snowglass_input::router::PhysicalInputRouter::new(Arc::new(gamepad.clone()));
        let bluetooth = BluetoothHandle::start(physical_router.clone(), sources.clone());
        let directories =
            snowglass_library::Directories::discover().map_err(|error| error.to_string())?;
        let controller_manager = ControllerManager::open(
            directories.storage().join("input.sqlite3"),
            input_api.controller_profiles(),
        )
        .await?;

        let transport = Self {
            control,
            gamepad,
            provider,
            hub,
            sources,
            input_api,
            bluetooth,
            physical_router,
            controller_manager,
        };
        transport.publish_input_api();
        Ok(transport)
    }

    pub fn attach(&self, endpoint: GameInputEndpoint) {
        self.control.attach(endpoint.control);
        self.gamepad.attach(endpoint.gamepad);
    }

    pub fn detach(&self) {
        self.gamepad.detach();
        self.control.detach();
    }

    /// Reconciles one validated configuration snapshot with the live provider.
    /// Launch setup and frontend transactions both enter through this path.
    pub fn reconcile(
        &mut self,
        previous: &InputConfiguration,
        next: &InputConfiguration,
        topology_locked: bool,
    ) -> Result<(), String> {
        let previous = self.controller_manager.resolve_configuration(previous);
        let next = self.controller_manager.resolve_configuration(next);
        InputReconciler::reconcile(&self.provider, &previous, &next, topology_locked)?;
        self.sync_physical_routes(&next.configuration);
        Ok(())
    }

    pub fn devices(&self) -> Vec<snowglass_input::provider::sdl3::PhysicalDeviceState> {
        let devices = self.provider.devices();
        self.sources.replace_connected(
            InputSourceKind::Sdl,
            devices.iter().map(|device| InputSourceDescriptor {
                id: device.device_id.clone(),
                source: InputSourceKind::Sdl,
                name: device.name.clone(),
                connected: true,
                metadata: serde_json::json!({
                    "vendorId": device.vendor,
                    "productId": device.product,
                    "guid": device.guid,
                    "serial": device.serial,
                }),
            }),
        );
        devices
    }

    pub fn control_sender(&self) -> RoutedInputSender {
        self.control.clone()
    }

    pub fn hub(&self) -> &InputHub {
        &self.hub
    }

    pub fn controller_topology(&self) -> Result<ControllerTopology, String> {
        self.controller_topology_for(&self.hub.snapshot())
    }

    pub fn controller_topology_for(
        &self,
        configuration: &InputConfiguration,
    ) -> Result<ControllerTopology, String> {
        self.controller_manager
            .topology_for(configuration, &self.sources)
    }

    pub async fn apply_controller_command(
        &mut self,
        command: serde_json::Value,
        topology_locked: bool,
    ) -> Result<(serde_json::Value, bool), String> {
        let outcome = self
            .controller_manager
            .command(command, &self.bluetooth.snapshot())
            .await?;
        let response = outcome.response.clone();
        let changed = self.apply_controller_outcome(outcome, topology_locked)?;
        self.publish_input_api();
        Ok((response, changed))
    }

    pub fn observe_controllers(&mut self, topology_locked: bool) -> Result<bool, String> {
        let controllers = self.bluetooth.controllers();
        self.controller_manager
            .sync_configuration(&self.hub.snapshot(), &self.sources);
        let outcome =
            self.controller_manager
                .observe(&self.provider.devices(), &controllers, &self.sources);
        let changed = self.apply_controller_outcome(outcome, topology_locked)?;
        self.publish_input_api();
        Ok(changed)
    }

    fn publish_input_api(&self) {
        self.input_api.publish(
            self.controller_manager.snapshot(&self.bluetooth.snapshot()),
            &self.controller_manager,
        );
    }

    fn sync_physical_routes(&self, configuration: &InputConfiguration) {
        let routes = self.controller_manager.routes_for(configuration);
        let players = routes
            .iter()
            .flat_map(|route| {
                route
                    .sources
                    .iter()
                    .map(move |source| (source.as_str().to_owned(), route.slot))
            })
            .collect();
        self.physical_router.set_routes(routes);
        self.bluetooth.assign_players(players);
    }

    fn apply_controller_outcome(
        &mut self,
        outcome: ControllerCommandOutcome,
        topology_locked: bool,
    ) -> Result<bool, String> {
        for effect in outcome.effects {
            match effect {
                ControllerManagerEffect::SessionDiscovery(active) => {
                    self.bluetooth.set_session_active(active);
                }
                ControllerManagerEffect::DisconnectSources(sources) => {
                    self.bluetooth.disconnect_sources(sources);
                }
                ControllerManagerEffect::Connect(id) => self.bluetooth.connect(id),
                ControllerManagerEffect::Forget(id) => self.bluetooth.forget(id),
                ControllerManagerEffect::ReapplyMappings => {
                    let configuration = self
                        .controller_manager
                        .resolve_configuration(&self.hub.snapshot());
                    InputReconciler::apply(&self.provider, &configuration)?;
                    self.sync_physical_routes(&configuration.configuration);
                }
            }
        }
        let Some(assignments) = outcome.assignments else {
            return Ok(false);
        };
        let previous = self.hub.snapshot();
        let transaction = InputTransaction {
            expected_revision: previous.revision,
            operations: vec![InputOperation::ReplaceAssignments { assignments }],
        };
        let next = self
            .hub
            .preview(&transaction)
            .map_err(|error| error.to_string())?;
        self.reconcile(&previous, &next, topology_locked)?;
        self.hub
            .apply(transaction)
            .map_err(|error| error.to_string())?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use snowglass_input::gamepad::{GamepadButton, GamepadSlot};

    #[derive(Default)]
    struct RecordingSink(Mutex<Vec<GamepadUpdate>>);

    impl GamepadUpdateSink for RecordingSink {
        fn publish(&self, update: GamepadUpdate) {
            self.0.lock().push(update);
        }
    }

    #[test]
    fn routed_sink_replays_latest_and_neutralizes_on_detach() {
        let routed = RoutedGamepadSink::default();
        routed.publish(GamepadUpdate {
            slot: GamepadSlot::new(2).unwrap(),
            sequence: 7,
            connected: true,
            state: GamepadState {
                buttons: GamepadButton::A.mask(),
                ..GamepadState::default()
            },
        });

        let target = Arc::new(RecordingSink::default());
        routed.attach(target.clone());
        routed.detach();

        let updates = target.0.lock();
        assert_eq!(updates.len(), 2);
        assert!(updates[0].connected);
        assert_eq!(updates[0].sequence, 7);
        assert!(!updates[1].connected);
        assert_eq!(updates[1].sequence, 8);
        assert_eq!(updates[1].state, GamepadState::default());
    }

    #[test]
    fn focus_loss_clears_buttons_and_modifiers() {
        let mut mouse_buttons = 0b1_1111;
        let mut dom_mouse_keys = DOM_MK_SHIFT | DOM_MK_CONTROL;
        let mut alt = true;
        reset_focus_state(&mut mouse_buttons, &mut dom_mouse_keys, &mut alt);

        assert_eq!(mouse_buttons, 0);
        assert_eq!(dom_mouse_keys, 0);
        assert!(!alt);
    }
}
