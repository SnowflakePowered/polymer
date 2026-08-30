use consolespec::input::BindingKey;
use parking_lot::{RwLock, RwLockReadGuard};
use snowglass_input::GamepadReport;
use snowglass_input_layout::{
    layout::{InputLayout, ProjectionMode},
    mapping::{HostSample, Mapping},
    topology::{InputTopology, RemapMode},
};
use std::{collections::BTreeMap, str::FromStr};

struct CompiledPort {
    mapping: Mapping,
}

#[derive(Default)]
struct Current {
    document: InputTopology,
    ports: BTreeMap<u8, CompiledPort>,
}

#[derive(Default)]
pub struct TopologyState(RwLock<Current>);

pub struct TopologyGuard<'a>(RwLockReadGuard<'a, Current>);

impl TopologyState {
    pub fn read(&self) -> TopologyGuard<'_> {
        TopologyGuard(self.0.read())
    }

    pub fn set(&self, document: InputTopology) {
        let ports = document
            .ports
            .iter()
            .filter_map(|port| {
                let layout = InputLayout::get(&port.layout)?;
                let mode = match port.mode {
                    RemapMode::Position => ProjectionMode::Position,
                    RemapMode::Name => ProjectionMode::Name,
                    RemapMode::Custom => ProjectionMode::Custom,
                    RemapMode::Passthrough => ProjectionMode::Position,
                };
                let bindings = port
                    .bindings
                    .iter()
                    .filter_map(|(target, source)| {
                        Some((
                            BindingKey::from_str(target).ok()?,
                            source
                                .as_ref()
                                .and_then(|source| BindingKey::from_str(source).ok()),
                        ))
                    })
                    .collect::<Vec<_>>();
                Mapping::compile(layout, mode, &bindings)
                    .ok()
                    .map(|mapping| (port.player, CompiledPort { mapping }))
            })
            .collect();
        *self.0.write() = Current { document, ports };
    }

    pub fn map(
        &self,
        player: u8,
        state: &GamepadReport,
        host: &HostSample,
    ) -> Option<GamepadReport> {
        self.0
            .read()
            .ports
            .get(&player)
            .map(|port| port.mapping.apply(state, host))
    }
}

impl TopologyGuard<'_> {
    pub fn document(&self) -> &InputTopology {
        &self.0.document
    }

    pub fn revision(&self) -> u32 {
        self.0.document.revision
    }

    pub fn pad_count(&self) -> usize {
        self.0.document.ports.len().min(8)
    }

    pub fn layouts(&self) -> Vec<&str> {
        let mut ports = self.0.document.ports.iter().collect::<Vec<_>>();
        ports.sort_by_key(|port| port.player);
        ports
            .into_iter()
            .take(8)
            .map(|port| port.layout.as_str())
            .collect()
    }
}
