use super::PlatformKeys;
use async_trait::async_trait;
use polymer_consolestorage::keys::{
    KeyDescription, KeyError, KeyIdentity, KeyQuery, KeyReader, KeyRecord,
};
use std::path::PathBuf;

impl PlatformKeys {
    pub async fn file(&self, identity: &KeyIdentity) -> Result<PathBuf, KeyError> {
        identity.validate()?;
        let connection = self
            .store
            .database
            .connect()
            .map_err(|error| KeyError::Database(error.to_string()))?;
        let mut rows = connection
            .query(
                "SELECT digest FROM keys WHERE platform = ? AND namespace = ? AND id = ?",
                turso::params![
                    self.platform.clone(),
                    identity.namespace.clone(),
                    identity.id.clone()
                ],
            )
            .await
            .map_err(|error| KeyError::Database(error.to_string()))?;
        let row = rows
            .next()
            .await
            .map_err(|error| KeyError::Database(error.to_string()))?
            .ok_or_else(|| {
                KeyError::NotFound(format!(
                    "{}/{}/{}",
                    self.platform, identity.namespace, identity.id
                ))
            })?;
        self.object_path(
            &row.get::<String>(0)
                .map_err(|error| KeyError::Database(error.to_string()))?,
        )
    }
}

#[async_trait]
impl KeyReader for PlatformKeys {
    async fn list(&self, query: &KeyQuery) -> Result<Vec<KeyDescription>, KeyError> {
        if query
            .namespace
            .as_deref()
            .is_some_and(|namespace| !matches!(namespace, "system" | "content"))
            || query.title_id.as_ref().is_some_and(String::is_empty)
        {
            return Err(KeyError::Invalid(
                "invalid namespace or empty title filter".into(),
            ));
        }
        let connection = self
            .store
            .database
            .connect()
            .map_err(|error| KeyError::Database(error.to_string()))?;
        let mut rows = connection
            .query(
                "SELECT namespace, id, title_id, byte_length FROM keys
                 WHERE platform = ? AND (? IS NULL OR namespace = ?) AND (? IS NULL OR title_id = ?)
                 ORDER BY namespace, id",
                turso::params![
                    self.platform.clone(),
                    query.namespace.clone(),
                    query.namespace.clone(),
                    query.title_id.clone(),
                    query.title_id.clone()
                ],
            )
            .await
            .map_err(|error| KeyError::Database(error.to_string()))?;
        let mut result = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| KeyError::Database(error.to_string()))?
        {
            result.push(KeyDescription {
                identity: KeyIdentity {
                    namespace: row
                        .get(0)
                        .map_err(|error| KeyError::Database(error.to_string()))?,
                    id: row
                        .get(1)
                        .map_err(|error| KeyError::Database(error.to_string()))?,
                },
                title_id: row
                    .get(2)
                    .map_err(|error| KeyError::Database(error.to_string()))?,
                byte_length: row
                    .get::<i64>(3)
                    .map_err(|error| KeyError::Database(error.to_string()))?
                    as u64,
            });
        }
        Ok(result)
    }

    async fn read(&self, identity: &KeyIdentity) -> Result<KeyRecord, KeyError> {
        identity.validate()?;
        let connection = self
            .store
            .database
            .connect()
            .map_err(|error| KeyError::Database(error.to_string()))?;
        let mut rows = connection
            .query(
                "SELECT title_id, digest FROM keys WHERE platform = ? AND namespace = ? AND id = ?",
                turso::params![
                    self.platform.clone(),
                    identity.namespace.clone(),
                    identity.id.clone()
                ],
            )
            .await
            .map_err(|error| KeyError::Database(error.to_string()))?;
        let row = rows
            .next()
            .await
            .map_err(|error| KeyError::Database(error.to_string()))?
            .ok_or_else(|| {
                KeyError::NotFound(format!(
                    "{}/{}/{}",
                    self.platform, identity.namespace, identity.id
                ))
            })?;
        let title_id = row
            .get(0)
            .map_err(|error| KeyError::Database(error.to_string()))?;
        let digest: String = row
            .get(1)
            .map_err(|error| KeyError::Database(error.to_string()))?;
        let bytes = tokio::fs::read(self.object_path(&digest)?).await?;
        Ok(KeyRecord {
            identity: identity.clone(),
            title_id,
            bytes,
        })
    }
}
