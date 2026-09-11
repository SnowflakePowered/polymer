use super::{ControllerManager, DeviceMapping};
use polymer_controllers::{
    ControllerDevice, ControllerMapping, ControllerTarget, ControllerTopology,
};

use consolespec::input::BindingKey;
use polymer_config::input::{
    ControllerBindings, InputConfiguration, ProjectionMode, SourceSelector,
};
use polymer_controllers::layout::InputLayout;
use polymer_input::DeviceDescriptor;
use polymer_library::{
    InputIdentityScope, InputMappingMode, InputMappingProfile as StoredMappingProfile,
};
use std::{collections::BTreeMap, str::FromStr};

#[derive(Clone, Debug)]
pub(super) struct MappingProfile {
    pub mode: ProjectionMode,
    pub mapping: BTreeMap<String, String>,
    pub scope: InputIdentityScope,
}

impl MappingProfile {
    pub(super) fn generated(
        mode: ProjectionMode,
        layout_id: &str,
        scope: InputIdentityScope,
    ) -> Self {
        let mapping = InputLayout::get(layout_id)
            .map(|layout| {
                layout
                    .default_gamepad_bindings()
                    .map(|(target, source)| (target.to_string(), source.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            mode,
            mapping,
            scope,
        }
    }

    pub(super) fn from_stored(profile: StoredMappingProfile) -> Self {
        let mode = match profile.mode {
            InputMappingMode::Name => ProjectionMode::Name,
            InputMappingMode::Position => ProjectionMode::Position,
            InputMappingMode::Custom => ProjectionMode::Custom,
        };
        if mode == ProjectionMode::Custom {
            Self {
                mode,
                mapping: profile.bindings,
                scope: profile.identity_scope,
            }
        } else {
            Self::generated(mode, &profile.layout_id, profile.identity_scope)
        }
    }

    fn bindings(&self) -> ControllerBindings {
        ControllerBindings::try_from_strings(
            self.mapping
                .iter()
                .map(|(target, source)| (target.clone(), source.clone())),
        )
        .expect("stored controller mappings were validated before persistence")
    }
}

impl ControllerManager {
    pub fn topology_for(
        &self,
        configuration: &InputConfiguration,
    ) -> Result<ControllerTopology, String> {
        let mappings = configuration
            .ports
            .iter()
            .map(|port| {
                let player = port.slot.ok_or_else(|| {
                    format!(
                        "runtime controller topology requires a player for {}",
                        port.port
                    )
                })?;
                let source = configuration.source_for(player).cloned();
                let descriptor = source.as_ref().and_then(|source| match source {
                    SourceSelector::Device { device_id } => self.descriptor(device_id),
                    SourceSelector::KeyboardMouse => None,
                });
                let profile = descriptor
                    .as_ref()
                    .map(|device| self.profile_for(device, &port.layout));
                let mode = profile
                    .as_ref()
                    .map_or(ProjectionMode::Position, |profile| profile.mode);
                let layout = InputLayout::get(&port.layout)
                    .ok_or_else(|| format!("unknown input layout {}", port.layout))?;
                let mapping = profile.as_ref().map_or_else(
                    || layout.default_controller_bindings(),
                    MappingProfile::bindings,
                );
                Ok(ControllerMapping {
                    target: ControllerTarget {
                        group: port.port.group.clone(),
                        port: usize::from(port.port.index),
                    },
                    gamepad: player,
                    source,
                    layout: port.layout.clone(),
                    connected: descriptor.is_some()
                        || configuration.source_for(player) == Some(&SourceSelector::KeyboardMouse),
                    device: descriptor.as_ref().map(ControllerDevice::from_descriptor),
                    mouse: port.force_mouse,
                    projection_mode: mode,
                    mapping,
                    pointer: polymer_controllers::topology::PointerBinding::default(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let topology = ControllerTopology { mappings };
        topology.validate()?;
        Ok(topology)
    }

    pub fn projection_mode(&self, device: &DeviceDescriptor, layout: &str) -> ProjectionMode {
        self.profile_for(device, layout).mode
    }

    pub fn mapping_for_device(&self, device: &DeviceDescriptor, layout: &str) -> DeviceMapping {
        let profile = self.profile_for(device, layout);
        DeviceMapping {
            mode: profile.mode,
            mapping: profile.mapping,
        }
    }

    fn profile_for(&self, device: &DeviceDescriptor, layout: &str) -> MappingProfile {
        let identity = Self::descriptor_identity(device);
        self.mappings.get(&identity, layout).unwrap_or_else(|| {
            MappingProfile::generated(
                ProjectionMode::Name,
                layout,
                Self::identity_scope(&identity),
            )
        })
    }

    pub(super) fn mapping_key(
        &self,
        id: &str,
        layout_id: &str,
    ) -> Result<(String, String), String> {
        InputLayout::get(layout_id).ok_or_else(|| format!("unknown input layout '{layout_id}'"))?;
        let identity = self
            .descriptor(id)
            .as_ref()
            .map_or_else(|| id.to_owned(), Self::descriptor_identity);
        Ok((identity, layout_id.to_owned()))
    }

    pub(super) fn mapping_response(
        &self,
        id: &str,
        layout_id: &str,
    ) -> Result<serde_json::Value, String> {
        let (identity, layout_id) = self.mapping_key(id, layout_id)?;
        let profile = self.mappings.get(&identity, &layout_id).unwrap_or_else(|| {
            MappingProfile::generated(
                ProjectionMode::Name,
                &layout_id,
                Self::identity_scope(&identity),
            )
        });
        Ok(serde_json::json!({
            "mode": profile.mode,
            "mapping": profile.mapping,
            "scope": profile.scope.as_str(),
        }))
    }

    pub(super) fn set_mapping(
        &mut self,
        id: &str,
        layout_id: &str,
        update: DeviceMapping,
    ) -> Result<impl std::future::Future<Output = Result<(), String>> + Send + use<>, String> {
        let (identity, layout_id) = self.mapping_key(id, layout_id)?;
        let DeviceMapping { mode, mapping } = update;
        let layout = InputLayout::get(&layout_id).expect("layout was validated");
        let mapping = if mode == ProjectionMode::Custom {
            mapping
                .into_iter()
                .map(|(target, source)| {
                    let target_key = BindingKey::from_str(&target)
                        .map_err(|error| format!("invalid input target '{target}': {error}"))?;
                    if !layout.custom_binding_targets().any(|key| key == target_key) {
                        return Err(format!("input layout {layout_id} has no target '{target}'"));
                    }
                    if source != "none" {
                        let source_key = BindingKey::from_str(&source)
                            .map_err(|error| format!("invalid input source '{source}': {error}"))?;
                        layout
                            .custom_binding(target_key, source_key)
                            .ok_or_else(|| {
                                format!("input source '{source}' is not a gamepad expression")
                            })?;
                    }
                    Ok((target, source))
                })
                .collect::<Result<BTreeMap<_, _>, String>>()?
        } else {
            BTreeMap::new()
        };
        let scope = Self::identity_scope(&identity);
        let stored_mode = match mode {
            ProjectionMode::Name => InputMappingMode::Name,
            ProjectionMode::Position => InputMappingMode::Position,
            ProjectionMode::Custom => InputMappingMode::Custom,
        };
        let mut store = self
            .mapping_store
            .clone()
            .ok_or_else(|| "the input mapping store is unavailable".to_owned())?;
        let mappings = self.mappings.clone();
        Ok(async move {
            store
                .put_mapping(&StoredMappingProfile {
                    identity: identity.clone(),
                    identity_scope: scope,
                    layout_id: layout_id.clone(),
                    mode: stored_mode,
                    revision: 0,
                    bindings: mapping.clone(),
                })
                .await
                .map_err(|error| error.to_string())?;
            let profile = if mode == ProjectionMode::Custom {
                MappingProfile {
                    mode,
                    mapping,
                    scope,
                }
            } else {
                MappingProfile::generated(mode, &layout_id, scope)
            };
            mappings.put(identity, layout_id, profile);
            Ok(())
        })
    }

    pub(super) fn descriptor_identity(device: &DeviceDescriptor) -> String {
        if device.id.backend() == polymer_input::BackendId::BLUETOOTH {
            return device.id.to_string();
        }
        device.serial.as_ref().map_or_else(
            || {
                format!(
                    "sdl:model/{}/{}/{}",
                    device.vendor_id.unwrap_or_default(),
                    device.product_id.unwrap_or_default(),
                    device.name.to_ascii_lowercase()
                )
            },
            |serial| format!("sdl:serial/{serial}"),
        )
    }

    pub(super) fn identity_scope(identity: &str) -> InputIdentityScope {
        if identity.starts_with("sdl:model/") {
            InputIdentityScope::Type
        } else {
            InputIdentityScope::Device
        }
    }
}
