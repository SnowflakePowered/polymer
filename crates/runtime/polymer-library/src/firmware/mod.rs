//! User-supplied hardware data, independent of NANDs and game packages.

mod bios;
mod install;
mod read;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    pub label: String,
    pub names: Vec<String>,
    pub byte_length: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FirmwareLookup {
    Name(String),
    Md5(String),
    Sha1(String),
    Sha256(String),
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
                label TEXT NOT NULL,
                digest TEXT NOT NULL,
                byte_length INTEGER NOT NULL CHECK(byte_length >= 0),
                PRIMARY KEY(platform, label)
            ) STRICT;
            CREATE TABLE IF NOT EXISTS firmware_names (
                platform TEXT NOT NULL,
                label TEXT NOT NULL,
                name TEXT NOT NULL,
                position INTEGER NOT NULL,
                PRIMARY KEY(platform, label, name),
                FOREIGN KEY(platform, label) REFERENCES firmware(platform, label) ON DELETE CASCADE
            ) STRICT;
            CREATE INDEX IF NOT EXISTS firmware_name_lookup ON firmware_names(platform, name);
            CREATE TABLE IF NOT EXISTS firmware_hashes (
                platform TEXT NOT NULL,
                label TEXT NOT NULL,
                algorithm TEXT NOT NULL CHECK(algorithm IN ('md5', 'sha1', 'sha256')),
                hash TEXT NOT NULL,
                PRIMARY KEY(platform, label, algorithm),
                FOREIGN KEY(platform, label) REFERENCES firmware(platform, label) ON DELETE CASCADE
            ) STRICT;
            CREATE INDEX IF NOT EXISTS firmware_hash_lookup ON firmware_hashes(platform, algorithm, hash);",
            )
            .await?;
        Ok(Self { root, database })
    }

    // Restrict stored platform names to known machines and safe path components.
    fn valid_platform(platform: &str) -> bool {
        consolespec::MachineSpec::try_from(platform).is_ok()
            && platform
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
    }
}
