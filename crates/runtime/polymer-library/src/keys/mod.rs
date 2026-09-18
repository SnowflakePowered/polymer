//! Platform-scoped persistence for opaque key records.

mod read;
mod write;

use polymer_consolestorage::keys::KeyError;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct KeyStore {
    root: PathBuf,
    database: turso::Database,
}

#[derive(Clone)]
pub struct PlatformKeys {
    store: KeyStore,
    platform: String,
}

impl KeyStore {
    pub async fn open(root: impl AsRef<Path>) -> Result<Self, KeyError> {
        let root = root.as_ref().to_owned();
        tokio::fs::create_dir_all(&root).await?;
        let path = root.join("keys.db");
        let database = turso::Builder::new_local(
            path.to_str()
                .ok_or_else(|| KeyError::Invalid("database path is not UTF-8".into()))?,
        )
        .build()
        .await
        .map_err(|error| KeyError::Database(error.to_string()))?;
        database
            .connect()
            .map_err(|error| KeyError::Database(error.to_string()))?
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS keys (
                    platform TEXT NOT NULL,
                    namespace TEXT NOT NULL CHECK(namespace IN ('system', 'content')),
                    id TEXT NOT NULL,
                    title_id TEXT,
                    digest TEXT NOT NULL,
                    byte_length INTEGER NOT NULL CHECK(byte_length >= 0),
                    PRIMARY KEY(platform, namespace, id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS keys_title_lookup ON keys(platform, title_id);",
            )
            .await
            .map_err(|error| KeyError::Database(error.to_string()))?;
        Ok(Self { root, database })
    }

    pub fn for_platform(&self, platform: &str) -> Result<PlatformKeys, KeyError> {
        if consolespec::MachineSpec::try_from(platform).is_err()
            || !platform
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(KeyError::Invalid("unknown platform".into()));
        }
        Ok(PlatformKeys {
            store: self.clone(),
            platform: platform.to_owned(),
        })
    }
}

impl PlatformKeys {
    pub fn platform(&self) -> &str {
        &self.platform
    }

    fn object_path(&self, digest: &str) -> Result<PathBuf, KeyError> {
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(KeyError::Invalid("invalid stored key digest".into()));
        }
        Ok(self
            .store
            .root
            .join(&self.platform)
            .join("objects")
            .join(digest))
    }
}
