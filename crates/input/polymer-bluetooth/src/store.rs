//! Versioned persistence for bonds and reconnection metadata.

use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use atomic_write_file::AtomicWriteFile;
use rusqlite::{Connection, params};

use crate::transport::bredr::{LinkKey, LinkKeyStore};
use crate::{
    DeviceAddr, Error, Result, TransportKind,
    devices::{ProfileId, VendorProductId},
};

const STORE_VERSION: u32 = 1;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct ControllerRecord {
    pub address: DeviceAddr,
    pub transport: TransportKind,
    pub profile_id: ProfileId,
    pub vendor_product: Option<VendorProductId>,
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

pub struct SqliteControllerStore {
    connection: Mutex<Connection>,
}

pub struct ControllerLinkKeyStore {
    store: Arc<dyn ControllerStore>,
    updates: Mutex<()>,
}

impl ControllerLinkKeyStore {
    pub fn new(store: Arc<dyn ControllerStore>) -> Self {
        Self {
            store,
            updates: Mutex::new(()),
        }
    }

    pub fn register(&self, mut record: ControllerRecord) -> Result<()> {
        let _update = self.updates.lock().map_err(|_| Error::ChannelClosed)?;
        if let Some(existing) = self.store.records()?.into_iter().find(|existing| {
            existing.address == record.address && existing.transport == record.transport
        }) {
            record.link_key = existing.link_key;
            record.link_key_type = existing.link_key_type;
            record.vendor_product = record.vendor_product.or(existing.vendor_product);
        }
        self.store.put(record)
    }
}

impl LinkKeyStore for ControllerLinkKeyStore {
    fn get(&self, addr: DeviceAddr) -> Result<Option<LinkKey>> {
        let _update = self.updates.lock().map_err(|_| Error::ChannelClosed)?;
        Ok(self
            .store
            .records()?
            .into_iter()
            .find(|record| record.address == addr && record.transport == TransportKind::BrEdr)
            .and_then(|record| {
                Some(LinkKey {
                    value: record.link_key?,
                    key_type: record.link_key_type?,
                })
            }))
    }
    fn put(&self, addr: DeviceAddr, key: LinkKey) -> Result<()> {
        let _update = self.updates.lock().map_err(|_| Error::ChannelClosed)?;
        let mut record = self
            .store
            .records()?
            .into_iter()
            .find(|record| record.address == addr && record.transport == TransportKind::BrEdr)
            .unwrap_or(ControllerRecord {
                address: addr,
                transport: TransportKind::BrEdr,
                profile_id: ProfileId::new("unknown.bredr"),
                vendor_product: None,
                link_key: None,
                link_key_type: None,
                adapter_index: None,
                auto_reconnect: false,
                player_index: 0,
            });
        record.link_key = Some(key.value);
        record.link_key_type = Some(key.key_type);
        self.store.put(record)
    }
    fn remove(&self, addr: DeviceAddr) -> Result<()> {
        let _update = self.updates.lock().map_err(|_| Error::ChannelClosed)?;
        let Some(mut record) = self
            .store
            .records()?
            .into_iter()
            .find(|record| record.address == addr && record.transport == TransportKind::BrEdr)
        else {
            return Ok(());
        };
        record.link_key = None;
        record.link_key_type = None;
        self.store.put(record)
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

impl SqliteControllerStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path).map_err(Self::database_error)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(Self::database_error)?;
        connection
            .execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS bluetooth_controllers (
                    address        BLOB NOT NULL CHECK (length(address) = 6),
                    address_kind   TEXT NOT NULL CHECK (address_kind IN ('public', 'random')),
                    transport      TEXT NOT NULL CHECK (transport IN ('ble', 'br-edr')),
                    profile_id     TEXT NOT NULL,
                    link_key       BLOB CHECK (link_key IS NULL OR length(link_key) = 16),
                    link_key_type  INTEGER,
                    adapter_index  INTEGER CHECK (adapter_index IS NULL OR adapter_index >= 0),
                    auto_reconnect INTEGER NOT NULL CHECK (auto_reconnect IN (0, 1)),
                    player_index   INTEGER NOT NULL CHECK (player_index BETWEEN 0 AND 255),
                    vendor_id      INTEGER CHECK (vendor_id IS NULL OR vendor_id BETWEEN 0 AND 65535),
                    product_id     INTEGER CHECK (product_id IS NULL OR product_id BETWEEN 0 AND 65535),
                    CHECK ((vendor_id IS NULL) = (product_id IS NULL)),
                    PRIMARY KEY (address, address_kind, transport)
                ) STRICT;
                "#,
            )
            .map_err(Self::database_error)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn database_error(error: rusqlite::Error) -> Error {
        Error::Store(format!("input database error: {error}"))
    }

    const fn transport_name(transport: TransportKind) -> &'static str {
        match transport {
            TransportKind::Ble => "ble",
            TransportKind::BrEdr => "br-edr",
        }
    }

    fn transport(value: &str) -> Result<TransportKind> {
        match value {
            "ble" => Ok(TransportKind::Ble),
            "br-edr" => Ok(TransportKind::BrEdr),
            _ => Err(Error::Store(format!(
                "unknown Bluetooth transport '{value}' in input database"
            ))),
        }
    }

    const fn address_kind_name(kind: crate::AddressKind) -> &'static str {
        match kind {
            crate::AddressKind::Public => "public",
            crate::AddressKind::Random => "random",
        }
    }

    fn address_kind(value: &str) -> Result<crate::AddressKind> {
        match value {
            "public" => Ok(crate::AddressKind::Public),
            "random" => Ok(crate::AddressKind::Random),
            _ => Err(Error::Store(format!(
                "unknown Bluetooth address kind '{value}' in input database"
            ))),
        }
    }

    fn record(row: &rusqlite::Row<'_>) -> Result<ControllerRecord> {
        let address = row.get::<_, Vec<u8>>(0).map_err(Self::database_error)?;
        let address: [u8; 6] = address.try_into().map_err(|address: Vec<u8>| {
            Error::Store(format!(
                "Bluetooth address in input database has {} bytes",
                address.len()
            ))
        })?;
        let kind = Self::address_kind(&row.get::<_, String>(1).map_err(Self::database_error)?)?;
        let transport = Self::transport(&row.get::<_, String>(2).map_err(Self::database_error)?)?;
        let link_key = row
            .get::<_, Option<Vec<u8>>>(4)
            .map_err(Self::database_error)?
            .map(|key| {
                key.try_into().map_err(|key: Vec<u8>| {
                    Error::Store(format!(
                        "Bluetooth link key in input database has {} bytes",
                        key.len()
                    ))
                })
            })
            .transpose()?;
        let adapter_index = row
            .get::<_, Option<i64>>(6)
            .map_err(Self::database_error)?
            .map(|index| {
                usize::try_from(index)
                    .map_err(|_| Error::Store("Bluetooth adapter index is out of range".to_owned()))
            })
            .transpose()?;
        let player_index = u8::try_from(row.get::<_, i64>(8).map_err(Self::database_error)?)
            .map_err(|_| Error::Store("Bluetooth player index is out of range".to_owned()))?;
        let vendor_id = row.get::<_, Option<u16>>(9).map_err(Self::database_error)?;
        let product_id = row
            .get::<_, Option<u16>>(10)
            .map_err(Self::database_error)?;
        Ok(ControllerRecord {
            address: DeviceAddr::new(address, kind),
            transport,
            profile_id: ProfileId::new(row.get::<_, String>(3).map_err(Self::database_error)?),
            vendor_product: vendor_id
                .zip(product_id)
                .map(|(vendor_id, product_id)| VendorProductId::new(vendor_id, product_id)),
            link_key,
            link_key_type: row.get::<_, Option<u8>>(5).map_err(Self::database_error)?,
            adapter_index,
            auto_reconnect: row.get::<_, bool>(7).map_err(Self::database_error)?,
            player_index,
        })
    }
}

impl ControllerStore for SqliteControllerStore {
    fn records(&self) -> Result<Vec<ControllerRecord>> {
        let connection = self.connection.lock().map_err(|_| Error::ChannelClosed)?;
        let mut statement = connection
            .prepare(
                r#"
                SELECT address, address_kind, transport, profile_id, link_key,
                       link_key_type, adapter_index, auto_reconnect, player_index,
                       vendor_id, product_id
                FROM bluetooth_controllers
                ORDER BY transport, address
                "#,
            )
            .map_err(Self::database_error)?;
        let mut rows = statement.query(()).map_err(Self::database_error)?;
        let mut records = Vec::new();
        while let Some(row) = rows.next().map_err(Self::database_error)? {
            records.push(Self::record(row)?);
        }
        Ok(records)
    }

    fn put(&self, record: ControllerRecord) -> Result<()> {
        let adapter_index = record
            .adapter_index
            .map(i64::try_from)
            .transpose()
            .map_err(|_| Error::Store("Bluetooth adapter index is out of range".to_owned()))?;
        let connection = self.connection.lock().map_err(|_| Error::ChannelClosed)?;
        connection
            .execute(
                r#"
                INSERT INTO bluetooth_controllers (
                    address, address_kind, transport, profile_id, link_key,
                    link_key_type, adapter_index, auto_reconnect, player_index,
                    vendor_id, product_id
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT (address, address_kind, transport) DO UPDATE SET
                    profile_id = excluded.profile_id,
                    link_key = excluded.link_key,
                    link_key_type = excluded.link_key_type,
                    adapter_index = excluded.adapter_index,
                    auto_reconnect = excluded.auto_reconnect,
                    player_index = excluded.player_index,
                    vendor_id = excluded.vendor_id,
                    product_id = excluded.product_id
                "#,
                params![
                    record.address.bytes().as_slice(),
                    Self::address_kind_name(record.address.kind()),
                    Self::transport_name(record.transport),
                    record.profile_id.as_str(),
                    record.link_key.as_ref().map(<[u8; 16]>::as_slice),
                    record.link_key_type,
                    adapter_index,
                    record.auto_reconnect,
                    record.player_index,
                    record.vendor_product.map(|value| value.vendor_id),
                    record.vendor_product.map(|value| value.product_id),
                ],
            )
            .map_err(Self::database_error)?;
        Ok(())
    }

    fn remove(&self, address: DeviceAddr) -> Result<()> {
        let connection = self.connection.lock().map_err(|_| Error::ChannelClosed)?;
        connection
            .execute(
                "DELETE FROM bluetooth_controllers WHERE address = ?1 AND address_kind = ?2",
                params![
                    address.bytes().as_slice(),
                    Self::address_kind_name(address.kind())
                ],
            )
            .map_err(Self::database_error)?;
        Ok(())
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
        let mut updated = records.clone();
        updated.insert(key(record.address, record.transport), record);
        self.save(&updated)?;
        *records = updated;
        Ok(())
    }
    fn remove(&self, address: DeviceAddr) -> Result<()> {
        let mut records = self.records.lock().map_err(|_| Error::ChannelClosed)?;
        let mut updated = records.clone();
        updated.retain(|_, record| record.address != address);
        self.save(&updated)?;
        *records = updated;
        Ok(())
    }
}

fn key(address: DeviceAddr, transport: TransportKind) -> String {
    format!("{transport:?}:{address}")
}
