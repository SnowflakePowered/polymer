use super::*;
use serde::Serialize;
use snowglass_egui::{InputAxisDiagnostics, InputSlotDiagnostics};
use snowglass_input::{
    gamepad::GamepadButton,
    layout::SdlButton,
    provider::sdl3::{PhysicalDeviceState, PhysicalGamepadState},
    router::SlotRoute,
};
use snowglass_input_bluetooth::BluetoothControllerState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishedButton {
    button: String,
    pressed: bool,
    mapped: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishedAxis {
    axis: serde_json::Value,
    value: i16,
    mapped: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishedDevice {
    id: String,
    kind: &'static str,
    name: String,
    assigned_indices: Vec<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    layout: Option<Option<String>>,
    buttons: Vec<PublishedButton>,
    axes: Vec<PublishedAxis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mapped: Option<GamepadState>,
}

impl PublishedDevice {
    fn sdl(
        device: PhysicalDeviceState,
        states: &[Option<PhysicalGamepadState>],
        ui_priority: bool,
        controls: &mut HashMap<String, i32>,
    ) -> Self {
        let assigned_indices = states
            .iter()
            .enumerate()
            .filter_map(|(slot, state)| {
                state
                    .as_ref()
                    .is_some_and(|state| state.device_id == device.device_id)
                    .then_some(slot)
            })
            .collect::<Vec<_>>();
        let mapped_states = assigned_indices
            .iter()
            .filter_map(|&slot| states.get(slot).and_then(Option::as_ref))
            .collect::<Vec<_>>();
        let primary = mapped_states.first().copied();
        let buttons = device
            .buttons
            .into_iter()
            .map(|(button, pressed)| {
                let global = button == SdlButton::Guide;
                let mapped = global
                    || mapped_states
                        .iter()
                        .any(|state| state.mapped_buttons.contains(&button));
                if (global || ui_priority || !mapped)
                    && let Some((_, expression)) = snowglass_input::sdl::BUTTONS
                        .iter()
                        .find(|(source, _)| source == &button)
                {
                    controls.insert(
                        format!("{}\0{expression}", device.device_id),
                        i32::from(pressed),
                    );
                }
                PublishedButton {
                    button: format!("{button:?}"),
                    pressed,
                    mapped,
                }
            })
            .collect();
        let axes = device
            .axes
            .into_iter()
            .map(|(axis, value)| {
                let mapped = mapped_states
                    .iter()
                    .any(|state| state.mapped_axes.contains(&axis));
                if (ui_priority || !mapped)
                    && let Some((_, expression)) = snowglass_input::sdl::AXES
                        .iter()
                        .find(|(source, _)| source == &axis)
                {
                    controls.insert(
                        format!("{}\0{expression}", device.device_id),
                        i32::from(value),
                    );
                }
                PublishedAxis {
                    axis: format!("{axis:?}").into(),
                    value,
                    mapped,
                }
            })
            .collect();
        Self {
            id: device.device_id,
            kind: "controller",
            name: device.name,
            assigned_indices,
            layout: Some(primary.map(|state| state.layout_id.clone())),
            buttons,
            axes,
            mapped: Some(primary.map(|state| state.mapped).unwrap_or_default()),
        }
    }

    fn bluetooth(
        controller: BluetoothControllerState,
        routes: &[SlotRoute],
        controls: &mut HashMap<String, i32>,
    ) -> Self {
        let id = controller.source_id.as_str().to_owned();
        let assigned_indices = routes
            .iter()
            .filter(|route| route.sources.contains(&controller.source_id))
            .map(|route| usize::from(route.slot))
            .collect::<Vec<_>>();
        let mapped = !assigned_indices.is_empty();
        let buttons = GamepadButton::ALL
            .into_iter()
            .map(|button| {
                let pressed = controller.state.button(button);
                if let Some((_, expression)) = snowglass_input::sdl::BUTTONS.get(button as usize) {
                    controls.insert(format!("{id}\0{expression}"), i32::from(pressed));
                }
                PublishedButton {
                    button: format!("{button:?}"),
                    pressed,
                    mapped,
                }
            })
            .collect();
        let axes = controller
            .state
            .sdl_axes()
            .into_iter()
            .enumerate()
            .map(|(axis, value)| {
                if let Some((_, expression)) = snowglass_input::sdl::AXES.get(axis) {
                    controls.insert(format!("{id}\0{expression}"), i32::from(value));
                }
                PublishedAxis {
                    axis: axis.into(),
                    value,
                    mapped,
                }
            })
            .collect();
        Self {
            id,
            kind: "controller",
            name: controller.name,
            assigned_indices,
            layout: None,
            buttons,
            axes,
            mapped: None,
        }
    }

    fn keyboard(slot: usize, state: &PhysicalGamepadState) -> Self {
        Self {
            id: state.device_id.clone(),
            kind: "keyboardMouse",
            name: state.name.clone(),
            assigned_indices: vec![slot],
            layout: Some(Some(state.layout_id.clone())),
            buttons: state
                .buttons
                .iter()
                .map(|(button, pressed)| PublishedButton {
                    button: format!("{button:?}"),
                    pressed: *pressed,
                    mapped: true,
                })
                .collect(),
            axes: state
                .axes
                .iter()
                .map(|(axis, value)| PublishedAxis {
                    axis: format!("{axis:?}").into(),
                    value: *value,
                    mapped: true,
                })
                .collect(),
            mapped: Some(state.mapped),
        }
    }
}

impl InputRouter {
    pub fn build_publication(
        &mut self,
        mappings: &[snowglass_input::topology::ControllerMapping],
        game_active: bool,
        topology_locked: bool,
    ) -> InputPublication {
        let _ = self.transport.observe_controllers(topology_locked);
        let states = self.transport.provider.states();
        let ui_priority = self.priority == InputPriority::Ui;
        let diagnostics = InputDiagnostics {
            active: self.priority == InputPriority::Game && game_active,
            slots: mappings
                .iter()
                .map(|config| {
                    let slot = usize::from(config.gamepad);
                    let Some(state) = states.get(slot).and_then(Option::as_ref) else {
                        return InputSlotDiagnostics {
                            slot,
                            layout: config.layout.clone(),
                            ..InputSlotDiagnostics::default()
                        };
                    };
                    InputSlotDiagnostics {
                        slot,
                        layout: state.layout_id.clone(),
                        connected: true,
                        name: state.name.clone(),
                        guid: state.guid.clone(),
                        pressed: state
                            .buttons
                            .iter()
                            .filter(|(_, pressed)| *pressed)
                            .map(|(button, _)| format!("{button:?}"))
                            .collect(),
                        axes: state
                            .axes
                            .iter()
                            .map(|(axis, value)| InputAxisDiagnostics {
                                name: format!("{axis:?}"),
                                value: *value,
                            })
                            .collect(),
                        mapped_buttons: state.mapped.buttons,
                        left_stick: state.mapped.left_stick,
                        right_stick: state.mapped.right_stick,
                        left_trigger: state.mapped.left_trigger,
                        right_trigger: state.mapped.right_trigger,
                        pointer: state.mapped.pointer,
                        accelerometer: state.mapped.accelerometer,
                        gyroscope: state.mapped.gyroscope,
                    }
                })
                .collect(),
        };

        let mut controls = HashMap::new();
        let mut devices = self
            .transport
            .devices()
            .into_iter()
            .map(|device| PublishedDevice::sdl(device, &states, ui_priority, &mut controls))
            .collect::<Vec<_>>();
        let routes = self.transport.physical_router.routes();
        devices.extend(
            self.transport
                .bluetooth
                .controllers()
                .into_iter()
                .filter(|controller| controller.connected)
                .map(|controller| PublishedDevice::bluetooth(controller, &routes, &mut controls)),
        );
        devices.extend(states.iter().enumerate().filter_map(|(slot, state)| {
            state
                .as_ref()
                .filter(|state| state.device_id == "keyboard-mouse")
                .map(|state| PublishedDevice::keyboard(slot, state))
        }));

        let routed = self
            .route_ui_controls(controls)
            .into_iter()
            .filter_map(|(key, value)| {
                let (source_id, expression) = key.split_once('\0')?;
                Some(serde_json::json!({
                    "sourceId": source_id,
                    "expression": expression,
                    "value": value,
                }))
            })
            .collect();
        let state_changed = serde_json::to_string(&devices)
            .ok()
            .map(|json| self.observe_published_state(&json));
        let sources =
            serde_json::to_value(&devices).unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
        InputPublication {
            diagnostics,
            sources,
            routed,
            state_changed,
            priority: if ui_priority { "ui" } else { "game" },
            api_state: self.observe_input_api_state(),
        }
    }
}
