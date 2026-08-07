//! Shell policy for physical input capture.
//!
//! The emulator instance owns IPC. This adapter only gives long-lived shell
//! input objects a stable endpoint while game instances are replaced.

use parking_lot::RwLock;
use snowglass_input::provider::sdl3::{DeviceSelector, ProviderConfig, Sdl3Provider, SlotConfig};
use snowglass_input::{
    gamepad::{GamepadState, GamepadUpdate, GamepadUpdateSink},
    hub::{InputConfiguration, InputHub, SourceSelector},
};
use snowglass_ipc::{Message, MessageSink};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Default)]
pub struct RoutedInputSender(Arc<RwLock<Option<snowglass_runtime::ProtocolInputSender>>>);

impl RoutedInputSender {
    fn attach(&self, instance: &snowglass_runtime::EmulatorInstance) {
        *self.0.write() = Some(instance.protocol_input_sender());
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

    pub fn attach(&self, instance: &snowglass_runtime::EmulatorInstance) {
        self.control.attach(instance);
        self.gamepad.attach(instance.gamepad_input());
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
}
