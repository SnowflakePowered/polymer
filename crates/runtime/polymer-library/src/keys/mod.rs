//! Platform-scoped persistence for opaque key records.

mod read;
mod write;

use polymer_consolestorage::keys::KeyError;
use std::path::Path;

#[derive(Clone)]
pub struct KeyStore {
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
                    byte_length INTEGER NOT NULL CHECK(byte_length >= 0),
                    bytes BLOB NOT NULL,
                    PRIMARY KEY(platform, namespace, id)
                ) STRICT;
                CREATE INDEX IF NOT EXISTS keys_title_lookup ON keys(platform, title_id);",
            )
            .await
            .map_err(|error| KeyError::Database(error.to_string()))?;
        Ok(Self { database })
    }

    pub async fn platforms(&self) -> Result<Vec<String>, KeyError> {
        let connection = self
            .database
            .connect()
            .map_err(|error| KeyError::Database(error.to_string()))?;
        let mut rows = connection
            .query("SELECT DISTINCT platform FROM keys ORDER BY platform", ())
            .await
            .map_err(|error| KeyError::Database(error.to_string()))?;
        let mut platforms = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| KeyError::Database(error.to_string()))?
        {
            platforms.push(
                row.get::<String>(0)
                    .map_err(|error| KeyError::Database(error.to_string()))?,
            );
        }
        Ok(platforms)
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
}
