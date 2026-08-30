mod command;
mod profile;

use crate::{ControllerDevice, InputConfiguration, SourceAssignment};
use parking_lot::RwLock;
use profile::MappingProfile;
use snowglass_input::{DeviceDescriptor, InputHub};
use snowglass_input_layout::layout::ProjectionMode;
use snowglass_library::InputStore;
use std::{
    collections::{BTreeMap, HashMap},
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
        let has_connected_controllers = manager
            .hub
            .as_ref()
            .is_some_and(|hub| !hub.devices().is_empty());
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
    mappings: ControllerProfiles,
    mapping_store: Option<InputStore>,
}

impl ControllerManager {
    pub fn with_profiles(mappings: ControllerProfiles) -> Self {
        Self {
            hub: None,
            session: None,
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
            .map(InputHub::snapshot)
            .unwrap_or_default()
            .into_iter()
            .map(|(index, descriptor, state)| {
                let device = ControllerDevice::from_descriptor(&descriptor);
                serde_json::json!({
                    "index": index.map(|value| value.get()),
                    "id": device.id,
                    "name": device.name,
                    "profile": device.profile,
                    "vendorId": device.vendor_id,
                    "productId": device.product_id,
                    "role": device.role,
                    "capabilities": device.capabilities,
                    "connected": true,
                    "battery": {
                        "level": state.battery.level,
                        "charging": state.battery.charging,
                    },
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "devices": devices,
            "activeConnectionSession": self.session.as_ref().map(|id| serde_json::json!({"id": id, "active": true})),
            "layouts": snowglass_input_layout::layout::InputLayout::all(),
        })
    }

    pub fn sync_configuration(&self, configuration: &InputConfiguration) -> Result<(), String> {
        let Some(hub) = &self.hub else {
            return Ok(());
        };
        let devices = hub.devices();
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

    fn descriptor(&self, id: &str) -> Option<DeviceDescriptor> {
        self.hub
            .as_ref()?
            .devices()
            .into_iter()
            .find(|(_, descriptor)| descriptor.id.as_str() == id)
            .map(|(_, descriptor)| descriptor)
    }
}
