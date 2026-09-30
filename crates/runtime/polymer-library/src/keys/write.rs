use super::PlatformKeys;
use async_trait::async_trait;
use polymer_consolestorage::keys::{KeyError, KeyIdentity, KeyRecord, KeyWriter};
use std::collections::HashSet;

#[async_trait]
impl KeyWriter for PlatformKeys {
    async fn put(&self, records: &[KeyRecord]) -> Result<(), KeyError> {
        let mut identities = HashSet::new();
        for record in records {
            record.identity.validate()?;
            if record.title_id.as_ref().is_some_and(String::is_empty)
                || !identities.insert(record.identity.clone())
            {
                return Err(KeyError::Invalid("empty or duplicate key identity".into()));
            }
        }
        let mut connection = self
            .store
            .database
            .connect()
            .map_err(|error| KeyError::Database(error.to_string()))?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await
            .map_err(|error| KeyError::Database(error.to_string()))?;
        for record in records {
            transaction
                .execute(
                    "INSERT INTO keys (platform, namespace, id, title_id, byte_length, bytes)
                     VALUES (?, ?, ?, ?, ?, ?)
                     ON CONFLICT(platform, namespace, id) DO UPDATE SET
                         title_id = excluded.title_id,
                         byte_length = excluded.byte_length,
                         bytes = excluded.bytes",
                    turso::params![
                        self.platform.clone(),
                        record.identity.namespace.clone(),
                        record.identity.id.clone(),
                        record.title_id.clone(),
                        record.bytes.len() as i64,
                        record.bytes.clone()
                    ],
                )
                .await
                .map_err(|error| KeyError::Database(error.to_string()))?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| KeyError::Database(error.to_string()))?;
        Ok(())
    }

    async fn remove(&self, identity: &KeyIdentity) -> Result<bool, KeyError> {
        identity.validate()?;
        let connection = self
            .store
            .database
            .connect()
            .map_err(|error| KeyError::Database(error.to_string()))?;
        let count = connection
            .execute(
                "DELETE FROM keys WHERE platform = ? AND namespace = ? AND id = ?",
                turso::params![
                    self.platform.clone(),
                    identity.namespace.clone(),
                    identity.id.clone()
                ],
            )
            .await
            .map_err(|error| KeyError::Database(error.to_string()))?;
        Ok(count != 0)
    }
}
