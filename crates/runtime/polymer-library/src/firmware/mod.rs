//! User-supplied hardware data, independent of NANDs and game packages.

mod install;

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, thiserror::Error)]
pub enum FirmwareError {
    #[error("firmware I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("firmware database: {0}")]
    Database(#[from] turso::Error),
    #[error("invalid firmware: {0}")]
    Invalid(String),
    #[error("firmware not found: {0}")]
    NotFound(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareDescription {
    pub platform: String,
    #[serde(rename = "type")]
    pub firmware_type: String,
    pub label: String,
    pub byte_length: u64,
}

/// Resolved installation contents; omitted JS contents are bound to the input snapshot.
#[derive(Clone, Debug)]
pub struct FirmwareInstallDescriptor {
    pub platform: String,
    pub firmware_type: String,
    pub label: String,
    pub contents: Arc<[u8]>,
}

impl FirmwareInstallDescriptor {
    pub fn validate_batch(batch: &[Self], platforms: &[String]) -> Result<(), FirmwareError> {
        let mut slots = BTreeSet::new();
        for item in batch {
            if !platforms.contains(&item.platform) || !FirmwareStore::valid_platform(&item.platform)
            {
                return Err(FirmwareError::Invalid(format!(
                    "undeclared platform {}",
                    item.platform
                )));
            }
            if item.firmware_type.trim().is_empty() || item.label.trim().is_empty() {
                return Err(FirmwareError::Invalid(
                    "type and label must be nonempty".into(),
                ));
            }
            if !slots.insert((&item.platform, &item.firmware_type, &item.label)) {
                return Err(FirmwareError::Invalid(
                    "duplicate slot in firmware batch".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct FirmwareStore {
    root: PathBuf,
    database: turso::Database,
}

impl FirmwareStore {
    pub async fn open(root: impl AsRef<Path>) -> Result<Self, FirmwareError> {
        let root = root.as_ref().to_owned();
        tokio::fs::create_dir_all(&root).await?;
        let path = root.join("firmware.db");
        let database = turso::Builder::new_local(
            path.to_str()
                .ok_or_else(|| FirmwareError::Invalid("database path is not UTF-8".into()))?,
        )
        .build()
        .await?;
        database
            .connect()?
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS firmware (
                platform TEXT NOT NULL,
                type TEXT NOT NULL,
                label TEXT NOT NULL,
                digest TEXT NOT NULL,
                byte_length INTEGER NOT NULL CHECK(byte_length >= 0),
                PRIMARY KEY(platform, type, label)
            ) STRICT;",
            )
            .await?;
        Ok(Self { root, database })
    }

    // Platform names are validated against the plugin manifest by the host. Restrict
    // path components here too, including callers which bypass the plugin runtime.
    fn valid_platform(platform: &str) -> bool {
        consolespec::MachineSpec::try_from(platform).is_ok()
            && platform
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
    }

    pub async fn list(&self, platform: &str) -> Result<Vec<FirmwareDescription>, FirmwareError> {
        let connection = self.database.connect()?;
        let mut rows = connection.query("SELECT platform, type, label, byte_length FROM firmware WHERE platform = ? ORDER BY type, label", [platform]).await?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().await? {
            result.push(FirmwareDescription {
                platform: row.get(0)?,
                firmware_type: row.get(1)?,
                label: row.get(2)?,
                byte_length: row.get::<i64>(3)? as u64,
            });
        }
        Ok(result)
    }

    /// Resolve once: immutable object paths keep existing file resources stable on replacement.
    pub async fn file(
        &self,
        platform: &str,
        firmware_type: &str,
        label: &str,
    ) -> Result<(String, PathBuf), FirmwareError> {
        let connection = self.database.connect()?;
        let mut rows = connection
            .query(
                "SELECT label, digest FROM firmware WHERE platform = ? AND type = ? AND label = ?",
                turso::params![platform, firmware_type, label],
            )
            .await?;
        let row = rows.next().await?.ok_or_else(|| {
            FirmwareError::NotFound(format!("{platform}/{firmware_type}/{label}"))
        })?;
        let digest: String = row.get(1)?;
        if !Self::valid_platform(platform)
            || digest.len() != 64
            || !digest.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(FirmwareError::Invalid("invalid stored object path".into()));
        }
        Ok((
            row.get(0)?,
            self.root.join(platform).join("objects").join(digest),
        ))
    }
}
