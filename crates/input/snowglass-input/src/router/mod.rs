//! Provider-neutral routing from physical controllers to stable virtual slots.

use crate::{
    gamepad::{GamepadButton, GamepadSlot, GamepadState, GamepadUpdate, GamepadUpdateSink},
    layout::InputLayout,
    sdl::Wire,
};
use consolespec::input::BindingKey;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

/// A provider-owned identity that remains stable across reconnects.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[repr(transparent)]
pub struct PhysicalSourceId(String);

impl PhysicalSourceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalControllerRole {
    Whole,
    LeftHalf,
    RightHalf,
}

#[derive(Clone, Debug)]
pub struct PhysicalGamepadUpdate {
    pub source: PhysicalSourceId,
    pub role: PhysicalControllerRole,
    pub connected: bool,
    pub state: GamepadState,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CompositionMode {
    Single,
    JoyConPair,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotRoute {
    pub slot: u8,
    pub sources: Vec<PhysicalSourceId>,
    pub composition: CompositionMode,
    pub remap: Option<GamepadRemap>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GamepadRemap {
    buttons: Vec<(u8, u8)>,
    axes: Vec<(u8, u8, bool)>,
}

impl GamepadRemap {
    pub fn custom(
        layout: &InputLayout,
        bindings: impl IntoIterator<Item = (BindingKey, BindingKey)>,
    ) -> Self {
        let mut remap = Self {
            buttons: Vec::new(),
            axes: Vec::new(),
        };
        for (target, source) in bindings {
            let Some(binding) = layout.custom_binding(target, source) else {
                continue;
            };
            match (layout.sdl_wire(target), crate::sdl::wire(source)) {
                (Some(Wire::Button(target)), Some(Wire::Button(source))) => {
                    remap.buttons.push((target.index(), source.index()));
                }
                (Some(Wire::Axis(target)), Some(Wire::Axis(source))) => {
                    let crate::layout::InputBinding::Axis { invert, .. } = binding else {
                        continue;
                    };
                    remap.axes.push((target.index(), source.index(), invert));
                }
                _ => {}
            }
        }
        remap.buttons.sort_by_key(|(target, _)| *target);
        remap.buttons.dedup_by_key(|(target, _)| *target);
        remap.axes.sort_by_key(|(target, _, _)| *target);
        remap.axes.dedup_by_key(|(target, _, _)| *target);
        remap
    }

    fn apply(&self, source: GamepadState) -> GamepadState {
        let mut output = GamepadState {
            pointer: source.pointer,
            accelerometer: source.accelerometer,
            gyroscope: source.gyroscope,
            ..GamepadState::default()
        };
        for &(target, input) in &self.buttons {
            let Some(input) = GamepadButton::from_wire(input) else {
                continue;
            };
            let Some(target) = GamepadButton::from_wire(target) else {
                continue;
            };
            if source.button(input) {
                output.buttons |= target.mask();
            }
        }
        let source_axes = source.sdl_axes();
        let mut output_axes = output.sdl_axes();
        for &(target, input, invert) in &self.axes {
            let Some(value) = source_axes.get(usize::from(input)).copied() else {
                continue;
            };
            if let Some(target) = output_axes.get_mut(usize::from(target)) {
                *target = if invert {
                    (-i32::from(value)).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
                } else {
                    value
                };
            }
        }
        output.left_stick = [output_axes[0], output_axes[1]];
        output.right_stick = [output_axes[2], output_axes[3]];
        output.left_trigger = (i32::from(output_axes[4]) - i32::from(i16::MIN)) as u16;
        output.right_trigger = (i32::from(output_axes[5]) - i32::from(i16::MIN)) as u16;
        output
    }
}

#[derive(Clone)]
pub struct PhysicalInputRouter {
    state: Arc<RwLock<RouterState>>,
    output: Arc<dyn GamepadUpdateSink>,
}

#[derive(Default)]
struct RouterState {
    routes: Vec<SlotRoute>,
    sources: HashMap<PhysicalSourceId, SourceState>,
    sequences: HashMap<u8, u32>,
}

#[derive(Clone, Copy)]
struct SourceState {
    role: PhysicalControllerRole,
    connected: bool,
    state: GamepadState,
}

impl PhysicalInputRouter {
    pub fn new(output: Arc<dyn GamepadUpdateSink>) -> Self {
        Self {
            state: Arc::new(RwLock::new(RouterState::default())),
            output,
        }
    }

    /// Atomically replaces routing. Removed slots are explicitly neutralized;
    /// new routes immediately receive the latest retained physical state.
    pub fn set_routes(&self, routes: Vec<SlotRoute>) {
        let (removed, updates) = {
            let mut state = self.state.write();
            let old_slots = state
                .routes
                .iter()
                .map(|route| route.slot)
                .collect::<Vec<_>>();
            state.routes = routes;
            let removed_slots = old_slots
                .into_iter()
                .filter(|slot| !state.routes.iter().any(|route| route.slot == *slot))
                .collect::<Vec<_>>();
            let removed = removed_slots
                .into_iter()
                .map(|slot| sequenced(&mut state, slot, false, GamepadState::default()))
                .collect::<Vec<_>>();
            let routes = state.routes.clone();
            let updates = routes
                .iter()
                .map(|route| route_update(&mut state, route))
                .collect::<Vec<_>>();
            (removed, updates)
        };
        for update in removed.into_iter().chain(updates) {
            self.output.publish(update);
        }
    }

    pub fn routes(&self) -> Vec<SlotRoute> {
        self.state.read().routes.clone()
    }

    pub fn sources_for_slot(&self, slot: GamepadSlot) -> Vec<PhysicalSourceId> {
        self.state
            .read()
            .routes
            .iter()
            .find(|route| route.slot == slot.get())
            .map_or_else(Vec::new, |route| route.sources.clone())
    }

    pub fn publish(&self, update: PhysicalGamepadUpdate) {
        let updates = {
            let mut state = self.state.write();
            state.sources.insert(
                update.source.clone(),
                SourceState {
                    role: update.role,
                    connected: update.connected,
                    state: update.state,
                },
            );
            let routes = state
                .routes
                .iter()
                .filter(|route| route.sources.contains(&update.source))
                .cloned()
                .collect::<Vec<_>>();
            routes
                .iter()
                .map(|route| route_update(&mut state, route))
                .collect::<Vec<_>>()
        };
        for update in updates {
            self.output.publish(update);
        }
    }
}

fn route_update(state: &mut RouterState, route: &SlotRoute) -> GamepadUpdate {
    let selected = route
        .sources
        .iter()
        .filter_map(|source| state.sources.get(source).copied())
        .collect::<Vec<_>>();
    let connected = match route.composition {
        CompositionMode::Single => selected.first().is_some_and(|source| source.connected),
        CompositionMode::JoyConPair => {
            selected
                .iter()
                .any(|source| source.connected && source.role == PhysicalControllerRole::LeftHalf)
                && selected.iter().any(|source| {
                    source.connected && source.role == PhysicalControllerRole::RightHalf
                })
        }
    };
    let mut gamepad = if connected {
        compose(route.composition, &selected)
    } else {
        GamepadState::default()
    };
    if let Some(remap) = &route.remap {
        gamepad = remap.apply(gamepad);
    }
    sequenced(state, route.slot, connected, gamepad)
}

fn compose(mode: CompositionMode, selected: &[SourceState]) -> GamepadState {
    if mode == CompositionMode::Single {
        return selected
            .first()
            .map_or(GamepadState::default(), |source| source.state);
    }
    let mut output = GamepadState::default();
    for source in selected.iter().filter(|source| source.connected) {
        output.buttons |= source.state.buttons;
        output.left_trigger = output.left_trigger.max(source.state.left_trigger);
        output.right_trigger = output.right_trigger.max(source.state.right_trigger);
        match source.role {
            PhysicalControllerRole::LeftHalf => {
                output.left_stick = source.state.left_stick;
            }
            PhysicalControllerRole::RightHalf => {
                output.right_stick = source.state.right_stick;
                output.pointer = source.state.pointer;
                output.accelerometer = source.state.accelerometer;
                output.gyroscope = source.state.gyroscope;
            }
            PhysicalControllerRole::Whole => {}
        }
    }
    output
}

fn sequenced(
    state: &mut RouterState,
    slot: u8,
    connected: bool,
    gamepad: GamepadState,
) -> GamepadUpdate {
    let sequence = state.sequences.entry(slot).or_default();
    *sequence = sequence.wrapping_add(1);
    GamepadUpdate {
        slot: GamepadSlot::new(slot).expect("all u8 slot numbers are valid"),
        sequence: *sequence,
        connected,
        state: gamepad,
    }
}
