//! Runtime-owned controller connection, player assignment, and mapping policy.

use parking_lot::RwLock;
use snowglass_input::{
    hub::{InputConfiguration, SourceAssignment, SourceSelector},
    layout::{InputLayout, ProjectionMode},
    provider::sdl3::PhysicalDeviceState,
    router::{CompositionMode, PhysicalControllerRole, PhysicalSourceId, SlotRoute},
    source::{InputSourceCatalog, InputSourceDescriptor, InputSourceKind},
};
use snowglass_input_bluetooth::BluetoothControllerState;
use snowglass_library::InputStore;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::Arc,
};

mod mapping;
use mapping::MappingProfile;

const BASE_PLAYERS: u8 = 4;
const SESSION_PLAYERS: u8 = 8;

#[derive(Clone, Debug)]
pub enum ControllerManagerEffect {
    SessionDiscovery(bool),
    AssignPlayers(Vec<(String, u8)>),
    DisconnectSources(Vec<String>),
    Connect(String),
    Forget(String),
    ReapplyMappings,
}

pub struct ControllerCommandOutcome {
    pub response: serde_json::Value,
    pub assignments: Option<Vec<SourceAssignment>>,
    pub effects: Vec<ControllerManagerEffect>,
}

#[derive(Clone, Debug)]
pub struct DeviceMapping {
    pub mode: ProjectionMode,
    pub mapping: BTreeMap<String, String>,
}

#[derive(Clone, Default)]
pub struct ControllerProfiles(Arc<RwLock<HashMap<String, HashMap<String, MappingProfile>>>>);

impl ControllerProfiles {
    fn clear(&self) {
        self.0.write().clear();
    }

    fn get(&self, identity: &str, layout: &str) -> Option<MappingProfile> {
        self.0
            .read()
            .get(identity)
            .and_then(|profiles| profiles.get(layout))
            .cloned()
    }

    fn put(&self, identity: String, layout: String, profile: MappingProfile) {
        self.0
            .write()
            .entry(identity)
            .or_default()
            .insert(layout, profile);
    }

    fn remove(&self, identity: &str, layout: &str) {
        let mut mappings = self.0.write();
        if let Some(profiles) = mappings.get_mut(identity) {
            profiles.remove(layout);
            if profiles.is_empty() {
                mappings.remove(identity);
            }
        }
    }
}

pub struct ControllerManager {
    session: Option<ConnectionSession>,
    players: BTreeMap<u8, Option<String>>,
    controllers: BTreeMap<String, LogicalController>,
    source_controllers: HashMap<String, String>,
    routes: HashMap<String, ControllerRoute>,
    neutral: HashSet<String>,
    mappings: ControllerProfiles,
    mapping_store: Option<InputStore>,
}

struct ConnectionSession {
    id: String,
}

#[derive(Clone)]
struct LogicalController {
    id: String,
    name: String,
    source: InputSourceKind,
    connected: bool,
    metadata: serde_json::Value,
    route: ControllerRoute,
}

#[derive(Clone)]
struct ControllerRoute {
    sources: Vec<String>,
    composition: CompositionMode,
}

impl ControllerManager {
    pub(crate) fn with_profiles(mappings: ControllerProfiles) -> Self {
        Self {
            session: None,
            players: BTreeMap::new(),
            controllers: BTreeMap::new(),
            source_controllers: HashMap::new(),
            routes: HashMap::new(),
            neutral: HashSet::new(),
            mappings,
            mapping_store: None,
        }
    }

    pub async fn open(
        mapping_path: impl AsRef<Path>,
        mappings: ControllerProfiles,
    ) -> Result<Self, String> {
        let store = InputStore::open(mapping_path)
            .await
            .map_err(|error| error.to_string())?;
        let profiles = store.profiles().await.map_err(|error| error.to_string())?;
        let mut manager = Self::with_profiles(mappings);
        manager.mappings.clear();
        for profile in profiles {
            let identity = profile.identity.clone();
            let layout = profile.layout_id.clone();
            manager
                .mappings
                .put(identity, layout, MappingProfile::from_stored(profile));
        }
        manager.mapping_store = Some(store);
        Ok(manager)
    }

    pub fn snapshot(&self, bluetooth: &serde_json::Value) -> serde_json::Value {
        let devices = self
            .controllers
            .values()
            .map(|controller| self.device_json(controller))
            .collect::<Vec<_>>();
        let visible_players = self.visible_player_count();
        let players = (0..visible_players)
            .map(|index| {
                let controller = self
                    .players
                    .get(&index)
                    .and_then(|id| id.as_ref())
                    .and_then(|id| self.controllers.get(id))
                    .map(|controller| self.device_json(controller))
                    .unwrap_or(serde_json::Value::Null);
                serde_json::json!({ "index": index, "controller": controller })
            })
            .collect::<Vec<_>>();
        let candidates = bluetooth["devices"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|candidate| {
                let id = candidate["id"].as_str().unwrap_or_default();
                serde_json::json!({
                    "id": id,
                    "source": "bt",
                    "name": candidate["name"],
                    "connected": candidate["connected"],
                    "playerIndex": self.player_for(id),
                    "metadata": {},
                })
            })
            .collect::<Vec<_>>();
        let active_session = self.session.as_ref().map(|session| {
            serde_json::json!({
                "id": session.id,
                "active": true,
                "discovery": {
                    "active": bluetooth["discovery"]["active"].as_bool().unwrap_or(false),
                },
                "candidates": candidates,
                "players": players,
            })
        });
        serde_json::json!({
            "availability": bluetooth["availability"].clone(),
            "devices": devices,
            "players": players,
            "activeConnectionSession": active_session,
            "layouts": InputLayout::all(),
        })
    }

    pub fn routes_for(&self, configuration: &InputConfiguration) -> Vec<SlotRoute> {
        configuration
            .assignments
            .iter()
            .filter_map(|assignment| {
                let SourceSelector::Device { device_id } = &assignment.source else {
                    return None;
                };
                let route =
                    self.routes
                        .get(device_id)
                        .cloned()
                        .unwrap_or_else(|| ControllerRoute {
                            sources: vec![device_id.clone()],
                            composition: CompositionMode::Single,
                        });
                device_id.starts_with("bt://").then(|| SlotRoute {
                    slot: assignment.index,
                    sources: route
                        .sources
                        .into_iter()
                        .map(PhysicalSourceId::new)
                        .collect(),
                    composition: route.composition,
                    remap: self.remap_for(device_id, assignment.index, configuration),
                })
            })
            .collect()
    }

    pub fn sync_configuration(
        &mut self,
        configuration: &InputConfiguration,
        sources: &InputSourceCatalog,
    ) {
        if !self.players.is_empty() || self.session.is_some() {
            return;
        }
        for assignment in &configuration.assignments {
            let SourceSelector::Device { device_id } = &assignment.source else {
                continue;
            };
            let descriptor = sources.get(device_id);
            let route = self
                .routes
                .get(device_id)
                .cloned()
                .unwrap_or_else(|| ControllerRoute {
                    sources: vec![device_id.clone()],
                    composition: CompositionMode::Single,
                });
            let controller = LogicalController {
                id: device_id.clone(),
                name: descriptor
                    .as_ref()
                    .map_or_else(|| "Controller".to_owned(), |value| value.name.clone()),
                source: descriptor
                    .as_ref()
                    .map_or(InputSourceKind::Sdl, |value| value.source),
                connected: descriptor.is_some_and(|value| value.connected),
                metadata: serde_json::json!({}),
                route,
            };
            self.controllers.insert(device_id.clone(), controller);
            self.players
                .insert(assignment.index, Some(device_id.clone()));
        }
    }

    pub fn observe(
        &mut self,
        system: &[PhysicalDeviceState],
        bluetooth: &[BluetoothControllerState],
        sources: &InputSourceCatalog,
    ) -> ControllerCommandOutcome {
        let previous = self.assignment_signature();
        self.observe_system(system);
        self.observe_bluetooth(bluetooth);
        self.refresh_connections(system, bluetooth);
        self.observe_joycon_pair(bluetooth);
        for controller in self.controllers.values() {
            sources.upsert(InputSourceDescriptor {
                id: controller.id.clone(),
                source: controller.source,
                name: controller.name.clone(),
                connected: controller.connected,
                metadata: controller.metadata.clone(),
            });
        }
        let assignments =
            (previous != self.assignment_signature()).then(|| self.source_assignments());
        let effects = assignments
            .as_ref()
            .map(|_| {
                vec![ControllerManagerEffect::AssignPlayers(
                    self.player_sources(),
                )]
            })
            .unwrap_or_default();
        ControllerCommandOutcome {
            response: serde_json::Value::Null,
            assignments,
            effects,
        }
    }

    pub async fn command(
        &mut self,
        command: serde_json::Value,
        bluetooth: &serde_json::Value,
    ) -> Result<ControllerCommandOutcome, String> {
        let kind = command
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "controller command requires a type".to_owned())?;
        let mut effects = Vec::new();
        let mut assignments = None;
        match kind {
            "controllers.session.start" => {
                if self.session.is_some() {
                    return Err("a controller connection session is already active".to_owned());
                }
                let bluetooth_sources = self
                    .controllers
                    .values()
                    .filter(|controller| controller.source == InputSourceKind::Bt)
                    .flat_map(|controller| controller.route.sources.clone())
                    .collect();
                self.players.clear();
                self.controllers.clear();
                self.source_controllers.clear();
                self.routes.clear();
                self.neutral.clear();
                self.session = Some(ConnectionSession {
                    id: uuid::Uuid::new_v4().to_string(),
                });
                assignments = Some(self.source_assignments());
                effects.push(ControllerManagerEffect::SessionDiscovery(true));
                effects.push(ControllerManagerEffect::DisconnectSources(
                    bluetooth_sources,
                ));
            }
            "controllers.session.finish" => {
                self.require_session(&command)?;
                self.session = None;
                effects.push(ControllerManagerEffect::SessionDiscovery(false));
            }
            "controllers.session.connect" => {
                self.require_session(&command)?;
                effects.push(ControllerManagerEffect::Connect(Self::id_field(&command)?));
            }
            "controllers.disconnect" => {
                let id = Self::id_field(&command)?;
                let controller = self
                    .controllers
                    .get(&id)
                    .ok_or_else(|| format!("unknown controller '{id}'"))?;
                effects.push(ControllerManagerEffect::DisconnectSources(
                    controller.route.sources.clone(),
                ));
                for assigned in self.players.values_mut() {
                    if assigned.as_deref() == Some(id.as_str()) {
                        *assigned = None;
                    }
                }
                assignments = Some(self.source_assignments());
            }
            "controllers.forget" => {
                let id = Self::id_field(&command)?;
                if let Some(controller) = self.controllers.remove(&id) {
                    self.players.values_mut().for_each(|assigned| {
                        if assigned.as_deref() == Some(id.as_str()) {
                            *assigned = None;
                        }
                    });
                    for source in controller.route.sources {
                        self.source_controllers.remove(&source);
                    }
                    assignments = Some(self.source_assignments());
                }
                effects.push(ControllerManagerEffect::Forget(id));
            }
            "controllers.mapping.get" => {
                return Ok(ControllerCommandOutcome {
                    response: self.mapping_response(&command)?,
                    assignments: None,
                    effects,
                });
            }
            "controllers.mapping.set" => {
                self.set_mapping(&command).await?;
                return Ok(ControllerCommandOutcome {
                    response: self.mapping_response(&command)?,
                    assignments: None,
                    effects: vec![ControllerManagerEffect::ReapplyMappings],
                });
            }
            "controllers.mapping.reset" => {
                let (identity, layout) = self.mapping_key(&command)?;
                let store = self
                    .mapping_store
                    .as_mut()
                    .ok_or_else(|| "the input mapping store is unavailable".to_owned())?;
                store
                    .remove_mapping(&identity, &layout)
                    .await
                    .map_err(|error| error.to_string())?;
                self.mappings.remove(&identity, &layout);
                return Ok(ControllerCommandOutcome {
                    response: self.mapping_response(&command)?,
                    assignments: None,
                    effects: vec![ControllerManagerEffect::ReapplyMappings],
                });
            }
            _ => return Err(format!("unknown controller command '{kind}'")),
        }
        let response = self.snapshot(bluetooth);
        if assignments.is_some() {
            effects.push(ControllerManagerEffect::AssignPlayers(
                self.player_sources(),
            ));
        }
        Ok(ControllerCommandOutcome {
            response,
            assignments,
            effects,
        })
    }

    fn observe_system(&mut self, system: &[PhysicalDeviceState]) {
        for device in system {
            let source = device.device_id.clone();
            self.upsert_single(
                source.clone(),
                device.name.clone(),
                InputSourceKind::Sdl,
                serde_json::json!({
                    "vendorId": device.vendor,
                    "productId": device.product,
                    "guid": device.guid,
                    "serial": device.serial,
                }),
            );
            if self.session.is_none() {
                self.assign_if_open(&source, BASE_PLAYERS);
                continue;
            }
            let eligible = device.buttons.iter().any(|(button, pressed)| {
                *pressed
                    && matches!(
                        button,
                        snowglass_input::layout::SdlButton::South
                            | snowglass_input::layout::SdlButton::East
                            | snowglass_input::layout::SdlButton::West
                            | snowglass_input::layout::SdlButton::North
                            | snowglass_input::layout::SdlButton::Start
                            | snowglass_input::layout::SdlButton::Guide
                    )
            });
            self.join_edge(source, eligible, SESSION_PLAYERS);
        }
    }

    fn observe_bluetooth(&mut self, bluetooth: &[BluetoothControllerState]) {
        for controller in bluetooth.iter().filter(|controller| controller.connected) {
            let source = controller.source_id.as_str().to_owned();
            self.upsert_single(
                source.clone(),
                controller.name.clone(),
                InputSourceKind::Bt,
                serde_json::json!({
                    "profileId": controller.profile_id,
                    "batteryLevel": controller.battery_level,
                    "charging": controller.charging,
                }),
            );
            if self.session.is_none() {
                self.assign_if_open(&source, BASE_PLAYERS);
                continue;
            }
            let sideways = [
                snowglass_input::gamepad::GamepadButton::LeftPaddle1,
                snowglass_input::gamepad::GamepadButton::RightPaddle1,
                snowglass_input::gamepad::GamepadButton::LeftPaddle2,
                snowglass_input::gamepad::GamepadButton::RightPaddle2,
            ]
            .into_iter()
            .filter(|button| controller.state.button(*button))
            .count()
                >= 2;
            let generic = [
                snowglass_input::gamepad::GamepadButton::A,
                snowglass_input::gamepad::GamepadButton::B,
                snowglass_input::gamepad::GamepadButton::X,
                snowglass_input::gamepad::GamepadButton::Y,
                snowglass_input::gamepad::GamepadButton::Start,
                snowglass_input::gamepad::GamepadButton::Guide,
            ]
            .into_iter()
            .any(|button| controller.state.button(button));
            let shoulders = controller
                .state
                .button(snowglass_input::gamepad::GamepadButton::L)
                && controller
                    .state
                    .button(snowglass_input::gamepad::GamepadButton::R);
            let switch_controller = controller.profile_id.starts_with("nintendo.switch");
            let eligible = if switch_controller {
                sideways || controller.role == PhysicalControllerRole::Whole && shoulders
            } else {
                generic
            };
            self.join_edge(source, eligible, SESSION_PLAYERS);
        }
    }

    fn observe_joycon_pair(&mut self, bluetooth: &[BluetoothControllerState]) {
        if self.session.is_none() {
            return;
        }
        let left = bluetooth.iter().find(|controller| {
            controller.connected
                && controller.role == PhysicalControllerRole::LeftHalf
                && controller
                    .state
                    .button(snowglass_input::gamepad::GamepadButton::L)
        });
        let right = bluetooth.iter().find(|controller| {
            controller.connected
                && controller.role == PhysicalControllerRole::RightHalf
                && controller
                    .state
                    .button(snowglass_input::gamepad::GamepadButton::R)
        });
        let (Some(left), Some(right)) = (left, right) else {
            return;
        };
        let left_id = left.source_id.as_str().to_owned();
        let right_id = right.source_id.as_str().to_owned();
        let existing = [&left_id, &right_id]
            .into_iter()
            .filter_map(|source| {
                self.source_controllers.get(source).and_then(|logical| {
                    self.player_for(logical)
                        .map(|index| (index, logical.clone()))
                })
            })
            .collect::<Vec<_>>();
        let index = existing
            .iter()
            .map(|(index, _)| *index)
            .min()
            .or_else(|| self.next_open(SESSION_PLAYERS));
        let Some(index) = index else { return };
        let pair_id = Self::pair_id(&left_id, &right_id);
        if self.controllers.contains_key(&pair_id) {
            return;
        }
        for (_, old) in &existing {
            self.controllers.remove(old);
            self.routes.remove(old);
        }
        let replaced = existing.iter().map(|(_, id)| id).collect::<HashSet<_>>();
        for assigned in self.players.values_mut() {
            if assigned.as_ref().is_some_and(|id| replaced.contains(id)) {
                *assigned = None;
            }
        }
        let route = ControllerRoute {
            sources: vec![left_id.clone(), right_id.clone()],
            composition: CompositionMode::JoyConPair,
        };
        self.routes.insert(pair_id.clone(), route.clone());
        self.controllers.insert(
            pair_id.clone(),
            LogicalController {
                id: pair_id.clone(),
                name: "Joy-Con Pair".to_owned(),
                source: InputSourceKind::Bt,
                connected: true,
                metadata: serde_json::json!({}),
                route,
            },
        );
        self.source_controllers.insert(left_id, pair_id.clone());
        self.source_controllers.insert(right_id, pair_id.clone());
        self.players.insert(index, Some(pair_id));
    }

    fn refresh_connections(
        &mut self,
        system: &[PhysicalDeviceState],
        bluetooth: &[BluetoothControllerState],
    ) {
        let connected = system
            .iter()
            .map(|device| device.device_id.as_str())
            .chain(
                bluetooth
                    .iter()
                    .filter(|controller| controller.connected)
                    .map(|controller| controller.source_id.as_str()),
            )
            .collect::<HashSet<_>>();
        for controller in self.controllers.values_mut() {
            controller.connected = controller
                .route
                .sources
                .iter()
                .all(|source| connected.contains(source.as_str()));
        }
    }

    fn upsert_single(
        &mut self,
        id: String,
        name: String,
        source: InputSourceKind,
        metadata: serde_json::Value,
    ) {
        if self.source_controllers.contains_key(&id) {
            if let Some(logical) = self
                .source_controllers
                .get(&id)
                .and_then(|logical| self.controllers.get_mut(logical))
            {
                logical.connected = true;
                logical.metadata = metadata;
            }
            return;
        }
        let route = ControllerRoute {
            sources: vec![id.clone()],
            composition: CompositionMode::Single,
        };
        self.routes.insert(id.clone(), route.clone());
        self.source_controllers.insert(id.clone(), id.clone());
        self.controllers.insert(
            id.clone(),
            LogicalController {
                id,
                name,
                source,
                connected: true,
                metadata,
                route,
            },
        );
    }

    fn join_edge(&mut self, source: String, eligible: bool, limit: u8) {
        if !eligible {
            self.neutral.insert(source);
        } else if self.neutral.remove(&source) {
            self.assign_if_open(&source, limit);
        }
    }

    fn assign_if_open(&mut self, source: &str, limit: u8) {
        let logical = self
            .source_controllers
            .get(source)
            .cloned()
            .unwrap_or_else(|| source.to_owned());
        if self.player_for(&logical).is_some() {
            return;
        }
        if let Some(index) = self.next_open(limit) {
            self.players.insert(index, Some(logical));
        }
    }

    fn next_open(&self, limit: u8) -> Option<u8> {
        (0..limit).find(|index| self.players.get(index).is_none_or(Option::is_none))
    }

    fn player_for(&self, id: &str) -> Option<u8> {
        self.players
            .iter()
            .find_map(|(index, assigned)| (assigned.as_deref() == Some(id)).then_some(*index))
    }

    fn visible_player_count(&self) -> u8 {
        if self.players.keys().any(|index| *index >= BASE_PLAYERS) {
            SESSION_PLAYERS
        } else {
            BASE_PLAYERS
        }
    }

    fn source_assignments(&self) -> Vec<SourceAssignment> {
        self.players
            .iter()
            .filter_map(|(index, controller)| {
                controller.as_ref().map(|id| SourceAssignment {
                    index: *index,
                    source: SourceSelector::Device {
                        device_id: id.clone(),
                    },
                })
            })
            .collect()
    }

    fn player_sources(&self) -> Vec<(String, u8)> {
        self.players
            .iter()
            .filter_map(|(index, controller)| {
                controller
                    .as_ref()
                    .and_then(|id| self.controllers.get(id))
                    .map(|controller| {
                        controller
                            .route
                            .sources
                            .iter()
                            .cloned()
                            .map(|source| (source, *index))
                            .collect::<Vec<_>>()
                    })
            })
            .flatten()
            .collect()
    }

    fn assignment_signature(&self) -> Vec<(u8, String)> {
        self.players
            .iter()
            .filter_map(|(index, value)| value.clone().map(|value| (*index, value)))
            .collect()
    }

    fn device_json(&self, controller: &LogicalController) -> serde_json::Value {
        serde_json::json!({
            "id": controller.id,
            "source": controller.source,
            "name": controller.name,
            "connected": controller.connected,
            "playerIndex": self.player_for(&controller.id),
            "metadata": controller.metadata,
        })
    }

    fn pair_id(left: &str, right: &str) -> String {
        let mut parts = [
            left.rsplit('/').next().unwrap_or(left),
            right.rsplit('/').next().unwrap_or(right),
        ];
        parts.sort_unstable();
        format!("bt://composite/{}+{}", parts[0], parts[1])
    }

    fn require_session(&self, command: &serde_json::Value) -> Result<(), String> {
        let requested = command
            .get("sessionId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "controller session command requires sessionId".to_owned())?;
        let active = self
            .session
            .as_ref()
            .ok_or_else(|| "no controller connection session is active".to_owned())?;
        if requested == active.id {
            Ok(())
        } else {
            Err("controller connection session is no longer active".to_owned())
        }
    }

    fn id_field(command: &serde_json::Value) -> Result<String, String> {
        command
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "controller command requires id".to_owned())
    }
}
