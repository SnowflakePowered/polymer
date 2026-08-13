//! Shell policy for physical input capture.
//!
//! The emulator instance owns IPC. This adapter only gives long-lived shell
//! input objects a stable endpoint while game instances are replaced.

use consolespec::input::BindingKey;
use parking_lot::RwLock;
use snowglass_input::provider::sdl3::{DeviceSelector, ProviderConfig, Sdl3Provider, SlotConfig};
use snowglass_input::{
    gamepad::{GamepadState, GamepadUpdate, GamepadUpdateSink},
    hub::{
        BindingOverride, InputConfiguration, InputHub, InputOperation, InputTransaction,
        PortConfiguration, SourceControl, SourceSelector, VirtualPort,
    },
    layout::{InputBinding, SdlAxis, SdlButton},
};
use snowglass_input_dinput::DirectInputForwarder;
use snowglass_ipc::{Message, MessageSink};
use snowglass_renderer::cli::GamepadArgument;
use snowglass_renderer::input::PointerRoutingState;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Instant,
};

const DOM_MK_LBUTTON: u32 = 0x0001;
const DOM_MK_RBUTTON: u32 = 0x0002;
const DOM_MK_SHIFT: u32 = 0x0004;
const DOM_MK_CONTROL: u32 = 0x0008;
const DOM_MK_MBUTTON: u32 = 0x0010;
const DOM_MK_XBUTTON1: u32 = 0x0020;
const DOM_MK_XBUTTON2: u32 = 0x0040;
const DOM_MOUSE_BUTTONS: u32 =
    DOM_MK_LBUTTON | DOM_MK_RBUTTON | DOM_MK_MBUTTON | DOM_MK_XBUTTON1 | DOM_MK_XBUTTON2;

#[derive(Debug)]
pub(crate) enum InputCommand {
    ForegroundHitRegions(Vec<ForegroundHitRegion>),
    Configure {
        request_id: u64,
        transaction: snowglass_input::hub::InputTransaction,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputPriority {
    Game,
    Foreground,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PointerTarget {
    Overlay,
    Foreground,
    Background,
    Game,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeyboardTarget {
    Overlay,
    Background,
    Foreground,
    Game,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PointerPosition {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DomPointerKind {
    Move,
    Button { button: u16, pressed: bool },
    Wheel { horizontal: bool, delta: i32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DomPointerEvent {
    pub kind: DomPointerKind,
    pub position: PointerPosition,
    pub left: bool,
    pub right: bool,
    pub middle: bool,
    pub back: bool,
    pub forward: bool,
    pub shift: bool,
    pub control: bool,
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

#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ForegroundHitRegion {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
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
    foreground_hit_regions: Vec<ForegroundHitRegion>,
    foreground_mouse_capture: bool,
    last_published_state: String,
    ui_controls: HashMap<String, i32>,
    next_publish: Instant,
    guide_chord_held: bool,
}

impl InputRouter {
    pub fn new(transport: InputTransport, direct_input: DirectInputForwarder) -> Self {
        Self {
            transport,
            mouse_buttons: 0,
            pointer_routing: PointerRoutingState::new(Instant::now()),
            direct_input,
            priority: InputPriority::Foreground,
            cursor: PointerPosition::default(),
            dom_mouse_keys: 0,
            dom_alt_key: false,
            foreground_hit_regions: Vec::new(),
            foreground_mouse_capture: false,
            last_published_state: String::new(),
            ui_controls: HashMap::new(),
            next_publish: Instant::now(),
            guide_chord_held: false,
        }
    }

    pub fn provider(&self) -> Option<&Sdl3Provider> {
        self.transport.provider()
    }

    pub fn configuration_snapshot(&self) -> InputConfiguration {
        self.transport.hub().snapshot()
    }

    pub fn layouts(&self) -> &'static [snowglass_input::layout::InputLayout] {
        self.transport.hub().layouts()
    }

    pub fn attach(&self, endpoint: GameInputEndpoint) {
        self.transport.attach(endpoint);
    }

    pub fn detach(&self) {
        self.transport.detach();
    }

    pub fn devices(&self) -> Vec<snowglass_input::provider::sdl3::PhysicalDeviceState> {
        self.transport.devices()
    }

    pub fn add_mouse_delta(&self, delta_x: f64, delta_y: f64) {
        if self.priority == InputPriority::Game
            && let Some(provider) = self.provider()
        {
            provider.add_mouse_delta([delta_x, delta_y]);
        }
    }

    /// Returns true when the key was projected into the active gamepad data
    /// plane. Unmapped keys remain available for the coordinator to publish as
    /// neutral game input events.
    pub fn route_game_key(&self, key: &str, pressed: bool) -> bool {
        let Some(provider) = self
            .provider()
            .filter(|provider| provider.is_key_mapped(key))
        else {
            return false;
        };
        provider.set_key(key.to_owned(), pressed);
        true
    }

    pub const fn priority(&self) -> InputPriority {
        self.priority
    }

    pub fn replace_priority(&mut self, priority: InputPriority) -> InputPriority {
        std::mem::replace(&mut self.priority, priority)
    }

    pub fn replace_foreground_hit_regions(&mut self, regions: Vec<ForegroundHitRegion>) {
        self.foreground_hit_regions = regions
            .into_iter()
            .filter(|region| {
                region.x.is_finite()
                    && region.y.is_finite()
                    && region.width.is_finite()
                    && region.height.is_finite()
                    && region.width > 0.0
                    && region.height > 0.0
            })
            .collect();
    }

    pub fn clear_foreground_hit_regions(&mut self) {
        self.foreground_hit_regions.clear();
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

    pub fn foreground_pointer_event(&mut self, kind: DomPointerKind) -> DomPointerEvent {
        foreground_pointer_event(&mut self.dom_mouse_keys, self.cursor, kind)
    }

    pub fn foreground_buttons_active(&self) -> bool {
        self.dom_mouse_keys & DOM_MOUSE_BUTTONS != 0
    }

    pub fn set_foreground_mouse_capture(&mut self, captured: bool) {
        self.foreground_mouse_capture = captured;
    }

    pub fn captured_connection(&self) -> Option<u64> {
        self.pointer_routing.captured_connection()
    }

    pub fn has_active_pointer_sequence(&self) -> bool {
        self.captured_connection().is_some()
            || self.foreground_buttons_active()
            || self.foreground_mouse_capture
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
                self.provider().map(|provider| {
                    provider.set_pointer(None);
                });
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
                if let Some(provider) = self.provider() {
                    provider.set_pointer(Some(pointer_axes(sent.normalized_x, sent.normalized_y)));
                }
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
        if let Some(provider) = self.provider() {
            provider.clear_keys();
            provider.set_pointer(None);
        }
    }

    pub fn set_provider_enabled(&self, enabled: bool) {
        if let Some(provider) = self.provider() {
            provider.set_enabled(enabled);
        }
    }

    pub fn clear_provider_state(&self) {
        if let Some(provider) = self.provider() {
            provider.clear_keys();
            provider.set_pointer(None);
            provider.set_enabled(false);
        }
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
            &mut self.foreground_mouse_capture,
            &mut self.guide_chord_held,
        );
        if let Some(provider) = self.provider() {
            provider.clear_keys();
            provider.set_pointer(None);
        }
        self.direct_input.reset_mouse_position();
    }

    pub fn publication_due(&mut self, now: Instant) -> bool {
        if now < self.next_publish {
            return false;
        }
        self.next_publish = now + std::time::Duration::from_millis(16);
        true
    }

    pub fn observe_guide_button(&mut self, pressed: bool) -> bool {
        let newly_pressed = pressed && !self.guide_chord_held;
        self.guide_chord_held = pressed;
        newly_pressed
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

    pub fn set_mouse_button(&mut self, flag: u8, pressed: bool) -> u8 {
        let previous = self.mouse_buttons;
        if pressed {
            self.mouse_buttons |= flag;
        } else {
            self.mouse_buttons &= !flag;
        }
        previous
    }

    pub fn foreground_hit_test(&self) -> bool {
        let x = f64::from(self.cursor.x);
        let y = f64::from(self.cursor.y);
        self.foreground_hit_regions.iter().any(|region| {
            x >= region.x
                && x < region.x + region.width
                && y >= region.y
                && y < region.y + region.height
        })
    }

    pub fn foreground_owns_pointer(
        &self,
        overlay_consumed: bool,
        require_idle_mouse: bool,
    ) -> bool {
        self.priority == InputPriority::Foreground
            || self.foreground_mouse_capture
            || self.foreground_region_claims(overlay_consumed, require_idle_mouse)
    }

    pub fn foreground_region_claims(
        &self,
        overlay_consumed: bool,
        require_idle_mouse: bool,
    ) -> bool {
        !overlay_consumed
            && self.pointer_routing.captured_connection().is_none()
            && (!require_idle_mouse || self.mouse_buttons == 0)
            && self.foreground_hit_test()
    }

    /// Chooses the first subsystem offered a pointer event. A background DOM
    /// handler may decline it, in which case the coordinator continues to the
    /// game surface beneath it.
    pub fn pointer_target(
        &self,
        overlay_consumed: bool,
        require_idle_mouse: bool,
        background_hit: bool,
    ) -> PointerTarget {
        if overlay_consumed
            && !self.foreground_mouse_capture
            && self.pointer_routing.captured_connection().is_none()
        {
            return PointerTarget::Overlay;
        }
        if self.foreground_owns_pointer(overlay_consumed, require_idle_mouse) {
            PointerTarget::Foreground
        } else if background_hit {
            PointerTarget::Background
        } else {
            PointerTarget::Game
        }
    }

    /// Keyboard events are offered to the background document for shortcuts
    /// before falling through to the priority owner.
    pub fn keyboard_target(
        &self,
        overlay_consumed: bool,
        background_prevented: bool,
    ) -> KeyboardTarget {
        if background_prevented {
            KeyboardTarget::Background
        } else if overlay_consumed && self.priority == InputPriority::Foreground {
            KeyboardTarget::Overlay
        } else if self.priority == InputPriority::Game {
            KeyboardTarget::Game
        } else {
            KeyboardTarget::Foreground
        }
    }

    pub fn configure_launch(
        &mut self,
        machine: &str,
        mappings: &[GamepadMapping],
    ) -> Result<(), String> {
        let mut bindings = Vec::new();
        let mut ports = Vec::new();
        let mut group_indices: HashMap<String, u8> = HashMap::new();
        for mapping in mappings {
            let slot = u8::try_from(mapping.slot)
                .map_err(|_| "virtual pad slot exceeds 255".to_owned())?;
            let mut overrides = HashMap::new();
            for (target, source) in &mapping.mapping {
                let target = target
                    .parse::<BindingKey>()
                    .map_err(|error| error.to_string())?;
                let binding = parse_input_binding(source)?;
                overrides.insert(target, hub_binding(&binding));
                bindings.push((mapping.slot, target, binding));
            }
            let port = match mapping.port.clone() {
                Some(port) => port,
                None => {
                    let specification = consolespec::MachineSpec::try_from(machine)
                        .map_err(|error| error.to_string())?;
                    let group = specification.group_for(&mapping.layout)?;
                    let index = group_indices.entry(group.name().to_owned()).or_insert(0);
                    let port = VirtualPort::new(group.name(), *index);
                    *index = index.checked_add(1).ok_or_else(|| {
                        format!("input group '{}' exceeds 255 ports", group.name())
                    })?;
                    port
                }
            };
            ports.push(PortConfiguration {
                port,
                layout: mapping.layout.clone(),
                slot: Some(slot),
                source: Some(hub_source(mapping.selector())),
                overrides,
            });
        }
        let hub = self.transport.hub();
        hub.apply(InputTransaction {
            expected_revision: hub.snapshot().revision,
            operations: vec![InputOperation::ReplacePorts {
                machine: machine.to_owned(),
                ports,
            }],
        })
        .map_err(|error| error.to_string())?;

        let mut slots = mappings
            .iter()
            .map(|mapping| mapping.slot)
            .max()
            .map(|slot| {
                vec![
                    SlotConfig {
                        selector: DeviceSelector::Index(usize::MAX),
                        layout_id: "XBOX_CONTROLLER".to_owned(),
                        force_mouse: false,
                    };
                    slot + 1
                ]
            })
            .unwrap_or_default();
        for mapping in mappings {
            slots[mapping.slot] = SlotConfig {
                selector: mapping.selector(),
                layout_id: mapping.layout.clone(),
                force_mouse: mapping.mouse,
            };
        }
        self.transport.restart_provider(ProviderConfig {
            slots,
            gamecontroller_database: None,
            keyboard_mapping_directory: None,
        })?;
        if let Some(provider) = self.transport.provider() {
            for (slot, target, binding) in bindings {
                provider.set_binding(slot, target, binding);
            }
        }
        Ok(())
    }

    /// Applies one DOM-requested configuration transaction and projects the
    /// resulting bindings into the live SDL provider. The coordinator only
    /// publishes the returned immutable snapshot; hub mutation and provider
    /// compatibility remain input-subsystem concerns.
    pub fn apply_configuration_transaction(
        &mut self,
        transaction: InputTransaction,
        emulator_attached: bool,
    ) -> Result<InputConfiguration, String> {
        let topology_change = transaction.operations.iter().any(|operation| {
            matches!(
                operation,
                InputOperation::ReplacePorts { .. }
                    | InputOperation::ConfigurePort { .. }
                    | InputOperation::SetSource { .. }
                    | InputOperation::SetLayout { .. }
                    | InputOperation::RemovePort { .. }
            )
        });
        if emulator_attached && topology_change {
            return Err(
                "changing controller topology while an emulator is attached is not supported by the current SDL hook"
                    .to_owned(),
            );
        }

        let current = self.transport.hub().snapshot();
        if emulator_attached
            && let Some(binding) = transaction.operations.iter().find_map(|operation| {
                let InputOperation::SetOverride { port, binding, .. } = operation else {
                    return None;
                };
                let source = current
                    .ports
                    .iter()
                    .find(|configured| configured.port == *port)
                    .and_then(|configured| configured.source.as_ref());
                (!binding_supported_by_attached_provider(binding, source)).then_some(binding)
            })
        {
            return Err(format!(
                "binding {binding:?} cannot yet be projected by the SDL data plane"
            ));
        }

        let configuration = self
            .transport
            .hub()
            .apply(transaction)
            .map_err(|error| error.to_string())?;
        if topology_change {
            self.transport.restart_from_configuration(&configuration)?;
        }
        if let Some(provider) = self.transport.provider() {
            project_bindings(provider, &configuration);
        }
        Ok(configuration)
    }
}

fn reset_focus_state(
    mouse_buttons: &mut u8,
    dom_mouse_keys: &mut u32,
    dom_alt_key: &mut bool,
    foreground_mouse_capture: &mut bool,
    guide_chord_held: &mut bool,
) {
    *mouse_buttons = 0;
    *dom_mouse_keys = 0;
    *dom_alt_key = false;
    *foreground_mouse_capture = false;
    *guide_chord_held = false;
}

fn set_flag(flags: &mut u32, flag: u32, enabled: bool) {
    if enabled {
        *flags |= flag;
    } else {
        *flags &= !flag;
    }
}

fn foreground_pointer_event(
    keys: &mut u32,
    position: PointerPosition,
    kind: DomPointerKind,
) -> DomPointerEvent {
    if let DomPointerKind::Button { button, pressed } = kind {
        let flag = match button {
            1 => DOM_MK_LBUTTON,
            2 => DOM_MK_RBUTTON,
            3 => DOM_MK_MBUTTON,
            4 => DOM_MK_XBUTTON1,
            5 => DOM_MK_XBUTTON2,
            _ => 0,
        };
        set_flag(keys, flag, pressed);
    }
    DomPointerEvent {
        kind,
        position,
        left: *keys & DOM_MK_LBUTTON != 0,
        right: *keys & DOM_MK_RBUTTON != 0,
        middle: *keys & DOM_MK_MBUTTON != 0,
        back: *keys & DOM_MK_XBUTTON1 != 0,
        forward: *keys & DOM_MK_XBUTTON2 != 0,
        shift: *keys & DOM_MK_SHIFT != 0,
        control: *keys & DOM_MK_CONTROL != 0,
    }
}

fn pointer_axes(x: f64, y: f64) -> [i16; 2] {
    let axis = |value: f64| {
        (value.clamp(0.0, 1.0) * f64::from(u16::MAX) + f64::from(i16::MIN)).round() as i16
    };
    [axis(x), axis(y)]
}

fn project_bindings(provider: &Sdl3Provider, configuration: &InputConfiguration) {
    for port in &configuration.ports {
        // Overrides are fanned out per virtual pad; a port that presents none
        // has nothing to fan out to.
        let Some(slot) = port.slot.map(usize::from) else {
            continue;
        };
        provider.reset_bindings(slot);
        for (target, binding) in &port.overrides {
            if let BindingOverride::Axis {
                source:
                    source @ (SourceControl::MouseDelta { .. } | SourceControl::MousePosition { .. }),
                transform,
            } = binding
            {
                provider.set_mouse_axis_binding(slot, target.clone(), source.clone(), *transform);
            } else if matches!(port.source, Some(SourceSelector::KeyboardMouse)) {
                let key = match binding {
                    BindingOverride::Button {
                        source: SourceControl::Key { code },
                    }
                    | BindingOverride::Axis {
                        source: SourceControl::Key { code },
                        ..
                    } => Some(code.clone()),
                    BindingOverride::Disabled => None,
                    _ => continue,
                };
                provider.set_key_binding(slot, target.clone(), key);
            } else if let Some(binding) = binding.sdl_binding() {
                provider.set_binding(slot, target.clone(), binding);
            }
        }
    }
}

fn hub_source(selector: DeviceSelector) -> SourceSelector {
    match selector {
        DeviceSelector::Keyboard => SourceSelector::KeyboardMouse,
        DeviceSelector::Index(index) => SourceSelector::SdlIndex { index },
        DeviceSelector::Name(name) => SourceSelector::SdlName { name },
        DeviceSelector::Guid(guid) => SourceSelector::SdlGuid {
            guid,
            duplicate_ordinal: 0,
        },
        DeviceSelector::GuidOrdinal {
            guid,
            duplicate_ordinal,
        } => SourceSelector::SdlGuid {
            guid,
            duplicate_ordinal,
        },
        DeviceSelector::DeviceId(device_id) => SourceSelector::Device { device_id },
    }
}

fn hub_binding(binding: &InputBinding) -> BindingOverride {
    match binding {
        InputBinding::Button { button } => BindingOverride::Button {
            source: SourceControl::GamepadButton { button: *button },
        },
        InputBinding::Axis { axis, invert } => BindingOverride::Axis {
            source: SourceControl::GamepadAxis { axis: *axis },
            transform: snowglass_input::hub::AxisTransform {
                invert: *invert,
                ..Default::default()
            },
        },
        InputBinding::Disabled { .. } => BindingOverride::Disabled,
    }
}

pub(crate) fn binding_supported_by_attached_provider(
    binding: &BindingOverride,
    source: Option<&SourceSelector>,
) -> bool {
    if matches!(
        binding,
        BindingOverride::Axis {
            source: SourceControl::MouseDelta { .. } | SourceControl::MousePosition { .. },
            ..
        }
    ) {
        return true;
    }
    if matches!(source, Some(SourceSelector::KeyboardMouse)) {
        return matches!(
            binding,
            BindingOverride::Button {
                source: SourceControl::Key { .. }
            } | BindingOverride::Axis {
                source: SourceControl::Key { .. },
                ..
            } | BindingOverride::Disabled
        );
    }
    binding.sdl_binding().is_some()
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GamepadMapping {
    pub gamepad_id: GamepadId,
    #[serde(default)]
    pub mouse: bool,
    pub slot: usize,
    #[serde(default)]
    pub port: Option<VirtualPort>,
    pub layout: String,
    #[serde(default)]
    pub mapping: HashMap<String, String>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum GamepadId {
    Index(usize),
    Name(String),
}

impl GamepadMapping {
    pub fn selector(&self) -> DeviceSelector {
        match &self.gamepad_id {
            GamepadId::Index(index) => DeviceSelector::Index(*index),
            GamepadId::Name(value) if value == "K" => DeviceSelector::Keyboard,
            GamepadId::Name(value) => {
                let compact = value.replace('-', "");
                if compact.len() == 32 && compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    DeviceSelector::Guid(compact.to_ascii_lowercase())
                } else {
                    DeviceSelector::Name(value.clone())
                }
            }
        }
    }
}

impl From<&GamepadArgument> for GamepadMapping {
    fn from(value: &GamepadArgument) -> Self {
        let gamepad_id = match &value.selector {
            DeviceSelector::Keyboard => GamepadId::Name("K".to_owned()),
            DeviceSelector::Index(index) => GamepadId::Index(*index),
            DeviceSelector::Name(name) | DeviceSelector::Guid(name) => {
                GamepadId::Name(name.clone())
            }
            DeviceSelector::GuidOrdinal { guid, .. } => GamepadId::Name(guid.clone()),
            DeviceSelector::DeviceId(device_id) => GamepadId::Name(device_id.clone()),
        };
        Self {
            gamepad_id,
            mouse: value.force_mouse,
            slot: value.slot,
            port: None,
            layout: value.layout.clone(),
            mapping: HashMap::new(),
        }
    }
}

pub fn parse_input_binding(value: &str) -> Result<InputBinding, String> {
    let value = value.trim().to_ascii_lowercase();
    if value == "disabled" || value == "none" {
        return Ok(InputBinding::Disabled { disabled: true });
    }
    let (kind, mut source) = value
        .split_once(':')
        .map_or(("button", value.as_str()), |parts| parts);
    let invert = source.starts_with('-');
    source = source.trim_start_matches(['+', '-']);
    match kind {
        "button" => {
            serde_json::from_value::<SdlButton>(serde_json::Value::String(source.to_owned()))
                .map(|button| InputBinding::Button { button })
                .map_err(|error| format!("invalid gamepad button binding '{value}': {error}"))
        }
        "axis" => serde_json::from_value::<SdlAxis>(serde_json::Value::String(source.to_owned()))
            .map(|axis| InputBinding::Axis { axis, invert })
            .map_err(|error| format!("invalid gamepad axis binding '{value}': {error}")),
        _ => Err(format!(
            "invalid mapping '{value}'; expected button:NAME, axis:NAME, or disabled"
        )),
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
}

impl InputTransport {
    pub fn runtime() -> Result<Self, String> {
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

        Ok(Self {
            control,
            gamepad,
            provider,
            hub,
        })
    }

    pub fn attach(&self, endpoint: GameInputEndpoint) {
        self.control.attach(endpoint.control);
        self.gamepad.attach(endpoint.gamepad);
    }

    pub fn detach(&self) {
        self.gamepad.detach();
        self.control.detach();
    }

    pub fn restart_provider(&mut self, config: ProviderConfig) -> Result<(), String> {
        self.provider.reconfigure_slots(config.slots)
    }

    pub fn restart_from_configuration(
        &mut self,
        configuration: &InputConfiguration,
    ) -> Result<(), String> {
        self.restart_provider(provider_config(configuration))
    }

    pub fn provider(&self) -> Option<&Sdl3Provider> {
        Some(&self.provider)
    }

    pub fn devices(&self) -> Vec<snowglass_input::provider::sdl3::PhysicalDeviceState> {
        self.provider.devices()
    }

    pub fn control_sender(&self) -> RoutedInputSender {
        self.control.clone()
    }

    pub fn hub(&self) -> &InputHub {
        &self.hub
    }
}

/// The provider is indexed by virtual pad, not by emulator port. Ports that
/// present no pad — passthrough, or simply unconfigured — contribute nothing, so
/// the slot run is dense even where the port run is not.
fn provider_config(configuration: &InputConfiguration) -> ProviderConfig {
    let highest_slot = configuration
        .ports
        .iter()
        .filter_map(|port| port.slot)
        .map(usize::from)
        .max();
    let mut slots = highest_slot
        .map(|slot| {
            vec![
                SlotConfig {
                    selector: DeviceSelector::Index(usize::MAX),
                    layout_id: "XBOX_CONTROLLER".to_owned(),
                    force_mouse: false,
                };
                slot + 1
            ]
        })
        .unwrap_or_default();
    for port in &configuration.ports {
        let (Some(slot), Some(source)) = (port.slot, &port.source) else {
            continue;
        };
        slots[usize::from(slot)] = SlotConfig {
            selector: match source {
                SourceSelector::KeyboardMouse => DeviceSelector::Keyboard,
                SourceSelector::SdlIndex { index } => DeviceSelector::Index(*index),
                SourceSelector::SdlName { name } => DeviceSelector::Name(name.clone()),
                SourceSelector::SdlGuid {
                    guid,
                    duplicate_ordinal,
                } => DeviceSelector::GuidOrdinal {
                    guid: guid.clone(),
                    duplicate_ordinal: *duplicate_ordinal,
                },
                SourceSelector::Device { device_id } => DeviceSelector::DeviceId(device_id.clone()),
            },
            layout_id: port.layout.clone(),
            force_mouse: false,
        };
    }
    ProviderConfig {
        slots,
        ..ProviderConfig::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use snowglass_input::{
        gamepad::{GamepadButton, GamepadSlot},
        hub::{PortConfiguration, VirtualPort},
    };
    use std::collections::HashMap;

    #[test]
    fn attached_provider_accepts_mouse_axis_but_not_mouse_button_expressions() {
        let mouse_axis = BindingOverride::Axis {
            source: SourceControl::MouseDelta {
                axis: snowglass_input::hub::PointerAxis::X,
            },
            transform: Default::default(),
        };
        assert!(binding_supported_by_attached_provider(
            &mouse_axis,
            Some(&SourceSelector::SdlIndex { index: 0 })
        ));

        let mouse_button = BindingOverride::Button {
            source: SourceControl::MouseButton { button: 1 },
        };
        assert!(!binding_supported_by_attached_provider(
            &mouse_button,
            Some(&SourceSelector::KeyboardMouse)
        ));
    }

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
    fn provider_topology_preserves_ports_and_stable_selectors() {
        // Port `controller.3` on slot 2: the runs skew, which is the whole
        // reason the two are separate fields.
        let configuration = InputConfiguration {
            revision: 1,
            machine: Some("NINTENDO_GCN".to_owned()),
            ports: vec![PortConfiguration {
                port: VirtualPort::new("controller", 3),
                layout: "GAMECUBE_CONTROLLER".to_owned(),
                slot: Some(2),
                source: Some(SourceSelector::SdlGuid {
                    guid: "abc".to_owned(),
                    duplicate_ordinal: 1,
                }),
                overrides: HashMap::new(),
            }],
        };

        let config = provider_config(&configuration);
        assert_eq!(config.slots.len(), 3);
        assert_eq!(config.slots[2].layout_id, "GAMECUBE_CONTROLLER");
        assert_eq!(
            config.slots[2].selector,
            DeviceSelector::GuidOrdinal {
                guid: "abc".to_owned(),
                duplicate_ordinal: 1,
            }
        );
    }

    #[test]
    fn pointer_policy_respects_priority_capture_overlay_and_hit_regions() {
        let transport = InputTransport::runtime().unwrap();
        let direct = DirectInputForwarder::new(transport.control_sender());
        let mut router = InputRouter::new(transport, direct);
        router.foreground_hit_regions = vec![ForegroundHitRegion {
            x: 10.0,
            y: 20.0,
            width: 30.0,
            height: 40.0,
        }];
        router.cursor = PointerPosition { x: 15, y: 25 };
        router.priority = InputPriority::Game;
        assert!(router.foreground_owns_pointer(false, true));
        assert!(!router.foreground_owns_pointer(true, true));
        assert_eq!(
            router.pointer_target(false, true, true),
            PointerTarget::Foreground
        );
        assert_eq!(
            router.pointer_target(true, true, true),
            PointerTarget::Overlay
        );
        router.cursor = PointerPosition { x: 100, y: 100 };
        assert!(!router.foreground_owns_pointer(false, true));
        assert_eq!(
            router.pointer_target(false, true, true),
            PointerTarget::Background
        );
        assert_eq!(
            router.pointer_target(false, true, false),
            PointerTarget::Game
        );
        router.foreground_mouse_capture = true;
        assert!(router.foreground_owns_pointer(true, true));
        assert_eq!(
            router.pointer_target(true, true, true),
            PointerTarget::Foreground
        );
        router.foreground_mouse_capture = false;
        router.priority = InputPriority::Foreground;
        assert!(router.foreground_owns_pointer(true, true));
        assert_eq!(router.keyboard_target(true, false), KeyboardTarget::Overlay);
        router.priority = InputPriority::Game;
        assert_eq!(router.keyboard_target(true, false), KeyboardTarget::Game);
        assert_eq!(
            router.keyboard_target(false, true),
            KeyboardTarget::Background
        );
    }

    #[test]
    fn foreground_hit_regions_deserialize_as_neutral_input_snapshots() {
        let region: ForegroundHitRegion =
            serde_json::from_str(r#"{"x": 400.25, "y": 300.5, "width": 716.75, "height": 380.0}"#)
                .unwrap();
        assert_eq!((region.x, region.y), (400.25, 300.5));
        assert_eq!((region.width, region.height), (716.75, 380.0));
    }

    #[test]
    fn foreground_pointer_events_are_neutral_snapshots_of_router_state() {
        let mut keys = DOM_MK_SHIFT;
        let position = PointerPosition { x: 12, y: 46 };

        let pressed = foreground_pointer_event(
            &mut keys,
            position,
            DomPointerKind::Button {
                button: 1,
                pressed: true,
            },
        );
        assert_eq!(pressed.position, position);
        assert!(pressed.left);
        assert!(pressed.shift);
        assert!(!pressed.control);
        assert_ne!(keys & DOM_MOUSE_BUTTONS, 0);

        let released = foreground_pointer_event(
            &mut keys,
            position,
            DomPointerKind::Button {
                button: 1,
                pressed: false,
            },
        );
        assert!(!released.left);
        assert_eq!(keys & DOM_MOUSE_BUTTONS, 0);
    }

    #[test]
    fn focus_loss_clears_buttons_modifiers_capture_and_guide_chord() {
        let mut mouse_buttons = 0b1_1111;
        let mut dom_mouse_keys = DOM_MOUSE_BUTTONS | DOM_MK_SHIFT | DOM_MK_CONTROL;
        let mut alt = true;
        let mut foreground_capture = true;
        let mut guide_chord = true;

        reset_focus_state(
            &mut mouse_buttons,
            &mut dom_mouse_keys,
            &mut alt,
            &mut foreground_capture,
            &mut guide_chord,
        );

        assert_eq!(mouse_buttons, 0);
        assert_eq!(dom_mouse_keys, 0);
        assert!(!alt);
        assert!(!foreground_capture);
        assert!(!guide_chord);
    }
}
