use polymer_config::input::SourceSelector;
use polymer_controllers::ControllerTopology;
use polymer_controllers::{
    layout::InputLayout,
    mapping::{HostSample, Mapping},
};
use polymer_input::{GamepadButton, GamepadReport};
use polymer_renderer::egui::InputSlotDiagnostics;
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct KeyboardInput {
    pub sample: HostSample,
    mappings: BTreeMap<u8, Mapping>,
}

impl KeyboardInput {
    pub fn configure(&mut self, topology: &ControllerTopology) {
        self.mappings = topology
            .mappings
            .iter()
            .filter_map(|port| {
                if !port.connected || port.source != Some(SourceSelector::KeyboardMouse) {
                    return None;
                }
                let layout = InputLayout::get(&port.layout)?;
                let mapping = Mapping::compile(
                    layout,
                    port.projection_mode,
                    port.orientation,
                    &port.mapping,
                )
                .ok()?;
                Some((port.gamepad, mapping))
            })
            .collect();
    }

    pub fn diagnostics(&self, player: u8, layout: &str) -> Option<InputSlotDiagnostics> {
        let mapping = self.mappings.get(&player)?;
        let state = mapping.apply(&GamepadReport::default(), &self.sample);
        Some(InputSlotDiagnostics {
            slot: usize::from(player),
            layout: layout.to_owned(),
            connected: true,
            name: "Keyboard and mouse".to_owned(),
            pressed: GamepadButton::ALL
                .into_iter()
                .filter(|button| state.buttons & button.mask() != 0)
                .map(|button| format!("{button:?}"))
                .collect(),
            mapped_buttons: state.buttons,
            left_stick: state.left_stick,
            right_stick: state.right_stick,
            left_trigger: state.left_trigger,
            right_trigger: state.right_trigger,
            ..Default::default()
        })
    }
}
