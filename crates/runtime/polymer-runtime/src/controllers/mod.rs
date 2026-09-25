//! Host controller assignments, connection sessions, and persistent mapping profiles.

mod command;
mod profile;
mod transaction;
pub use transaction::ControllerTopologyTransaction;

use parking_lot::RwLock;
use polymer_config::input::{
    InputConfiguration, InputOperation, InputTransaction, ProjectionMode, SourceAssignment,
    SourceSelector,
};
use polymer_controllers::{ControllerConfiguration, ControllerDevice, ControllerTopology};
use polymer_input::{DeviceDescriptor, DeviceId, DeviceRole, GamepadButton, InputHub};
use polymer_library::InputStore;
use profile::MappingProfile;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::Arc,
};

#[derive(Clone, Debug, Default)]
pub struct ControllerAssignmentSnapshot {
    assignments: BTreeMap<usize, SourceSelector>,
    has_connected_controllers: bool,
}

impl ControllerAssignmentSnapshot {
    pub(crate) fn from_manager(manager: &ControllerManager) -> Self {
        let assignments = manager
            .hub
            .as_ref()
            .map(|hub| {
                hub.assignments()
                    .into_iter()
                    .map(|(index, device)| {
                        (
                            usize::from(index.get()),
                            SourceSelector::Device {
                                device_id: device.to_string(),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let has_connected_controllers = manager.hub.as_ref().is_some_and(|hub| {
            hub.device_snapshots()
                .into_iter()
                .any(|(_, _, _, connected)| connected)
        });
        Self {
            assignments,
            has_connected_controllers,
        }
    }

    pub fn source_for(&self, slot: usize) -> Option<SourceSelector> {
        self.assignments.get(&slot).cloned().or_else(|| {
            (slot == 0 && !self.has_connected_controllers).then_some(SourceSelector::KeyboardMouse)
        })
    }
}

#[derive(Clone, Debug)]
pub enum ControllerManagerEffect {
    SessionDiscovery(bool),
    DisconnectSources(Vec<String>),
    Connect(String),
    Forget(String),
}

pub struct ControllerCommandOutcome {
    pub response: serde_json::Value,
    pub session_commit: Option<ControllerSessionCommit>,
    pub reapply_topology: bool,
    pub effects: Vec<ControllerManagerEffect>,
}

#[derive(Debug)]
pub struct ControllerSessionCommit {
    pub session_id: String,
    pub assignments: Vec<SourceAssignment>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceMapping {
    pub mode: ProjectionMode,
    #[serde(default)]
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
    configuration: ControllerConfiguration,
    applied: Option<ControllerTopology>,
    hub: Option<InputHub>,
    session: Option<ControllerConnectionSession>,
    join_neutral: HashSet<DeviceId>,
    mappings: ControllerProfiles,
    mapping_store: Option<InputStore>,
}

struct ControllerConnectionSession {
    id: String,
    committing: bool,
    assignments: Vec<SourceAssignment>,
}

impl ControllerManager {
    pub fn with_profiles(mappings: ControllerProfiles) -> Self {
        Self {
            configuration: ControllerConfiguration::new(None, [])
                .expect("an empty controller configuration is valid"),
            applied: None,
            hub: None,
            session: None,
            join_neutral: HashSet::new(),
            mappings,
            mapping_store: None,
        }
    }

    pub async fn open(
        mapping_path: impl AsRef<Path>,
        mappings: ControllerProfiles,
        hub: InputHub,
    ) -> Result<Self, String> {
        let store = InputStore::open(mapping_path)
            .await
            .map_err(|error| error.to_string())?;
        let profiles = store.profiles().await.map_err(|error| error.to_string())?;
        mappings.clear();
        for profile in profiles {
            let identity = profile.identity.clone();
            let layout = profile.layout_id.clone();
            mappings.put(identity, layout, MappingProfile::from_stored(profile));
        }
        Ok(Self {
            configuration: ControllerConfiguration::new(None, [])
                .expect("an empty controller configuration is valid"),
            applied: None,
            hub: Some(hub),
            session: None,
            join_neutral: HashSet::new(),
            mappings,
            mapping_store: Some(store),
        })
    }

    pub fn hub(&self) -> Option<&InputHub> {
        self.hub.as_ref()
    }

    pub fn configuration(&self) -> InputConfiguration {
        self.configuration.snapshot()
    }

    pub fn applied(&self) -> Option<&ControllerTopology> {
        self.applied.as_ref()
    }

    pub fn initialize(&mut self, topology: ControllerTopology) -> Result<(), String> {
        topology.validate()?;
        self.applied = Some(topology);
        Ok(())
    }

    pub fn clear_applied(&mut self) {
        self.applied = None;
    }

    pub fn apply_configuration(
        &self,
        transaction: InputTransaction,
    ) -> Result<InputConfiguration, String> {
        let configuration = self
            .configuration
            .apply(transaction)
            .map_err(|error| error.to_string())?;
        self.sync_configuration(&configuration)?;
        Ok(configuration)
    }

    pub fn transaction(
        &self,
        input: InputTransaction,
    ) -> Result<ControllerTopologyTransaction, String> {
        let previous_configuration = self.configuration();
        let candidate_configuration = self
            .configuration
            .preview(&input)
            .map_err(|error| error.to_string())?;
        let previous_topology = match &self.applied {
            Some(topology) => topology.clone(),
            None => self.topology_for(&previous_configuration)?,
        };
        let requested_topology = self.topology_for(&candidate_configuration)?;
        Ok(ControllerTopologyTransaction::new(
            input,
            previous_configuration,
            candidate_configuration,
            previous_topology,
            requested_topology,
        ))
    }

    pub fn connection_transaction(
        &self,
        commit: &ControllerSessionCommit,
    ) -> Result<ControllerTopologyTransaction, String> {
        self.transaction(InputTransaction {
            expected_revision: self.configuration().revision,
            operations: vec![InputOperation::ReplaceAssignments {
                assignments: commit.assignments.clone(),
            }],
        })
    }

    pub fn commit(
        &mut self,
        transaction: ControllerTopologyTransaction,
        applied: ControllerTopology,
    ) -> Result<InputConfiguration, String> {
        applied.validate()?;
        let configuration = self
            .configuration
            .apply(transaction.input)
            .map_err(|error| error.to_string())?;
        self.applied = Some(applied);
        Ok(configuration)
    }

    pub fn assignment_snapshot(&self) -> ControllerAssignmentSnapshot {
        ControllerAssignmentSnapshot::from_manager(self)
    }

    pub fn settle_connection_session(
        &mut self,
        session_id: &str,
        committed: bool,
    ) -> Result<ControllerManagerEffect, String> {
        let session = self
            .session
            .as_mut()
            .filter(|session| session.id == session_id)
            .ok_or_else(|| "unknown controller connection session".to_owned())?;
        if !session.committing {
            return Err("controller connection session is not committing".to_owned());
        }
        if committed {
            self.session = None;
            self.finish_assignment_session();
            Ok(ControllerManagerEffect::SessionDiscovery(false))
        } else {
            let assignments = session.assignments.clone();
            session.committing = false;
            self.sync_assignments(&assignments)?;
            Ok(ControllerManagerEffect::SessionDiscovery(true))
        }
    }

    pub fn snapshot(&self) -> serde_json::Value {
        let devices = self
            .hub
            .as_ref()
            .map(InputHub::device_snapshots)
            .unwrap_or_default()
            .into_iter()
            .map(|(index, descriptor, state, connected)| {
                let device = ControllerDevice::from_descriptor(&descriptor);
                serde_json::json!({
                    "index": index.map(|value| value.get()),
                    "id": device.id,
                    "name": device.name,
                    "profile": device.profile,
                    "vendorId": device.vendor_id,
                    "productId": device.product_id,
                    "serial": device.serial,
                    "role": device.role,
                    "capabilities": device.capabilities,
                    "connected": connected,
                    "pairing": false,
                    "battery": {
                        "level": state.battery.level,
                        "charging": state.battery.charging,
                    },
                })
            })
            .collect::<Vec<_>>();
        let player_count = if devices.iter().any(|device| {
            device["connected"].as_bool() == Some(true)
                && device["index"].as_u64().is_some_and(|index| index >= 4)
        }) {
            8
        } else {
            4
        };
        let players = (0..player_count)
            .map(|index| {
                let controller = devices
                    .iter()
                    .find(|device| device["index"].as_u64() == Some(index))
                    .cloned();
                serde_json::json!({ "index": index, "controller": controller })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "availability": { "osBle": "unknown", "winUsb": "unknown" },
            "devices": devices,
            "players": players,
            "activeConnectionSession": self.session.as_ref().map(|session| serde_json::json!({
                "id": session.id,
                "active": true,
                "committing": session.committing,
                "discovery": { "active": false },
                "candidates": [],
            })),
        })
    }

    pub fn sync_configuration(&self, configuration: &InputConfiguration) -> Result<(), String> {
        self.sync_assignments(&configuration.assignments)
    }

    fn sync_assignments(&self, assignments: &[SourceAssignment]) -> Result<(), String> {
        let Some(hub) = &self.hub else {
            return Ok(());
        };
        let devices = hub.devices();
        let configured = assignments
            .iter()
            .filter_map(|assignment| match &assignment.source {
                SourceSelector::Device { device_id } => Some(device_id.as_str()),
                SourceSelector::KeyboardMouse => None,
            })
            .collect::<HashSet<_>>();
        for (_, device) in hub.assignments() {
            if !configured.contains(device.as_str()) {
                hub.assign(&device, None)
                    .map_err(|error| error.to_string())?;
            }
        }
        for assignment in assignments {
            let SourceSelector::Device { device_id } = &assignment.source else {
                continue;
            };
            let device = devices
                .iter()
                .find(|(_, descriptor)| descriptor.id.as_str() == device_id)
                .map(|(_, descriptor)| descriptor.id.clone())
                .ok_or_else(|| format!("unknown input device '{device_id}'"))?;
            hub.assign(
                &device,
                Some(polymer_input::PlayerIndex::new(assignment.index)),
            )
            .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn observe_session(&mut self) -> bool {
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        let Some(hub) = self.hub.as_ref() else {
            return false;
        };
        if session.committing {
            return false;
        }
        let mut changed = false;
        for (index, descriptor, state, connected) in hub.device_snapshots() {
            if !connected || index.is_some() {
                continue;
            }
            let eligible = Self::join_eligible(&descriptor, state.gamepad.buttons);
            if !eligible {
                self.join_neutral.insert(descriptor.id);
                continue;
            }
            if !self.join_neutral.remove(&descriptor.id) {
                continue;
            }
            let Some(index) = (0..8)
                .map(polymer_input::PlayerIndex::new)
                .find(|index| hub.assignments().iter().all(|(used, _)| used != index))
            else {
                break;
            };
            if hub.assign(&descriptor.id, Some(index)).is_ok() {
                changed = true;
            }
        }
        changed
    }

    fn clear_assignments(&mut self) -> Result<(), String> {
        let Some(hub) = &self.hub else {
            return Ok(());
        };
        hub.set_auto_assign(false);
        for (_, device) in hub.assignments() {
            hub.assign(&device, None)
                .map_err(|error| error.to_string())?;
        }
        self.join_neutral = hub
            .device_snapshots()
            .into_iter()
            .filter(|(_, descriptor, state, connected)| {
                *connected && !Self::join_eligible(descriptor, state.gamepad.buttons)
            })
            .map(|(_, descriptor, _, _)| descriptor.id)
            .collect();
        Ok(())
    }

    fn finish_assignment_session(&mut self) {
        self.join_neutral.clear();
        if let Some(hub) = &self.hub {
            hub.set_auto_assign(true);
        }
    }

    fn source_assignments(&self) -> Vec<SourceAssignment> {
        self.hub
            .as_ref()
            .map(|hub| {
                hub.assignments()
                    .into_iter()
                    .map(|(index, device)| SourceAssignment {
                        index: index.get(),
                        source: SourceSelector::Device {
                            device_id: device.to_string(),
                        },
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn join_eligible(descriptor: &DeviceDescriptor, buttons: u32) -> bool {
        let pressed = |button: GamepadButton| buttons & button.mask() != 0;
        let generic = [
            GamepadButton::A,
            GamepadButton::B,
            GamepadButton::X,
            GamepadButton::Y,
            GamepadButton::Start,
            GamepadButton::Guide,
        ]
        .into_iter()
        .any(pressed);
        let shoulders = pressed(GamepadButton::L) && pressed(GamepadButton::R);
        let sideways = [
            GamepadButton::LeftPaddle1,
            GamepadButton::RightPaddle1,
            GamepadButton::LeftPaddle2,
            GamepadButton::RightPaddle2,
        ]
        .into_iter()
        .filter(|button| pressed(*button))
        .count()
            >= 2;
        let switch_controller = descriptor
            .profile
            .as_deref()
            .is_some_and(|profile| profile.starts_with("nintendo.switch"));
        if switch_controller {
            sideways || descriptor.role == DeviceRole::Whole && shoulders
        } else {
            generic || shoulders
        }
    }

    fn descriptor(&self, id: &str) -> Option<DeviceDescriptor> {
        self.hub
            .as_ref()?
            .devices()
            .into_iter()
            .find(|(_, descriptor)| descriptor.id.as_str() == id)
            .map(|(_, descriptor)| descriptor)
    }
}
