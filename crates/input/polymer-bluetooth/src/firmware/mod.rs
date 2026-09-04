//! Firmware bootstrap support for dedicated userspace Bluetooth adapters.

mod realtek;

use std::sync::Arc;

use crate::Result;

pub use realtek::{EmbeddedRealtek, REALTEK_FAMILIES};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirmwareIdentity {
    pub vendor_id: u16,
    pub product_id: u16,
    pub hci_version: u8,
    pub hci_revision: u16,
    pub manufacturer: u16,
    pub lmp_subversion: u16,
}

#[derive(Clone, Debug)]
pub struct PreparedFirmware {
    pub loader: &'static str,
    pub family: &'static str,
    pub bytes: Vec<u8>,
}

pub trait FirmwareLoader: Send + Sync {
    fn name(&self) -> &'static str;
    fn recognizes(&self, identity: &FirmwareIdentity) -> bool;
    fn requires_security_key(&self, _identity: &FirmwareIdentity) -> bool {
        false
    }
    fn prepare(
        &self,
        identity: &FirmwareIdentity,
        rom_version: Option<u8>,
        security_key_id: Option<u8>,
    ) -> Result<Option<PreparedFirmware>>;
}

#[derive(Clone, Default)]
pub struct FirmwareRegistry {
    loaders: Vec<Arc<dyn FirmwareLoader>>,
}

impl FirmwareRegistry {
    pub fn embedded() -> Self {
        let mut registry = Self::default();
        registry.register(Arc::new(EmbeddedRealtek));
        registry
    }

    pub fn register(&mut self, loader: Arc<dyn FirmwareLoader>) {
        self.loaders
            .retain(|registered| registered.name() != loader.name());
        self.loaders.push(loader);
    }

    pub fn matching(&self, identity: &FirmwareIdentity) -> Option<Arc<dyn FirmwareLoader>> {
        self.loaders
            .iter()
            .find(|loader| loader.recognizes(identity))
            .cloned()
    }
}
