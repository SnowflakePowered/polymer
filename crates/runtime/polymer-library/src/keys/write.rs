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
        for record in records {
            let digest = blake3::hash(&record.bytes).to_hex().to_string();
            let destination = self.object_path(&digest)?;
            match tokio::fs::metadata(&destination).await {
                Ok(metadata)
                    if metadata.is_file() && metadata.len() == record.bytes.len() as u64 =>
                {
                    continue;
                }
                Ok(_) => {
                    return Err(KeyError::Invalid(
                        "stored key object is not a file of the expected length".into(),
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let directory = destination.parent().expect("key object path has a parent");
            tokio::fs::create_dir_all(directory).await?;
            let directory = directory.to_owned();
            let bytes = record.bytes.clone();
            tokio::task::spawn_blocking(move || -> Result<(), KeyError> {
                use std::io::Write;
                let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
                temporary.write_all(&bytes)?;
                temporary.as_file().sync_all()?;
                match std::fs::hard_link(temporary.path(), destination) {
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                    result => result.map_err(KeyError::from),
                }
            })
            .await
            .map_err(|error| KeyError::Invalid(format!("key copy task failed: {error}")))??;
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
            let digest = blake3::hash(&record.bytes).to_hex().to_string();
            transaction
                .execute(
                    "INSERT INTO keys (platform, namespace, id, title_id, digest, byte_length)
                     VALUES (?, ?, ?, ?, ?, ?)
                     ON CONFLICT(platform, namespace, id) DO UPDATE SET
                         title_id = excluded.title_id,
                         digest = excluded.digest,
                         byte_length = excluded.byte_length",
                    turso::params![
                        self.platform.clone(),
                        record.identity.namespace.clone(),
                        record.identity.id.clone(),
                        record.title_id.clone(),
                        digest,
                        record.bytes.len() as i64
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
