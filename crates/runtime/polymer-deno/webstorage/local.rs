use super::{MAX_STORAGE_BYTES, WebStorageError};
use polymer_library::Directories;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS webstorage (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

PRAGMA user_version = 1;
"#;

#[derive(Clone)]
pub struct LocalStorage {
    connection: turso::Connection,
}

impl LocalStorage {
    pub async fn open(directories: &Directories) -> Result<Self, WebStorageError> {
        let path = directories.webstorage();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(WebStorageError::CreateDirectory)?;
        }
        let path = path.to_str().ok_or(WebStorageError::InvalidPath)?;
        let database = turso::Builder::new_local(path).build().await?;
        let connection = database.connect()?;
        let mut rows = connection.query("PRAGMA user_version", ()).await?;
        let version = rows
            .next()
            .await?
            .ok_or(WebStorageError::StateUnavailable)?
            .get::<i64>(0)?;
        drop(rows);
        match version {
            0 => connection.execute_batch(SCHEMA).await?,
            1 => {}
            version => return Err(WebStorageError::UnsupportedSchema(version)),
        }
        Ok(Self { connection })
    }

    pub async fn length(&self) -> Result<u32, WebStorageError> {
        let mut rows = self
            .connection
            .query("SELECT COUNT(*) FROM webstorage", ())
            .await?;
        let length = rows
            .next()
            .await?
            .ok_or(WebStorageError::StateUnavailable)?
            .get::<i64>(0)?;
        u32::try_from(length).map_err(|_| WebStorageError::StateUnavailable)
    }

    pub async fn key(&self, index: u32) -> Result<Option<String>, WebStorageError> {
        let mut rows = self
            .connection
            .query(
                "SELECT key FROM webstorage ORDER BY rowid LIMIT 1 OFFSET ?1",
                [i64::from(index)],
            )
            .await?;
        rows.next()
            .await?
            .map(|row| row.get::<String>(0).map_err(WebStorageError::from))
            .transpose()
    }

    pub async fn get_item(&self, key: &str) -> Result<Option<String>, WebStorageError> {
        let mut rows = self
            .connection
            .query("SELECT value FROM webstorage WHERE key = ?1", [key])
            .await?;
        rows.next()
            .await?
            .map(|row| row.get::<String>(0).map_err(WebStorageError::from))
            .transpose()
    }

    pub async fn set_item(&self, key: &str, value: &str) -> Result<(), WebStorageError> {
        if key.len() + value.len() >= MAX_STORAGE_BYTES {
            return Err(WebStorageError::StorageExceeded);
        }
        let mut rows = self
            .connection
            .query(
                r#"
                SELECT
                    COALESCE(SUM(length(CAST(key AS BLOB)) + length(CAST(value AS BLOB))), 0)
                    - COALESCE((
                        SELECT length(CAST(key AS BLOB)) + length(CAST(value AS BLOB))
                        FROM webstorage
                        WHERE key = ?1
                    ), 0)
                FROM webstorage
                "#,
                [key],
            )
            .await?;
        let current_size = rows
            .next()
            .await?
            .ok_or(WebStorageError::StateUnavailable)?
            .get::<i64>(0)?;
        drop(rows);
        let new_size = usize::try_from(current_size)
            .ok()
            .and_then(|size| size.checked_add(key.len() + value.len()))
            .ok_or(WebStorageError::StorageExceeded)?;
        if new_size >= MAX_STORAGE_BYTES {
            return Err(WebStorageError::StorageExceeded);
        }
        self.connection
            .execute(
                r#"
                INSERT INTO webstorage (key, value) VALUES (?1, ?2)
                ON CONFLICT (key) DO UPDATE SET value = excluded.value
                "#,
                (key, value),
            )
            .await?;
        Ok(())
    }

    pub async fn remove_item(&self, key: &str) -> Result<(), WebStorageError> {
        self.connection
            .execute("DELETE FROM webstorage WHERE key = ?1", [key])
            .await?;
        Ok(())
    }

    pub async fn clear(&self) -> Result<(), WebStorageError> {
        self.connection
            .execute("DELETE FROM webstorage", ())
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn persists_values_across_connections() {
        let root = tempfile::tempdir().unwrap();
        let directories = Directories::rooted(root.path(), root.path());
        let storage = LocalStorage::open(&directories).await.unwrap();
        storage.set_item("Polymer.theme", "chipotle").await.unwrap();
        drop(storage);

        let storage = LocalStorage::open(&directories).await.unwrap();
        assert_eq!(
            storage.get_item("Polymer.theme").await.unwrap(),
            Some("chipotle".to_owned())
        );
    }
}
