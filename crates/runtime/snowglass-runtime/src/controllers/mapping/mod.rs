use super::{ControllerManager, DeviceMapping, ResolvedInputConfiguration, ResolvedSlotMapping};
use consolespec::input::BindingKey;
use snowglass_input::{
    hub::{InputConfiguration, SourceSelector},
    layout::{InputBinding, InputLayout, ProjectionMode},
    router::GamepadRemap,
    source::{InputSourceDescriptor, InputSourceKind},
    topology::{ControllerBindings, ControllerMapping, ControllerTarget, ControllerTopology},
};
use snowglass_library::{
    InputIdentityScope, InputMappingMode, InputMappingProfile as StoredMappingProfile,
};
use std::{
    collections::{BTreeMap, HashMap},
    str::FromStr,
};

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
        sources: &snowglass_input::source::InputSourceCatalog,
    ) -> Result<ControllerTopology, String> {
        let resolved = self.resolve_configuration(configuration);
        let mappings = configuration
            .ports
            .iter()
            .map(|port| {
                let gamepad = port.slot.ok_or_else(|| {
                    format!(
                        "runtime controller topology requires a virtual gamepad for {}",
                        port.port
                    )
                })?;
                let source = configuration.source_for(gamepad).cloned();
                let device = match &source {
                    Some(SourceSelector::Device { device_id }) => sources.get(device_id),
                    Some(SourceSelector::KeyboardMouse) | None => None,
                };
                let layout = InputLayout::get(&port.layout)
                    .ok_or_else(|| format!("unknown input layout {}", port.layout))?;
                let resolved = resolved
                    .slots
                    .get(&gamepad)
                    .expect("resolved configurations contain every assigned port");
                let projection_mode = resolved.projection_mode;
                let mapping = if matches!(source, Some(SourceSelector::Device { .. }))
                    && projection_mode != ProjectionMode::Custom
                {
                    ControllerBindings::defaults(layout)
                } else {
                    ControllerBindings::from_overrides(layout, &resolved.overrides)?
                };
                Ok(ControllerMapping {
                    target: ControllerTarget {
                        group: port.port.group.clone(),
                        port: usize::from(port.port.index),
                    },
                    gamepad,
                    source: source.clone(),
                    layout: port.layout.clone(),
                    connected: device.as_ref().is_some_and(|device| device.connected)
                        || matches!(source, Some(SourceSelector::KeyboardMouse)),
                    device,
                    mouse: port.force_mouse,
                    projection_mode,
                    mapping,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let topology = ControllerTopology { mappings };
        topology.validate()?;
        Ok(topology)
    }

    pub fn projection_mode(&self, device_id: &str, layout: &str) -> ProjectionMode {
        self.mappings
            .get(&self.mapping_identity(device_id), layout)
            .map_or(ProjectionMode::Name, |profile| profile.mode)
    }

    pub fn mapping_for_device(
        &self,
        device: &InputSourceDescriptor,
        layout: &str,
    ) -> DeviceMapping {
        let identity = Self::descriptor_identity(device);
        let profile = self.mappings.get(&identity, layout).unwrap_or_else(|| {
            MappingProfile::generated(
                ProjectionMode::Name,
                layout,
                Self::identity_scope(&identity),
            )
        });
        DeviceMapping {
            mode: profile.mode,
            mapping: profile.mapping,
        }
    }

    pub(super) fn remap_for(
        &self,
        device_id: &str,
        slot: u8,
        configuration: &InputConfiguration,
    ) -> Option<GamepadRemap> {
        let layout = configuration
            .ports
            .iter()
            .find(|port| port.slot == Some(slot))
            .and_then(|port| InputLayout::get(&port.layout))?;
        let profile = self
            .mappings
            .get(&self.mapping_identity(device_id), &layout.id)?;
        (profile.mode == ProjectionMode::Custom).then(|| {
            GamepadRemap::custom(
                layout,
                profile
                    .bindings()
                    .iter()
                    .filter_map(|(target, source)| source.map(|source| (*target, *source))),
            )
        })
    }

    pub fn resolve_configuration(
        &self,
        configuration: &InputConfiguration,
    ) -> ResolvedInputConfiguration {
        let mut slots = HashMap::new();
        for port in &configuration.ports {
            let Some(slot) = port.slot else { continue };
            let mut resolved = ResolvedSlotMapping {
                projection_mode: ProjectionMode::Position,
                overrides: port.overrides.clone(),
            };
            let Some(device_id) = port
                .slot
                .and_then(|slot| {
                    configuration.assignments.iter().find_map(|assignment| {
                        (assignment.index == slot).then_some(&assignment.source)
                    })
                })
                .and_then(|source| match source {
                    SourceSelector::Device { device_id } => Some(device_id),
                    SourceSelector::KeyboardMouse => None,
                })
            else {
                slots.insert(slot, resolved);
                continue;
            };
            let Some(layout) = InputLayout::get(&port.layout) else {
                slots.insert(slot, resolved);
                continue;
            };
            resolved.overrides.clear();
            let identity = self.mapping_identity(device_id);
            let profile = self
                .mappings
                .get(&identity, &port.layout)
                .unwrap_or_else(|| {
                    MappingProfile::generated(
                        ProjectionMode::Name,
                        &port.layout,
                        Self::identity_scope(&identity),
                    )
                });
            resolved.projection_mode = profile.mode;
            if profile.mode != ProjectionMode::Custom {
                slots.insert(slot, resolved);
                continue;
            }
            for (target, source) in profile.bindings().iter() {
                let binding = match source {
                    Some(source) => layout.custom_binding(*target, *source),
                    None => Some(InputBinding::Disabled { disabled: true }),
                };
                if let Some(binding) = binding {
                    resolved.overrides.insert(*target, binding.into());
                }
            }
            slots.insert(slot, resolved);
        }
        ResolvedInputConfiguration {
            configuration: configuration.clone(),
            slots,
        }
    }

    pub(super) fn mapping_key(
        &self,
        id: &str,
        layout_id: &str,
    ) -> Result<(String, String), String> {
        InputLayout::get(layout_id).ok_or_else(|| format!("unknown input layout '{layout_id}'"))?;
        Ok((self.mapping_identity(id), layout_id.to_owned()))
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

    pub(super) async fn set_mapping(
        &mut self,
        id: &str,
        layout_id: &str,
        update: DeviceMapping,
    ) -> Result<(), String> {
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
                        return Err(format!(
                            "input layout {layout_id} has no customizable target '{target}'"
                        ));
                    }
                    if source != "none" {
                        let source_key = BindingKey::from_str(&source)
                            .map_err(|error| format!("invalid input source '{source}': {error}"))?;
                        layout
                            .custom_binding(target_key, source_key)
                            .ok_or_else(|| {
                                format!(
                                    "input source '{source}' is not a physical gamepad expression"
                                )
                            })?;
                    }
                    Ok((target, source))
                })
                .collect::<Result<BTreeMap<_, _>, String>>()?
        } else {
            BTreeMap::new()
        };
        if mode == ProjectionMode::Custom {
            let missing = layout
                .custom_binding_targets()
                .filter(|target| !mapping.contains_key(&target.to_string()))
                .map(|target| target.to_string())
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(format!(
                    "custom mapping for {layout_id} must specify: {}",
                    missing.join(", ")
                ));
            }
        }
        let scope = Self::identity_scope(&identity);
        let stored_mode = match mode {
            ProjectionMode::Name => InputMappingMode::Name,
            ProjectionMode::Position => InputMappingMode::Position,
            ProjectionMode::Custom => InputMappingMode::Custom,
        };
        let store = self
            .mapping_store
            .as_mut()
            .ok_or_else(|| "the input mapping store is unavailable".to_owned())?;
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
        self.mappings.put(identity, layout_id, profile);
        Ok(())
    }

    fn mapping_identity(&self, id: &str) -> String {
        self.controllers.get(id).map_or_else(
            || id.to_owned(),
            |controller| {
                Self::descriptor_identity(&InputSourceDescriptor {
                    id: controller.id.clone(),
                    source: controller.source,
                    name: controller.name.clone(),
                    connected: controller.connected,
                    metadata: controller.metadata.clone(),
                })
            },
        )
    }

    fn descriptor_identity(device: &InputSourceDescriptor) -> String {
        if device.source == InputSourceKind::Bt {
            return device.id.clone();
        }
        device
            .metadata
            .get("serial")
            .and_then(serde_json::Value::as_str)
            .map(|serial| format!("sdl://serial/{serial}"))
            .unwrap_or_else(|| {
                format!(
                    "sdl://model/{}/{}/{}/{}",
                    device.metadata["vendorId"].as_u64().unwrap_or_default(),
                    device.metadata["productId"].as_u64().unwrap_or_default(),
                    device.metadata["guid"].as_str().unwrap_or_default(),
                    device.name.to_ascii_lowercase()
                )
            })
    }

    fn identity_scope(identity: &str) -> InputIdentityScope {
        if identity.starts_with("sdl://model/")
            || identity.starts_with("bt://model/")
            || identity.starts_with("sdl://type/")
            || identity.starts_with("bt://type/")
        {
            InputIdentityScope::Type
        } else {
            InputIdentityScope::Device
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ControllerProfiles;
    use consolespec::input::key;
    use snowglass_input::{
        hub::{BindingOverride, PortConfiguration, SourceControl, VirtualPort},
        layout::SdlButton,
        source::InputSourceCatalog,
    };

    #[test]
    fn live_topology_preserves_port_overrides_without_a_physical_source() {
        let configuration = InputConfiguration {
            revision: 0,
            machine: Some("MICROSOFT_XBOX".to_owned()),
            ports: vec![PortConfiguration {
                port: VirtualPort::new("controller", 0),
                layout: "XBOX_CONTROLLER".to_owned(),
                slot: Some(0),
                force_mouse: false,
                overrides: [(
                    key!(button.a),
                    BindingOverride::Button {
                        source: SourceControl::GamepadButton {
                            button: SdlButton::East,
                        },
                    },
                )]
                .into(),
            }],
            assignments: Vec::new(),
        };
        let manager = ControllerManager::with_profiles(ControllerProfiles::default());
        let topology = manager
            .topology_for(&configuration, &InputSourceCatalog::default())
            .unwrap();

        assert_eq!(
            serde_json::to_value(&topology.mappings[0].mapping).unwrap()["button.a"],
            "button.b"
        );
    }
}
