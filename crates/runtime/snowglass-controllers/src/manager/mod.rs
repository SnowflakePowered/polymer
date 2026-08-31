mod command;
mod profile;

use crate::{ControllerDevice, InputConfiguration, SourceAssignment};
use parking_lot::RwLock;
use profile::MappingProfile;
use snowglass_input::{DeviceDescriptor, DeviceId, DeviceRole, GamepadButton, InputHub};
use snowglass_input_layout::layout::ProjectionMode;
use snowglass_library::InputStore;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::Arc,
};

#[derive(Clone, Debug, Default)]
pub struct ControllerAssignmentSnapshot {
    assignments: BTreeMap<usize, crate::SourceSelector>,
    has_connected_controllers: bool,
}

impl ControllerAssignmentSnapshot {
    pub fn from_manager(manager: &ControllerManager) -> Self {
        let assignments = manager
            .hub
            .as_ref()
            .map(|hub| {
                hub.assignments()
                    .into_iter()
                    .map(|(index, device)| {
                        (
                            usize::from(index.get()),
                            crate::SourceSelector::Device {
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

    pub fn source_for(&self, slot: usize) -> Option<crate::SourceSelector> {
        self.assignments.get(&slot).cloned().or_else(|| {
            (slot == 0 && !self.has_connected_controllers)
                .then_some(crate::SourceSelector::KeyboardMouse)
        })
    }
}

#[derive(Clone, Debug)]
pub enum ControllerManagerEffect {
    SessionDiscovery(bool),
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
    hub: Option<InputHub>,
    session: Option<String>,
    join_neutral: HashSet<DeviceId>,
    mappings: ControllerProfiles,
    mapping_store: Option<InputStore>,
}

impl ControllerManager {
    pub fn with_profiles(mappings: ControllerProfiles) -> Self {
        Self {
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
            "activeConnectionSession": self.session.as_ref().map(|id| serde_json::json!({
                "id": id,
                "active": true,
                "discovery": { "active": false },
                "candidates": [],
            })),
            "layouts": snowglass_input_layout::layout::InputLayout::all(),
        })
    }

    pub fn sync_configuration(&self, configuration: &InputConfiguration) -> Result<(), String> {
        let Some(hub) = &self.hub else {
            return Ok(());
        };
        let devices = hub.devices();
        let configured = configuration
            .assignments
            .iter()
            .filter_map(|assignment| match &assignment.source {
                crate::SourceSelector::Device { device_id } => Some(device_id.as_str()),
                crate::SourceSelector::KeyboardMouse => None,
            })
            .collect::<HashSet<_>>();
        for (_, device) in hub.assignments() {
            if !configured.contains(device.as_str()) {
                hub.assign(&device, None)
                    .map_err(|error| error.to_string())?;
            }
        }
        for assignment in &configuration.assignments {
            let crate::SourceSelector::Device { device_id } = &assignment.source else {
                continue;
            };
            let device = devices
                .iter()
                .find(|(_, descriptor)| descriptor.id.as_str() == device_id)
                .map(|(_, descriptor)| descriptor.id.clone())
                .ok_or_else(|| format!("unknown input device '{device_id}'"))?;
            hub.assign(
                &device,
                Some(snowglass_input::PlayerIndex::new(assignment.index)),
            )
            .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn observe_session(&mut self) -> Option<Vec<SourceAssignment>> {
        self.session.as_ref()?;
        let hub = self.hub.as_ref()?;
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
                .map(snowglass_input::PlayerIndex::new)
                .find(|index| hub.assignments().iter().all(|(used, _)| used != index))
            else {
                break;
            };
            if hub.assign(&descriptor.id, Some(index)).is_ok() {
                changed = true;
            }
        }
        changed.then(|| self.source_assignments())
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
                        source: crate::SourceSelector::Device {
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

#[cfg(test)]
mod tests {
    use super::*;
    use snowglass_input::{
        BackendError, BackendHandle, BackendId, Calibration, Capabilities, DeviceState, Feedback,
        InputBackend,
    };
    use std::sync::Mutex;

    #[derive(Default)]
    struct TestBackend {
        handle: Mutex<Option<BackendHandle>>,
    }

    impl TestBackend {
        fn connect(&self, descriptor: DeviceDescriptor) {
            self.handle
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .connected(descriptor);
        }

        fn publish(&self, device: &DeviceId, state: DeviceState) {
            self.handle
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .publish(device, &state);
        }
    }

    impl InputBackend for TestBackend {
        fn id(&self) -> BackendId {
            BackendId::SDL3
        }

        fn start(&self, handle: BackendHandle) -> Result<(), BackendError> {
            *self.handle.lock().unwrap() = Some(handle);
            Ok(())
        }

        fn shutdown(&self) {}

        fn apply_feedback(
            &self,
            _device: &DeviceId,
            _feedback: &Feedback,
        ) -> Result<(), BackendError> {
            Err(BackendError::Unsupported)
        }

        fn set_calibration(
            &self,
            _device: &DeviceId,
            _calibration: &Calibration,
        ) -> Result<(), BackendError> {
            Ok(())
        }
    }

    #[test]
    fn a_new_session_clears_assignments_and_requires_a_new_join_edge() {
        let hub = InputHub::default();
        let backend = Arc::new(TestBackend::default());
        hub.register_backend(backend.clone()).unwrap();
        let device = DeviceId::new(BackendId::SDL3, "test-pad");
        backend.connect(DeviceDescriptor {
            id: device.clone(),
            name: Arc::from("Test Pad"),
            profile: None,
            vendor_id: None,
            product_id: None,
            serial: None,
            role: DeviceRole::Whole,
            capabilities: Capabilities::default(),
        });
        backend.publish(
            &device,
            DeviceState {
                gamepad: snowglass_input::GamepadReport {
                    buttons: GamepadButton::A.mask(),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert_eq!(hub.assignments().len(), 1);

        let mut manager = ControllerManager {
            hub: Some(hub.clone()),
            session: Some("session".to_owned()),
            join_neutral: HashSet::new(),
            mappings: ControllerProfiles::default(),
            mapping_store: None,
        };
        manager.clear_assignments().unwrap();
        assert!(hub.assignments().is_empty());
        assert!(manager.observe_session().is_none());

        backend.publish(&device, DeviceState::default());
        assert!(manager.observe_session().is_none());
        backend.publish(
            &device,
            DeviceState {
                gamepad: snowglass_input::GamepadReport {
                    buttons: GamepadButton::A.mask(),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let assignments = manager.observe_session().unwrap();
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].index, 0);
        assert_eq!(hub.assignments()[0].1, device);
    }
}
