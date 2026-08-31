use consolespec::input::BindingKey;
use parking_lot::{RwLock, RwLockReadGuard};
use snowglass_input::{GamepadReport, PointerFlags, PointerKind, PointerReport};
use snowglass_input_layout::{
    layout::{InputLayout, ProjectionMode},
    mapping::{HostSample, Mapping},
    topology::{InputTopology, PointerBinding, PointerMode, PointerSource, RemapMode},
};
use std::{collections::BTreeMap, str::FromStr};

struct CompiledPort {
    mapping: Option<Mapping>,
    pointer: PointerBinding,
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
                    RemapMode::Position => Some(ProjectionMode::Position),
                    RemapMode::Name => Some(ProjectionMode::Name),
                    RemapMode::Custom => Some(ProjectionMode::Custom),
                    RemapMode::Passthrough => None,
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
                mode.map_or(Some(None), |mode| {
                    Mapping::compile(layout, mode, &bindings).ok().map(Some)
                })
                .map(|mapping| {
                    (
                        port.player,
                        CompiledPort {
                            mapping,
                            pointer: port.pointer,
                        },
                    )
                })
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
            .and_then(|port| port.mapping.as_ref())
            .map(|mapping| mapping.apply(state, host))
    }

    pub fn pointer(&self, player: u8, device: PointerReport, host: &HostSample) -> PointerReport {
        let current = self.0.read();
        let Some(port) = current.ports.get(&player) else {
            return PointerReport::default();
        };
        let host = PointerReport {
            position: host.pointer.unwrap_or_default(),
            delta: host.delta.map(Self::delta_axis),
            flags: PointerFlags::HOST | PointerFlags::NATIVE,
            kind: PointerKind::Mouse,
        };
        let selected = match port.pointer.source {
            PointerSource::None => return PointerReport::default(),
            PointerSource::DeviceNative => device,
            PointerSource::HostMouse => host,
            PointerSource::DeviceThenHostMouse if device.kind != PointerKind::None => device,
            PointerSource::DeviceThenHostMouse => host,
        };
        match port.pointer.mode {
            PointerMode::None => PointerReport::default(),
            PointerMode::Relative => PointerReport {
                position: selected.delta,
                delta: selected.delta,
                flags: selected.flags,
                kind: selected.kind,
            },
            PointerMode::AbsoluteDevice if selected.flags.contains(PointerFlags::DEVICE) => {
                selected
            }
            PointerMode::AbsoluteDevice => PointerReport::default(),
            PointerMode::AbsoluteScreen
                if selected.flags.contains(PointerFlags::PROJECTED)
                    || selected.flags.contains(PointerFlags::HOST) =>
            {
                selected
            }
            PointerMode::AbsoluteScreen => PointerReport::default(),
        }
    }

    fn delta_axis(value: f32) -> i16 {
        (value.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
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
