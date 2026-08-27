//! Versioned persistence for bonds and reconnection metadata.

use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use atomic_write_file::AtomicWriteFile;

use crate::transport::bredr::{LinkKey, LinkKeyStore};
use crate::{DeviceAddr, Error, Result, TransportKind, devices::ProfileId};

const STORE_VERSION: u32 = 1;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct ControllerRecord {
    pub address: DeviceAddr,
    pub transport: TransportKind,
    pub profile_id: ProfileId,
    pub link_key: Option<[u8; 16]>,
    pub link_key_type: Option<u8>,
    pub adapter_index: Option<usize>,
    pub auto_reconnect: bool,
    pub player_index: u8,
}

pub trait ControllerStore: Send + Sync {
    fn records(&self) -> Result<Vec<ControllerRecord>>;
    fn put(&self, record: ControllerRecord) -> Result<()>;
    fn remove(&self, address: DeviceAddr) -> Result<()>;
}

#[derive(Default)]
pub struct MemoryControllerStore {
    records: Mutex<BTreeMap<String, ControllerRecord>>,
}

impl ControllerStore for MemoryControllerStore {
    fn records(&self) -> Result<Vec<ControllerRecord>> {
        Ok(self
            .records
            .lock()
            .map_err(|_| Error::ChannelClosed)?
            .values()
            .cloned()
            .collect())
    }
    fn put(&self, record: ControllerRecord) -> Result<()> {
        self.records
            .lock()
            .map_err(|_| Error::ChannelClosed)?
            .insert(key(record.address, record.transport), record);
        Ok(())
    }
    fn remove(&self, address: DeviceAddr) -> Result<()> {
        self.records
            .lock()
            .map_err(|_| Error::ChannelClosed)?
            .retain(|_, record| record.address != address);
        Ok(())
    }
}

#[derive(Default, serde::Deserialize, serde::Serialize)]
struct StoreFile {
    version: u32,
    controllers: Vec<ControllerRecord>,
}

pub struct FileControllerStore {
    path: PathBuf,
    records: Mutex<BTreeMap<String, ControllerRecord>>,
}

pub struct ControllerLinkKeyStore {
    store: Arc<dyn ControllerStore>,
}

impl ControllerLinkKeyStore {
    pub fn new(store: Arc<dyn ControllerStore>) -> Self {
        Self { store }
    }
}

impl LinkKeyStore for ControllerLinkKeyStore {
    fn get(&self, addr: DeviceAddr) -> Result<Option<LinkKey>> {
        Ok(self
            .store
            .records()?
            .into_iter()
            .find(|record| record.address == addr)
            .and_then(|record| {
                Some(LinkKey {
                    value: record.link_key?,
                    key_type: record.link_key_type?,
                })
            }))
    }
    fn put(&self, addr: DeviceAddr, key: LinkKey) -> Result<()> {
        let mut record = self
            .store
            .records()?
            .into_iter()
            .find(|record| record.address == addr)
            .unwrap_or(ControllerRecord {
                address: addr,
                transport: TransportKind::BrEdr,
                profile_id: ProfileId::new("unknown.bredr"),
                link_key: None,
                link_key_type: None,
                adapter_index: None,
                auto_reconnect: true,
                player_index: 0,
            });
        record.link_key = Some(key.value);
        record.link_key_type = Some(key.key_type);
        self.store.put(record)
    }
    fn remove(&self, addr: DeviceAddr) -> Result<()> {
        self.store.remove(addr)
    }
}

impl FileControllerStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let file = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<StoreFile>(&bytes).map_err(|error| {
                Error::Store(format!("could not parse {}: {error}", path.display()))
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => StoreFile {
                version: STORE_VERSION,
                controllers: Vec::new(),
            },
            Err(error) => return Err(error.into()),
        };
        if file.version != STORE_VERSION {
            return Err(Error::Store(format!(
                "unsupported controller store version {}",
                file.version
            )));
        }
        let records = file
            .controllers
            .into_iter()
            .map(|record| (key(record.address, record.transport), record))
            .collect();
        Ok(Self {
            path,
            records: Mutex::new(records),
        })
    }

    fn save(&self, records: &BTreeMap<String, ControllerRecord>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(&StoreFile {
            version: STORE_VERSION,
            controllers: records.values().cloned().collect(),
        })
        .map_err(|error| Error::Store(error.to_string()))?;
        let mut writer = AtomicWriteFile::open(&self.path)?;
        writer.write_all(&bytes)?;
        writer.commit()?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl ControllerStore for FileControllerStore {
    fn records(&self) -> Result<Vec<ControllerRecord>> {
        Ok(self
            .records
            .lock()
            .map_err(|_| Error::ChannelClosed)?
            .values()
            .cloned()
            .collect())
    }
    fn put(&self, record: ControllerRecord) -> Result<()> {
        let mut records = self.records.lock().map_err(|_| Error::ChannelClosed)?;
        records.insert(key(record.address, record.transport), record);
        self.save(&records)
    }
    fn remove(&self, address: DeviceAddr) -> Result<()> {
        let mut records = self.records.lock().map_err(|_| Error::ChannelClosed)?;
        records.retain(|_, record| record.address != address);
        self.save(&records)
    }
}

fn key(address: DeviceAddr, transport: TransportKind) -> String {
    format!("{transport:?}:{address}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trips_versioned_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("controllers.json");
        let store = FileControllerStore::open(&path).unwrap();
        store
            .put(ControllerRecord {
                address: "00:11:22:33:44:55".parse().unwrap(),
                transport: TransportKind::BrEdr,
                profile_id: ProfileId::new("test"),
                link_key: None,
                link_key_type: None,
                adapter_index: None,
                auto_reconnect: true,
                player_index: 0,
            })
            .unwrap();
        assert_eq!(
            FileControllerStore::open(path)
                .unwrap()
                .records()
                .unwrap()
                .len(),
            1
        );
    }
}
