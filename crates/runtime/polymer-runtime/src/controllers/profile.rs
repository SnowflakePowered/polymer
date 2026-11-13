use super::{ControllerManager, MappingUpdate};
use polymer_controllers::{
    ControllerDevice, ControllerMapping, ControllerTarget, ControllerTopology,
};

use polymer_config::input::{
    ControllerBindings, InputConfiguration, Orientation, ProjectionMode, SourceSelector,
    VirtualPort,
};
use polymer_controllers::layout::InputLayout;
use polymer_input::DeviceDescriptor;
use polymer_library::{
    InputGameKey, InputGameOverride, InputIdentityScope, InputMappingMode,
    InputMappingProfile as StoredMappingProfile,
};
use serde::Deserialize;
use std::{collections::BTreeMap, future::Future, pin::Pin};

pub(super) type MappingWrite = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(super) enum MappingScope {
    Source {
        source: SourceSelector,
    },
    Game {
        launchable: String,
        group: String,
        port: u8,
        #[serde(default)]
        source: Option<SourceSelector>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct MappingProfile {
    pub mode: ProjectionMode,
    pub orientation: Orientation,
    pub overrides: BTreeMap<String, String>,
    pub inherited: &'static str,
}

impl MappingProfile {
    pub(super) const DEFAULT: Self = Self {
        mode: ProjectionMode::Name,
        orientation: Orientation::Vertical,
        overrides: BTreeMap::new(),
        inherited: "default",
    };

    const UNPROFILED: Self = Self {
        mode: ProjectionMode::Position,
        orientation: Orientation::Vertical,
        overrides: BTreeMap::new(),
        inherited: "default",
    };

    pub(super) fn from_stored(profile: StoredMappingProfile) -> Self {
        Self {
            mode: Self::mode(profile.mode),
            orientation: if profile.horizontal {
                Orientation::Horizontal
            } else {
                Orientation::Vertical
            },
            overrides: profile.bindings,
            inherited: profile.identity_scope.as_str(),
        }
    }

    fn mode(mode: InputMappingMode) -> ProjectionMode {
        match mode {
            InputMappingMode::Name => ProjectionMode::Name,
            InputMappingMode::Position => ProjectionMode::Position,
            InputMappingMode::Custom => ProjectionMode::Custom,
        }
    }

    fn stored_mode(mode: ProjectionMode) -> InputMappingMode {
        match mode {
            ProjectionMode::Name => InputMappingMode::Name,
            ProjectionMode::Position => InputMappingMode::Position,
            ProjectionMode::Custom => InputMappingMode::Custom,
        }
    }

    pub(crate) fn bindings(&self) -> ControllerBindings {
        ControllerBindings::try_from_strings(self.overrides.clone())
            .expect("stored controller mappings were validated before persistence")
    }

    fn response(&self, layout: &InputLayout) -> serde_json::Value {
        serde_json::json!({
            "mode": self.mode,
            "orientation": self.orientation,
            "overrides": self.overrides,
            "bindings": layout.resolved_bindings(self.mode, self.orientation, &self.bindings()),
            "inherited": self.inherited,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct GameProfile {
    mode: Option<ProjectionMode>,
    orientation: Option<Orientation>,
    overrides: BTreeMap<String, String>,
}

impl GameProfile {
    pub(super) fn from_stored(game: InputGameOverride) -> Self {
        Self {
            mode: game.mode.map(MappingProfile::mode),
            orientation: game.horizontal.map(|horizontal| {
                if horizontal {
                    Orientation::Horizontal
                } else {
                    Orientation::Vertical
                }
            }),
            overrides: game.bindings,
        }
    }

    fn over(&self, base: MappingProfile) -> MappingProfile {
        let mut overrides = base.overrides;
        overrides.extend(self.overrides.clone());
        MappingProfile {
            mode: self.mode.unwrap_or(base.mode),
            orientation: self.orientation.unwrap_or(base.orientation),
            overrides,
            inherited: "game",
        }
    }

    fn response(
        &self,
        layout: &InputLayout,
        stored: bool,
        base: MappingProfile,
    ) -> serde_json::Value {
        let resolved = self.over(base);
        serde_json::json!({
            "mode": self.mode,
            "orientation": self.orientation,
            "overrides": self.overrides,
            "bindings": layout.resolved_bindings(resolved.mode, resolved.orientation, &resolved.bindings()),
            "inherited": if stored { "game" } else { "default" },
        })
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
                let profile = self.resolved_profile(
                    source.as_ref(),
                    descriptor.as_ref(),
                    self.game.as_deref(),
                    &port.port,
                    &port.layout,
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
                    projection_mode: profile.mode,
                    orientation: profile.orientation,
                    mapping: profile.bindings(),
                    pointer: polymer_controllers::topology::PointerBinding::default(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let topology = ControllerTopology { mappings };
        topology.validate()?;
        Ok(topology)
    }

    pub(crate) fn resolved_profile(
        &self,
        source: Option<&SourceSelector>,
        device: Option<&DeviceDescriptor>,
        game: Option<&str>,
        port: &VirtualPort,
        layout: &str,
    ) -> MappingProfile {
        let base = match (device, source) {
            (Some(device), _) => self.profile_for(device, layout),
            (None, Some(SourceSelector::KeyboardMouse)) => self.keyboard_profile(layout),
            (None, _) => MappingProfile::UNPROFILED,
        };
        let key = game.map(|launchable| InputGameKey {
            launchable: launchable.to_owned(),
            group: port.group.clone(),
            port: port.index,
            layout_id: layout.to_owned(),
        });
        match key.and_then(|key| self.mappings.games.get(&key).map(|game| game.clone())) {
            Some(game) => game.over(base),
            None => base,
        }
    }

    fn base_profile(&self, source: Option<&SourceSelector>, layout: &str) -> MappingProfile {
        match source {
            Some(SourceSelector::KeyboardMouse) => self.keyboard_profile(layout),
            Some(SourceSelector::Device { device_id }) => self
                .descriptor(device_id)
                .map_or(MappingProfile::DEFAULT, |device| {
                    self.profile_for(&device, layout)
                }),
            None => MappingProfile::DEFAULT,
        }
    }

    fn keyboard_profile(&self, layout_id: &str) -> MappingProfile {
        let mut profile = self
            .mappings
            .get("keyboardMouse", layout_id)
            .unwrap_or(MappingProfile::UNPROFILED);
        if profile.mode != ProjectionMode::Custom
            && let Some(layout) = InputLayout::get(layout_id)
        {
            let mut overrides = layout
                .keyboard_bindings()
                .map(|(target, source)| (target.to_string(), source.to_string()))
                .collect::<BTreeMap<_, _>>();
            overrides.extend(profile.overrides);
            profile.overrides = overrides;
        }
        profile
    }

    pub fn projection_mode(&self, device: &DeviceDescriptor, layout: &str) -> ProjectionMode {
        self.profile_for(device, layout).mode
    }

    fn profile_for(&self, device: &DeviceDescriptor, layout: &str) -> MappingProfile {
        self.mappings
            .get(&Self::descriptor_identity(device), layout)
            .unwrap_or(MappingProfile::DEFAULT)
    }

    fn source_identity(&self, source: &SourceSelector) -> String {
        match source {
            SourceSelector::KeyboardMouse => "keyboardMouse".to_owned(),
            SourceSelector::Device { device_id } => self
                .descriptor(device_id)
                .as_ref()
                .map_or_else(|| device_id.clone(), Self::descriptor_identity),
        }
    }

    fn game_key(launchable: &str, group: &str, port: u8, layout: &InputLayout) -> InputGameKey {
        InputGameKey {
            launchable: launchable.to_owned(),
            group: group.to_owned(),
            port,
            layout_id: layout.id.clone(),
        }
    }

    fn layout(layout_id: &str) -> Result<&'static InputLayout, String> {
        InputLayout::get(layout_id).ok_or_else(|| format!("unknown input layout '{layout_id}'"))
    }

    pub(super) fn mapping_response(
        &self,
        scope: &MappingScope,
        layout_id: &str,
    ) -> Result<serde_json::Value, String> {
        let layout = Self::layout(layout_id)?;
        Ok(match scope {
            MappingScope::Source {
                source: SourceSelector::KeyboardMouse,
            } => self.keyboard_profile(&layout.id).response(layout),
            MappingScope::Source { source } => self
                .mappings
                .get(&self.source_identity(source), &layout.id)
                .unwrap_or(MappingProfile::DEFAULT)
                .response(layout),
            MappingScope::Game {
                launchable,
                group,
                port,
                source,
            } => {
                let key = Self::game_key(launchable, group, *port, layout);
                let game = self.mappings.games.get(&key).map(|game| game.clone());
                game.as_ref().unwrap_or(&GameProfile::default()).response(
                    layout,
                    game.is_some(),
                    self.base_profile(source.as_ref(), &layout.id),
                )
            }
        })
    }

    fn validate_update(layout: &InputLayout, update: &MappingUpdate) -> Result<(), String> {
        if update.orientation == Some(Orientation::Horizontal) && !layout.horizontal {
            return Err(format!(
                "input layout {} cannot be held horizontally",
                layout.id
            ));
        }
        let bindings = ControllerBindings::try_from_strings(update.overrides.clone())?;
        if let Some((target, _)) = bindings
            .iter()
            .find(|(target, _)| !layout.custom_binding_targets().any(|key| key == **target))
        {
            return Err(format!(
                "input layout {} has no target '{target}'",
                layout.id
            ));
        }
        layout.validate_controller_bindings(&bindings)
    }

    pub(super) fn set_mapping(
        &mut self,
        scope: MappingScope,
        layout_id: &str,
        update: MappingUpdate,
    ) -> Result<(MappingWrite, serde_json::Value), String> {
        let layout = Self::layout(layout_id)?;
        Self::validate_update(layout, &update)?;
        let mut store = self
            .mapping_store
            .clone()
            .ok_or_else(|| "the input mapping store is unavailable".to_owned())?;
        let mappings = self.mappings.clone();
        match scope {
            MappingScope::Source { source } => {
                let identity = self.source_identity(&source);
                let scope = Self::identity_scope(&identity);
                let profile = MappingProfile {
                    mode: update.mode.unwrap_or(MappingProfile::DEFAULT.mode),
                    orientation: update.orientation.unwrap_or_default(),
                    overrides: update.overrides,
                    inherited: scope.as_str(),
                };
                let response = profile.response(layout);
                let stored = StoredMappingProfile {
                    identity: identity.clone(),
                    identity_scope: scope,
                    layout_id: layout.id.clone(),
                    mode: MappingProfile::stored_mode(profile.mode),
                    horizontal: profile.orientation == Orientation::Horizontal,
                    revision: 0,
                    bindings: profile.overrides.clone(),
                };
                let write = async move {
                    store
                        .put_mapping(&stored)
                        .await
                        .map_err(|error| error.to_string())?;
                    mappings.put(identity, stored.layout_id, profile);
                    Ok(())
                };
                Ok((Box::pin(write), response))
            }
            MappingScope::Game {
                launchable,
                group,
                port,
                source,
            } => {
                let key = Self::game_key(&launchable, &group, port, layout);
                let base = self.base_profile(source.as_ref(), &layout.id);
                let game = GameProfile {
                    mode: update.mode,
                    orientation: update.orientation,
                    overrides: update.overrides,
                };
                let response = game.response(layout, true, base);
                let stored = InputGameOverride {
                    key: key.clone(),
                    mode: game.mode.map(MappingProfile::stored_mode),
                    horizontal: game
                        .orientation
                        .map(|orientation| orientation == Orientation::Horizontal),
                    bindings: game.overrides.clone(),
                };
                let write = async move {
                    store
                        .put_game_override(&stored)
                        .await
                        .map_err(|error| error.to_string())?;
                    mappings.games.insert(key, game);
                    Ok(())
                };
                Ok((Box::pin(write), response))
            }
        }
    }

    pub(super) fn reset_mapping(
        &mut self,
        scope: MappingScope,
        layout_id: &str,
    ) -> Result<(MappingWrite, serde_json::Value), String> {
        let layout = Self::layout(layout_id)?;
        let mut store = self
            .mapping_store
            .clone()
            .ok_or_else(|| "the input mapping store is unavailable".to_owned())?;
        let mappings = self.mappings.clone();
        match scope {
            MappingScope::Source { source } => {
                let identity = self.source_identity(&source);
                let layout_id = layout.id.clone();
                let write = async move {
                    store
                        .remove_mapping(&identity, &layout_id)
                        .await
                        .map_err(|error| error.to_string())?;
                    mappings.remove(&identity, &layout_id);
                    Ok(())
                };
                Ok((Box::pin(write), MappingProfile::DEFAULT.response(layout)))
            }
            MappingScope::Game {
                launchable,
                group,
                port,
                source,
            } => {
                let key = Self::game_key(&launchable, &group, port, layout);
                let base = self.base_profile(source.as_ref(), &layout.id);
                let write = async move {
                    store
                        .remove_game_override(&key)
                        .await
                        .map_err(|error| error.to_string())?;
                    mappings.games.remove(&key);
                    Ok(())
                };
                Ok((
                    Box::pin(write),
                    GameProfile::default().response(layout, false, base),
                ))
            }
        }
    }

    pub(super) fn descriptor_identity(device: &DeviceDescriptor) -> String {
        if device.id.backend() == polymer_input::BackendId::BLUETOOTH && device.serial.is_none() {
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
