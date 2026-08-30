//! Stable physical-input identities shared by providers and API consumers.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InputSourceKind {
    Sdl,
    Bt,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputSourceDescriptor {
    pub id: String,
    pub source: InputSourceKind,
    pub name: String,
    pub connected: bool,
    pub metadata: serde_json::Value,
}

/// Process-lifetime catalog of real input devices.
///
/// Providers replace their connected view without deleting prior entries so a
/// configured emulator can still identify a temporarily disconnected device.
#[derive(Clone, Default)]
pub struct InputSourceCatalog(Arc<RwLock<BTreeMap<String, InputSourceDescriptor>>>);

impl InputSourceCatalog {
    pub fn upsert(&self, descriptor: InputSourceDescriptor) {
        self.0.write().insert(descriptor.id.clone(), descriptor);
    }

    pub fn replace_connected(
        &self,
        source: InputSourceKind,
        descriptors: impl IntoIterator<Item = InputSourceDescriptor>,
    ) {
        let mut entries = self.0.write();
        for descriptor in entries.values_mut().filter(|entry| entry.source == source) {
            descriptor.connected = false;
        }
        for mut descriptor in descriptors {
            debug_assert_eq!(descriptor.source, source);
            descriptor.connected = true;
            entries.insert(descriptor.id.clone(), descriptor);
        }
    }

    pub fn get(&self, id: &str) -> Option<InputSourceDescriptor> {
        self.0.read().get(id).cloned()
    }

    pub fn snapshot(&self) -> Vec<InputSourceDescriptor> {
        self.0.read().values().cloned().collect()
    }
}
